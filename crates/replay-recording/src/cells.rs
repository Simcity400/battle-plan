
use crate::columnar_frames::{Damage, DamageScope, Recording, SharedEntry};
use replay_data::rows::TableId;
use replay_columnar::{
    manifest::Status,
    state::Kind,
    store::{Strings, TableState, TableView, Value},
    unit::{
        self, BLOCK, lists,
        reader::{Advanced, Cursor, Event, Lives, LogCursor, LogEvent, Part},
    },
};
#[cfg(any(test, feature = "test-support"))]
use std::cell::{Cell, RefCell};
use std::{collections::BTreeMap, io};

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableReadStats {
    pub opened: u64,
    pub restores: u64,
    pub stretches: u64,
    pub backward_seeks: u64,
    pub visited_ordinals: u64,
    pub pinned: u64,
    pub columns: u64,
}

#[cfg(any(test, feature = "test-support"))]
pub type ReadCounts = BTreeMap<TableId, TableReadStats>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: RefCell<Option<ReadCounts>> = const { RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
fn note_read(table: TableId, update: impl FnOnce(&mut TableReadStats)) {
    if !COUNTING.get() {
        return;
    }
    COUNTS.with(|held| {
        if let Some(counts) = held.borrow_mut().as_mut() {
            update(counts.entry(table).or_default());
        }
    });
}

pub struct ReadCarry {
    #[cfg(any(test, feature = "test-support"))]
    counting: bool,
}

pub struct ReadSample {
    #[cfg(any(test, feature = "test-support"))]
    counts: Option<ReadCounts>,
}

pub fn capture_reads_context() -> ReadCarry {
    ReadCarry {
        #[cfg(any(test, feature = "test-support"))]
        counting: COUNTING.get(),
    }
}

pub fn run_with_reads<T>(_carry: &ReadCarry, work: impl FnOnce() -> T) -> (T, ReadSample) {
    #[cfg(any(test, feature = "test-support"))]
    if _carry.counting {
        let (value, counts) = count_reads(work);
        return (value, ReadSample { counts: Some(counts) });
    }
    (
        work(),
        ReadSample {
            #[cfg(any(test, feature = "test-support"))]
            counts: None,
        },
    )
}

pub fn merge_read_sample(_sample: ReadSample) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(counts) = _sample.counts {
        if !COUNTING.get() {
            return;
        }
        COUNTS.with(|held| {
            if let Some(target) = held.borrow_mut().as_mut() {
                for (table, stats) in counts {
                    let entry = target.entry(table).or_default();
                    entry.opened += stats.opened;
                    entry.restores += stats.restores;
                    entry.stretches += stats.stretches;
                    entry.backward_seeks += stats.backward_seeks;
                    entry.visited_ordinals += stats.visited_ordinals;
                    entry.pinned += stats.pinned;
                    entry.columns += stats.columns;
                }
            }
        });
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn count_reads<T>(work: impl FnOnce() -> T) -> (T, ReadCounts) {
    struct Guard {
        previous_flag: bool,
        previous: Option<ReadCounts>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            COUNTING.set(self.previous_flag);
            COUNTS.with(|held| *held.borrow_mut() = self.previous.take());
        }
    }
    let previous_flag = COUNTING.replace(true);
    let previous = COUNTS.with(|held| held.borrow_mut().replace(BTreeMap::new()));
    let mut guard = Guard { previous_flag, previous };
    let value = work();
    let counts = COUNTS.with(|held| held.borrow_mut().take()).unwrap_or_default();
    COUNTING.set(guard.previous_flag);
    COUNTS.with(|held| *held.borrow_mut() = guard.previous.take());
    std::mem::forget(guard);
    (value, counts)
}

fn label(name: &str) -> engine_allocation::labels::Scope {
    engine_allocation::labels::enter(engine_allocation::labels::owner(name), engine_allocation::labels::Kind::WalkerTables)
}

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

#[derive(Debug, Default)]
pub struct Changes {
    pub anew: bool,
    pub inserted: Vec<u32>,
    pub deleted: Vec<(u32, u128)>,
    pub written: Vec<Vec<u32>>,
    pub status_changed: bool,
    pub coverage_changed: bool,
    pub witnessed: Vec<u32>,
}

impl Changes {
    pub fn quiet(&self) -> bool {
        !self.anew && !self.status_changed && self.inserted.is_empty() && self.deleted.is_empty() && self.written.iter().all(Vec::is_empty) && self.witnessed.is_empty()
    }

    fn clear(&mut self) {
        self.anew = false;
        self.inserted.clear();
        self.deleted.clear();
        self.written.iter_mut().for_each(Vec::clear);
        self.status_changed = false;
        self.coverage_changed = false;
        self.witnessed.clear();
    }

    pub fn rows_moved(&self) -> bool {
        self.anew || self.coverage_changed || !self.witnessed.is_empty() || !self.inserted.is_empty() || !self.deleted.is_empty() || self.written.iter().any(|slots| !slots.is_empty())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Difference(i64),
    Value(u64),
    Missing,
    Unrecorded,
    Written,
}

#[derive(Clone, Copy, Debug)]
pub struct TrackEvent {
    pub column: usize,
    pub slot: u32,
    pub born: Option<u16>,
    pub hour: u16,
    pub change: Change,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    Bits(u64),
    Missing,
    Unrecorded,
    List,
}

impl Held {
    pub fn of(view: &TableView<'_>, column: usize, slot: u32) -> Self {
        match view.value(column, slot) {
            Value::Number(bits) => Self::Bits(bits),
            Value::Text(id) => Self::Bits(u64::from(id)),
            Value::Missing => Self::Missing,
            Value::Unrecorded => Self::Unrecorded,
            Value::List(_) => Self::List,
        }
    }
}

#[derive(Debug, Default)]
pub struct Stretch {
    pub block: u32,
    pub block_first: i32,
    pub from: u16,
    pub to: u16,
    pub start: Vec<(u32, u128, Vec<Held>)>,
    pub events: Vec<TrackEvent>,
    pub inserted: Vec<(u16, u32, u128)>,
    pub deleted: Vec<(u16, u32, u128)>,
    pub statuses: Vec<Status>,
    pub read: Vec<(u16, u32)>,
}

#[derive(Debug, Default)]
struct Tapped {
    events: Vec<TrackEvent>,
    read: Vec<(u16, u32)>,
}

impl Tapped {
    fn clear(&mut self) {
        self.events.clear();
        self.read.clear();
    }
}

pub fn read_columns(recording: &Recording, recorded: usize, table: usize, columns: Option<&[usize]>) -> Vec<usize> {
    let mut columns = match columns {
        Some(columns) => {
            let mut columns: Vec<usize> = columns.iter().copied().filter(|&column| column < recorded).collect();
            columns.sort_unstable();
            columns.dedup();
            columns
        }
        None => recording.read_columns(table),
    };
    if let Some(&id) = TableId::ALL.get(table) {
        let format = recording.format();
        columns.retain(|&column| !id.column_withheld_at(column, format));
    }
    columns
}

pub struct TableReader {
    table: usize,
    kinds: Vec<Kind>,
    held: usize,
    columns: Vec<usize>,
    run: u32,
    first_hour: i32,
    ordinals: u32,
    cells: TableState,
    strings: Strings,
    status: Status,
    read: bool,
    sampled: Option<i32>,
    view: Vec<u32>,
    witnessed_before: Vec<u32>,
    ordinal: Option<u32>,
    block: u32,
    entry: Option<SharedEntry>,
    next_part: usize,
    cursor: Cursor,
    changes: Changes,
    lives: Lives,
    marks: (Vec<(u16, u32)>, Vec<(u16, u32)>),
    keyed: bool,
    tracking: bool,
    pending: Tapped,
    restores: u32,
    log: Option<Box<LogRead>>,
    absent: bool,
    withheld_before: Option<(u32, String)>,
}

struct LogRead {
    cursor: LogCursor,
    from: u16,
    slots: BTreeMap<u64, u32>,
}

impl TableReader {
    pub fn new(recording: &Recording, run: u32, table: TableId, columns: Option<&[usize]>) -> io::Result<Self> {
        #[cfg(any(test, feature = "test-support"))]
        note_read(table, |stats| stats.opened += 1);
        let held = recording.run(run)?;
        let index = table.index();
        let absent = recording.predates(index).is_some();
        let withheld_before = if absent {
            None
        } else {
            match recording.table_coverage(index, run) {
                crate::columnar_frames::TableCoverage::Valid => None,
                crate::columnar_frames::TableCoverage::WithheldAll(reason) => {
                    return Err(Damage::wrap(DamageScope::Withheld { table: index as u32 }, invalid(reason.to_owned())));
                }
                crate::columnar_frames::TableCoverage::WithheldBefore { ordinal, reason } => Some((ordinal, reason.to_owned())),
            }
        };
        let kinds = crate::schema::schema()?.swap_remove(index);
        let recorded = recording.columns()[index].min(kinds.len());
        let columns = read_columns(recording, recorded, index, columns);
        #[cfg(any(test, feature = "test-support"))]
        note_read(table, |stats| stats.columns += columns.len() as u64);
        let log = !absent && held.ordinals > 0 && unit::reader::class_of(&recording.table_pages(run, index, 0, 1)?, index)?;
        Ok(Self {
            table: index,
            log: log.then(|| Box::new(LogRead { cursor: LogCursor::new(&kinds[..recorded], &columns, 0), from: 0, slots: BTreeMap::new() })),
            cursor: Cursor::new(&kinds[..recorded], &columns, 0),
            cells: TableState::new(&kinds[..recorded]),
            changes: Changes { written: vec![Vec::new(); columns.len()], ..Changes::default() },
            kinds,
            held: recorded,
            columns,
            run,
            first_hour: held.first_hour,
            ordinals: held.ordinals,
            strings: Strings::default(),
            status: Status::NotRecorded,
            read: false,
            sampled: None,
            view: Vec::new(),
            witnessed_before: Vec::new(),
            ordinal: None,
            block: 0,
            entry: None,
            next_part: 0,
            lives: Lives::default(),
            marks: (Vec::new(), Vec::new()),
            keyed: false,
            tracking: false,
            pending: Tapped::default(),
            restores: 0,
            absent,
            withheld_before,
        })
    }

    pub fn index_keys(&mut self) {
        self.keyed = true;
        self.cells.index_keys();
    }

    pub fn keep_tracks(&mut self) {
        self.tracking = true;
    }

    pub fn packs_on_step(&self) -> bool {
        self.log.is_none() && self.entry.as_ref().is_some_and(|entry| entry.unit.packed) && self.ordinal.is_some_and(|ordinal| (ordinal + 1) % BLOCK == 0)
    }

    pub fn slot(&self, key: u128) -> Option<u32> {
        self.cells.find(key)
    }

    pub fn into_parts(self) -> (TableState, Strings, Vec<Kind>) {
        (self.cells, self.strings, self.kinds)
    }

    pub fn table(&self) -> TableId {
        TableId::ALL[self.table]
    }

    pub fn run(&self) -> u32 {
        self.run
    }

    pub fn columns(&self) -> &[usize] {
        &self.columns
    }

    pub fn kinds(&self) -> &[Kind] {
        &self.kinds
    }

    pub fn hour(&self) -> Option<i32> {
        self.ordinal.map(|ordinal| self.first_hour + ordinal as i32)
    }

    pub fn cells(&self) -> &TableState {
        &self.cells
    }

    pub fn view(&self) -> TableView<'_> {
        TableView::new(&self.cells, &self.kinds, &self.strings)
    }

    pub fn strings(&self) -> &Strings {
        &self.strings
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn was_read(&self) -> bool {
        self.read
    }

    pub fn sampled_at(&self) -> Option<i32> {
        self.sampled
    }

    pub fn read_rows(&self) -> Option<&[u32]> {
        matches!(self.status, Status::Truncated { .. }).then_some(self.view.as_slice())
    }

    pub fn available(&self) -> bool {
        matches!(self.status, Status::Complete | Status::Truncated { .. })
    }

    pub fn changes(&self) -> &Changes {
        &self.changes
    }

    pub fn reason(&self) -> Option<&str> {
        match self.status {
            Status::Unavailable { reason } => Some(self.strings.get(reason).and_then(|text| std::str::from_utf8(text).ok()).unwrap_or("A table's reason is not text")),
            _ => None,
        }
    }

    pub fn restores(&self) -> u32 {
        self.restores
    }

    pub fn held_bytes(&self) -> usize {
        self.cells.bytes().total()
    }

    pub fn park_exact(&mut self) -> Option<usize> {
        if self.absent { return None; }
        self.park_frame().map(|heap| heap + size_of::<Self>())
    }

    pub(crate) fn resume_frame(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        if self.entry.is_some() || self.absent || self.ordinal.is_none() {
            return self.go_to(recording, ordinal);
        }
        let resumed = recording.table_entry(self.run, self.block, self.table).and_then(|entry| {
            self.entry = Some(entry);
            self.go_to(recording, ordinal)
        });
        if resumed.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::InvalidData) {
            *self = Self::new(recording, self.run, self.table(), None)?;
            return self.go_to(recording, ordinal);
        }
        resumed
    }

    pub(crate) fn park_frame(&mut self) -> Option<usize> {
        if self.log.is_some() || self.tracking || self.withheld_before.is_some()
            || matches!(self.status, Status::Unavailable { .. })
        {
            return None;
        }
        self.entry = None;
        Some(self.held_bytes() + self.cells.column_capacity_bytes() + self.strings.reader_bytes() + self.cursor.retained_bytes()
            + self.kinds.capacity() * size_of::<Kind>() + self.columns.capacity() * size_of::<usize>()
            + (self.view.capacity() + self.witnessed_before.capacity() + self.changes.inserted.capacity()
                + self.changes.witnessed.capacity() + self.changes.written.iter().map(Vec::capacity).sum::<usize>()) * size_of::<u32>()
            + self.changes.written.capacity() * size_of::<Vec<u32>>()
            + self.changes.deleted.capacity() * size_of::<(u32, u128)>()
            + (self.lives.inserted.capacity() + self.lives.deleted.capacity()) * size_of::<(u16, u32, u128)>()
            + (self.marks.0.capacity() + self.marks.1.capacity()) * size_of::<(u16, u32)>())
    }

    pub fn verify_restore(&self, recording: &Recording, block: u32) -> io::Result<bool> {
        if !self.ordinal.is_some_and(|at| at / BLOCK == block && at % BLOCK == BLOCK - 1)
            || self.status != Status::Complete
        {
            return Ok(false);
        }
        let entry = recording.table_entry(self.run, block, self.table)?;
        let Some(restore) = entry.unit.restore.as_ref().filter(|_| !entry.unit.packed) else { return Ok(false) };
        let (restored, _, tables) = unit::reader::restore_with_string_tables(
            recording.extents(), restore, &self.kinds[..self.held], &self.columns, self.keyed)?;
        if self.cells.slots() != restored.slots()
            || !(0..restored.slots()).all(|slot| {
                let key = restored.key(slot);
                self.cells.key(slot) == key && (key.is_none() || self.columns.iter().all(|&column| {
                    self.cells.value(column, slot) == restored.value(column, slot)
                }))
            })
        {
            return Ok(false);
        }
        let strings = Strings::restored(tables);
        Ok(self.strings.iter().all(|(id, _)| self.strings.get(id) == strings.get(id))
            && strings.iter().all(|(id, _)| self.strings.get(id) == strings.get(id)))
    }

    pub fn walk_exact(&mut self, recording: &Recording, to: u32, verified: impl Fn(u32) -> bool, mut on_change: impl FnMut(&Changes, &TableState)) -> io::Result<Option<bool>> {
        let Some(from) = self.ordinal else { return Ok(None) };
        if self.log.is_some() || self.tracking || self.absent || self.withheld_before.is_some()
            || !(from..self.ordinals).contains(&to) || self.status != Status::Complete
        {
            return Ok(None);
        }
        for block in self.block..to / BLOCK {
            if recording.table_entry(self.run, block, self.table)?.unit.restore.is_some() && !verified(block) {
                return Ok(None);
            }
        }
        if to > from && self.entry.is_none() {
            self.entry = Some(recording.table_entry(self.run, self.block, self.table)?);
        }
        let mut changed = false;
        for _ in from..to {
            if !self.step(recording)? || self.status != Status::Complete {
                return Ok(None);
            }
            if self.changes.anew || !self.changes.inserted.is_empty() || !self.changes.deleted.is_empty()
                || self.changes.written.iter().any(|slots| !slots.is_empty())
            {
                changed = true;
                on_change(&self.changes, &self.cells);
            }
        }
        Ok(Some(changed))
    }

    fn damaged(&self, block: u32, error: io::Error) -> io::Error {
        Damage::wrap(DamageScope::Table { run: self.run, block, table: self.table as u32 }, error)
    }

    fn check_ordinal(&self, ordinal: u32) -> io::Result<()> {
        if let Some((from, reason)) = &self.withheld_before
            && ordinal < *from
        {
            return Err(Damage::wrap(DamageScope::Withheld { table: self.table as u32 }, invalid(reason.clone())));
        }
        Ok(())
    }

    pub fn seek(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        self.check_ordinal(ordinal)?;
        if ordinal >= self.ordinals {
            return Err(invalid(format!("Run {} holds no ordinal {ordinal}", self.run)));
        }
        if self.absent {
            self.stand_absent(ordinal);
            self.changes.anew = true;
            self.changes.status_changed = true;
            self.changes.coverage_changed = true;
            return Ok(());
        }
        if self.log.is_some() {
            return self.seek_log(recording, ordinal);
        }
        let (block, hour) = (ordinal / BLOCK, (ordinal % BLOCK) as u16);
        let (restored, entry, first) = self.restore_before(recording, block)?;
        let restore = if first {
            entry.unit.first.as_ref().ok_or_else(|| self.damaged(0, invalid("The run's first block holds no first restore")))?
        } else {
            entry.unit.restore.as_ref().ok_or_else(|| self.damaged(restored, invalid("The block holds no restore")))?
        };
        let source = recording.extents();
        let restoring = label("reader: restored cells");
        let (cells, _, tables) = unit::reader::restore_with_string_tables(source, restore, &self.kinds[..self.held], &self.columns, self.keyed).map_err(|error| self.damaged(restored, error))?;
        drop(entry);
        self.cells = cells;
        self.restores += 1;
        self.strings = Strings::restored(tables);
        #[cfg(any(test, feature = "test-support"))]
        note_read(self.table(), |stats| {
            stats.restores += 1;
            stats.pinned += self.strings.copied() as u64;
        });
        drop(restoring);
        let (mut at, start) = if first { (restored, 1) } else { (restored + 1, 0) };
        while at < block {
            self.open_block(recording, at, start_of(at, first, restored, start))?;
            let last = self.block_hours(at) - 1;
            self.advance_to(recording, last, BLOCK as u16)?;
            if self.entry.as_ref().is_some_and(|entry| entry.unit.packed) {
                self.cells.pack()?;
                if self.keyed {
                    self.cells.try_index_keys()?;
                }
            }
            at += 1;
        }
        let begin = start_of(block, first, restored, start);
        self.open_block(recording, block, begin)?;
        if hour >= begin {
            if self.tracking {
                let mut pending = std::mem::take(&mut self.pending);
                let end = self.block_hours(block) - 1;
                self.advance_by(recording, hour, BLOCK as u16, Some((hour + 1, end, &mut pending)))?;
                self.pending = pending;
            } else {
                self.advance_to(recording, hour, BLOCK as u16)?;
            }
        }
        self.ordinal = Some(ordinal);
        self.settle(hour)?;
        self.changes.clear();
        self.changes.anew = true;
        self.changes.status_changed = true;
        self.changes.coverage_changed = true;
        Ok(())
    }

    pub fn step(&mut self, recording: &Recording) -> io::Result<bool> {
        let Some(ordinal) = self.ordinal else { return Err(invalid("A table reader steps before its first seek")) };
        if ordinal + 1 >= self.ordinals {
            return Ok(false);
        }
        let next = ordinal + 1;
        self.check_ordinal(next)?;
        let (block, hour) = (next / BLOCK, (next % BLOCK) as u16);
        if self.absent {
            self.stand_absent(next);
            return Ok(true);
        }
        self.changes.clear();
        if self.log.is_some() {
            if block != self.block {
                self.open_block(recording, block, 0)?;
            }
            let before = (self.status, self.read, self.sampled);
            self.advance_log(recording, hour)?;
            self.ordinal = Some(next);
            self.settle_stepped(hour, before)?;
            return Ok(true);
        }
        if block != self.block {
            if self.entry.as_ref().is_some_and(|entry| entry.unit.packed) {
                self.cells.pack()?;
                if self.keyed {
                    self.cells.try_index_keys()?;
                }
                self.changes.anew = true;
            }
            self.open_block(recording, block, 0)?;
        }
        let before = (self.status, self.read, self.sampled);
        self.advance_to(recording, hour, 1)?;
        for &(_, slot, key) in &self.lives.deleted {
            self.changes.deleted.push((slot, key));
        }
        for &(_, slot, _) in &self.lives.inserted {
            self.changes.inserted.push(slot);
        }
        self.changes.inserted.sort_unstable();
        self.changes.inserted.dedup();
        self.changes.inserted.retain(|&slot| self.cells.live().get(slot));
        self.ordinal = Some(next);
        self.settle_stepped(hour, before)?;
        Ok(true)
    }

    fn settle_stepped(&mut self, hour: u16, before: (Status, bool, Option<i32>)) -> io::Result<()> {
        std::mem::swap(&mut self.view, &mut self.witnessed_before);
        self.settle(hour)?;
        self.changes.status_changed = before != (self.status, self.read, self.sampled);
        self.changes.coverage_changed = (before.0, before.1) != (self.status, self.read);
        if matches!(before.0, Status::Truncated { .. }) && matches!(self.status, Status::Truncated { .. }) {
            let (old, new) = (&self.witnessed_before, &self.view);
            let (mut at, mut to) = (0, 0);
            while at < old.len() || to < new.len() {
                match (old.get(at), new.get(to)) {
                    (Some(a), Some(b)) if a == b => {
                        at += 1;
                        to += 1;
                    }
                    (Some(&a), Some(&b)) if a < b => {
                        self.changes.witnessed.push(a);
                        at += 1;
                    }
                    (Some(_), Some(&b)) | (None, Some(&b)) => {
                        self.changes.witnessed.push(b);
                        to += 1;
                    }
                    (Some(&a), None) => {
                        self.changes.witnessed.push(a);
                        at += 1;
                    }
                    (None, None) => break,
                }
            }
        }
        Ok(())
    }

    pub fn go_to(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        self.check_ordinal(ordinal)?;
        #[cfg(any(test, feature = "test-support"))]
        note_read(self.table(), |stats| {
            stats.visited_ordinals += 1;
            if self.ordinal.is_some_and(|at| ordinal < at) {
                stats.backward_seeks += 1;
            }
        });
        match self.ordinal {
            Some(at) if at == ordinal => {
                self.changes.clear();
                Ok(())
            }
            Some(at) if at < ordinal && ordinal < self.ordinals && self.log.is_some() => self.advance_log_to(recording, ordinal),
            Some(at) if at < ordinal && ordinal < self.ordinals && !self.absent && !self.restored_between(recording, at, ordinal / BLOCK)? => self.advance(recording, ordinal),
            _ => self.seek(recording, ordinal),
        }
    }

    fn restored_between(&self, recording: &Recording, at: u32, block: u32) -> io::Result<bool> {
        for restored in (at / BLOCK..block).rev() {
            if recording.table_entry(self.run, restored, self.table)?.unit.restore.is_some() {
                return Ok((restored + 1) * BLOCK - 1 > at);
            }
        }
        Ok(false)
    }

    fn advance(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        let (block, hour) = (ordinal / BLOCK, (ordinal % BLOCK) as u16);
        while self.block < block {
            let last = self.block_hours(self.block) - 1;
            self.advance_to(recording, last, BLOCK as u16)?;
            if self.entry.as_ref().is_some_and(|entry| entry.unit.packed) {
                self.cells.pack()?;
                if self.keyed {
                    self.cells.try_index_keys()?;
                }
            }
            self.open_block(recording, self.block + 1, 0)?;
        }
        self.changes.clear();
        if self.tracking {
            let mut pending = std::mem::take(&mut self.pending);
            let end = self.block_hours(block) - 1;
            self.advance_by(recording, hour, BLOCK as u16, Some((hour + 1, end, &mut pending)))?;
            self.pending = pending;
        } else {
            self.advance_to(recording, hour, BLOCK as u16)?;
        }
        self.ordinal = Some(ordinal);
        self.settle(hour)?;
        self.changes.anew = true;
        self.changes.status_changed = true;
        self.changes.coverage_changed = true;
        Ok(())
    }

    pub fn read_stretch(&mut self, recording: &Recording, to: u32) -> io::Result<Stretch> {
        #[cfg(any(test, feature = "test-support"))]
        note_read(self.table(), |stats| stats.stretches += 1);
        if self.log.is_some() {
            return Err(invalid(format!("{:?} is a log: its rows never change, so it has no stretch of track events; read its rows at an hour", self.table())));
        }
        let Some(ordinal) = self.ordinal else { return Err(invalid("A table reader reads a stretch before its first seek")) };
        let next = ordinal + 1;
        self.check_ordinal(next)?;
        if to < next || to >= self.ordinals || to / BLOCK != next / BLOCK {
            return Err(invalid(format!("A stretch to ordinal {to} after {ordinal} crosses a block or the run")));
        }
        let (block, from, last) = (next / BLOCK, (next % BLOCK) as u16, (to % BLOCK) as u16);
        if self.absent {
            self.stand_absent(to);
            self.changes.anew = true;
            self.changes.status_changed = true;
            self.changes.coverage_changed = true;
            let statuses = vec![Status::NotRecorded; usize::from(last - from) + 1];
            return Ok(Stretch { block, block_first: self.first_hour + (block * BLOCK) as i32, from, to: last, statuses, ..Stretch::default() });
        }
        if from == 0 {
            if self.entry.as_ref().is_some_and(|entry| entry.unit.packed) {
                self.cells.pack()?;
                if self.keyed {
                    self.cells.try_index_keys()?;
                }
            }
            self.open_block(recording, block, 0)?;
        }
        if !self.tracking {
            return Err(invalid("A stretch is read by a reader keeping tracks"));
        }
        let view = self.view();
        let start = view
            .rows()
            .filter_map(|slot| Some((slot, view.key(slot)?, self.columns.iter().map(|&column| Held::of(&view, column, slot)).collect())))
            .collect();
        let mut tapped = std::mem::take(&mut self.pending);
        tapped.events.retain(|event| from <= event.hour && event.hour <= last);
        tapped.read.retain(|&(hour, _)| from <= hour && hour <= last);
        self.changes.clear();
        self.advance_by(recording, last, BLOCK as u16, Some((from, last, &mut tapped)))?;
        let block_first = self.first_hour + (block * BLOCK) as i32;
        let mut statuses = Vec::with_capacity(usize::from(last - from) + 1);
        let entry = &self.entry.as_ref().expect("opened block").unit;
        for hour in from..=last {
            statuses.push(unit::reader::status(entry, hour, block_first).map_err(|error| self.damaged(block, error))?.0);
        }
        let Tapped { events, mut read } = tapped;
        if !statuses.iter().any(|status| matches!(status, Status::Truncated { .. })) {
            read.clear();
        }
        let stretch = Stretch {
            block,
            block_first,
            from,
            to: last,
            start,
            events,
            inserted: self.lives.inserted.clone(),
            deleted: self.lives.deleted.clone(),
            statuses,
            read,
        };
        self.ordinal = Some(to);
        self.settle(last)?;
        self.changes.anew = true;
        self.changes.status_changed = true;
        self.changes.coverage_changed = true;
        Ok(stretch)
    }

    fn restore_before(&self, recording: &Recording, block: u32) -> io::Result<(u32, SharedEntry, bool)> {
        let mut at = block;
        while at > 0 {
            at -= 1;
            let entry = recording.table_entry(self.run, at, self.table)?;
            if entry.unit.restore.is_some() {
                return Ok((at, entry, false));
            }
            if at == 0 {
                if entry.unit.first.is_none() {
                    return Err(self.damaged(0, invalid("The run's first block holds no first restore")));
                }
                return Ok((0, entry, true));
            }
        }
        let entry = recording.table_entry(self.run, 0, self.table)?;
        if entry.unit.first.is_none() {
            return Err(self.damaged(0, invalid("The run's first block holds no first restore")));
        }
        Ok((0, entry, true))
    }

    fn stand_absent(&mut self, ordinal: u32) {
        self.changes.clear();
        self.ordinal = Some(ordinal);
        self.block = ordinal / BLOCK;
    }

    fn block_hours(&self, block: u32) -> u16 {
        (self.ordinals - block * BLOCK).min(BLOCK) as u16
    }

    fn open_block(&mut self, recording: &Recording, block: u32, start: u16) -> io::Result<()> {
        self.entry = Some(recording.table_entry(self.run, block, self.table)?);
        self.block = block;
        self.next_part = 0;
        self.pending.clear();
        self.cursor = Cursor::new(&self.kinds[..self.held], &self.columns, start);
        if let Some(log) = &mut self.log {
            log.cursor = LogCursor::new(&self.kinds[..self.held], &self.columns, start);
            log.from = start;
        }
        Ok(())
    }

    fn advance_to(&mut self, recording: &Recording, hour: u16, step: u16) -> io::Result<()> {
        self.advance_by(recording, hour, step, None)
    }

    fn advance_by(&mut self, recording: &Recording, hour: u16, step: u16, mut tapping: Option<(u16, u16, &mut Tapped)>) -> io::Result<()> {
        let hours = self.block_hours(self.block);
        let _cells = label("reader: cells");
        self.lives.deleted.clear();
        self.lives.inserted.clear();
        loop {
            let advanced = self.cursor.advance(hour, step, &mut self.cells, &mut self.changes.written, &mut self.lives).map_err(|error| self.damaged(self.block, error))?;
            match advanced {
                Advanced::Reached => return Ok(()),
                Advanced::Missing { .. } => {
                    let entry = &self.entry.as_ref().expect("opened block").unit;
                    let Some(part) = entry.parts.get(self.next_part) else { return Ok(()) };
                    let last = entry.parts.get(self.next_part + 1).map_or(hours - 1, |next| next.first - 1);
                    let _parts = label("reader: fetched parts");
                    let fetched = unit::reader::fetch(recording.extents(), part, last, &self.columns, self.cursor.prefix().as_ref()).map_err(|error| self.damaged(self.block, error))?;
                    for (id, text) in fetched.strings() {
                        self.strings.pin(*id, text)?;
                    }
                    if let Some((from, to, tapped)) = tapping.as_mut() {
                        tap(&fetched, &self.kinds, &self.columns, *from, *to, tapped)?;
                    }
                    self.cursor.push(fetched);
                    self.next_part += 1;
                }
            }
        }
    }

    fn settle(&mut self, hour: u16) -> io::Result<()> {
        let block_first = self.first_hour + (self.block * BLOCK) as i32;
        let entry = &self.entry.as_ref().expect("opened block").unit;
        let (status, read, sampled, _) = unit::reader::status(entry, hour, block_first).map_err(|error| self.damaged(self.block, error))?;
        self.status = status;
        self.read = read;
        self.sampled = sampled;
        self.view.clear();
        let truncated = matches!(status, Status::Truncated { .. });
        if self.log.is_some() {
            if truncated {
                self.view.extend(self.cells.live().ones());
            }
            return Ok(());
        }
        if truncated {
            if self.ordinal == Some(0) {
                self.view.extend(self.cells.live().ones());
                return Ok(());
            }
            let (touched, read) = &mut self.marks;
            touched.clear();
            read.clear();
            self.cursor.marks(hour, hour, touched, read)?;
            let live = self.cells.live();
            self.view.extend(read.iter().map(|&(_, slot)| slot).filter(|&slot| live.get(slot)));
            self.view.extend(self.lives.inserted.iter().filter(|&&(at, _, _)| at == hour).map(|&(_, slot, _)| slot).filter(|&slot| live.get(slot)));
            self.view.sort_unstable();
            self.view.dedup();
        }
        Ok(())
    }
}

impl TableReader {
    fn seek_log(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        let (block, hour) = (ordinal / BLOCK, (ordinal % BLOCK) as u16);
        let (restored, entry, first) = self.restore_before(recording, block)?;
        let restore = if first {
            entry.unit.first.as_ref().ok_or_else(|| self.damaged(0, invalid("The run's first block holds no first restore")))?
        } else {
            entry.unit.restore.as_ref().ok_or_else(|| self.damaged(restored, invalid("The block holds no restore")))?
        };
        let source = recording.extents();
        let restoring = label("reader: restored cells");
        let (state, _, tables) = unit::reader::restore_with_string_tables(source, restore, &self.kinds[..self.held], &[], false).map_err(|error| self.damaged(restored, error))?;
        drop(entry);
        self.restores += 1;
        let listed: Vec<u64> = state.listed().ok_or_else(|| self.damaged(restored, invalid("A log's restore holds no live set")))?.ones().collect();
        self.cells = TableState::new(&self.kinds[..self.held]);
        if self.keyed {
            self.cells.try_index_keys()?;
        }
        self.strings = Strings::restored(tables);
        if !listed.is_empty() {
            let pages = recording.table_pages(self.run, self.table, 0, restored + 1)?;
            let log = self.log.as_deref_mut().expect("a log's reader");
            log.slots.clear();
            let (cells, strings, columns, slots) = (&mut self.cells, &mut self.strings, &self.columns, &mut log.slots);
            let mut failed = None;
            let read = unit::reader::log_rows_at(source, &pages, &self.kinds[..self.held], self.table, columns, &listed, &mut |event| {
                let put = match event {
                    LogEvent::Named { id, text } => strings.pin(id, text),
                    LogEvent::Appended { ordinal, key, rows, at, .. } => cells.put_row_from(key, rows, at, columns).map(|slot| {
                        slots.insert(ordinal, slot);
                    }),
                    LogEvent::Forgotten { .. } => Ok(()),
                };
                if let Err(error) = put {
                    if failed.is_none() {
                        failed = Some(error);
                    }
                }
            });
            let held = log.slots.len();
            read.map_err(|error| self.damaged(restored, error))?;
            if let Some(error) = failed {
                return Err(error);
            }
            if held != listed.len() {
                return Err(self.damaged(restored, invalid("A log's listed rows are not all in its parts")));
            }
        } else if let Some(log) = self.log.as_deref_mut() {
            log.slots.clear();
        }
        drop(restoring);
        let (mut at, start) = if first { (restored, 1) } else { (restored + 1, 0) };
        while at < block {
            self.open_block(recording, at, start_of(at, first, restored, start))?;
            self.advance_log(recording, self.block_hours(at) - 1)?;
            at += 1;
        }
        let begin = start_of(block, first, restored, start);
        self.open_block(recording, block, begin)?;
        if hour >= begin {
            self.advance_log(recording, hour)?;
        }
        self.ordinal = Some(ordinal);
        self.settle(hour)?;
        self.changes.clear();
        self.changes.anew = true;
        self.changes.status_changed = true;
        self.changes.coverage_changed = true;
        Ok(())
    }

    fn advance_log_to(&mut self, recording: &Recording, ordinal: u32) -> io::Result<()> {
        let (block, hour) = (ordinal / BLOCK, (ordinal % BLOCK) as u16);
        self.changes.clear();
        while self.block < block {
            self.advance_log(recording, self.block_hours(self.block) - 1)?;
            self.open_block(recording, self.block + 1, 0)?;
        }
        self.advance_log(recording, hour)?;
        self.ordinal = Some(ordinal);
        self.settle(hour)?;
        self.changes.anew = true;
        self.changes.status_changed = true;
        self.changes.coverage_changed = true;
        Ok(())
    }

    fn advance_log(&mut self, recording: &Recording, hour: u16) -> io::Result<()> {
        let hours = self.block_hours(self.block);
        let (run, block, table) = (self.run, self.block, self.table as u32);
        let damaged = |error| Damage::wrap(DamageScope::Table { run, block, table }, error);
        let _cells = label("reader: cells");
        let log = self.log.as_deref_mut().expect("a log's reader");
        loop {
            let (cells, columns, changes, slots, from) = (&mut self.cells, &self.columns, &mut self.changes, &mut log.slots, log.from);
            let mut failed = None;
            let advanced = log
                .cursor
                .advance(hour, &mut |event| {
                    if let Err(error) = listed(event, from, cells, columns, slots, changes) {
                        if failed.is_none() {
                            failed = Some(error);
                        }
                    }
                })
                .map_err(damaged)?;
            if let Some(error) = failed {
                return Err(error);
            }
            match advanced {
                Advanced::Reached => break,
                Advanced::Missing { .. } => {
                    let entry = &self.entry.as_ref().expect("opened block").unit;
                    let Some(part) = entry.parts.get(self.next_part) else { break };
                    let last = entry.parts.get(self.next_part + 1).map_or(hours - 1, |next| next.first - 1);
                    let _parts = label("reader: fetched parts");
                    let fetched = unit::reader::fetch(recording.extents(), part, last, &self.columns, log.cursor.prefix().as_ref()).map_err(damaged)?;
                    for (id, text) in fetched.strings() {
                        self.strings.pin(*id, text)?;
                    }
                    log.cursor.push(fetched);
                    self.next_part += 1;
                }
            }
        }
        self.changes.inserted.sort_unstable();
        Ok(())
    }
}

fn listed(event: LogEvent<'_>, from: u16, cells: &mut TableState, columns: &[usize], slots: &mut BTreeMap<u64, u32>, changes: &mut Changes) -> io::Result<()> {
    match event {
        LogEvent::Appended { hour, .. } | LogEvent::Forgotten { hour, .. } if hour < from => {}
        LogEvent::Appended { ordinal, key, rows, at, .. } => {
            if slots.contains_key(&ordinal) {
                return Err(invalid(format!("A log appends row {ordinal}, which it lists already")));
            }
            let slot = cells.put_row_from(key, rows, at, columns)?;
            slots.insert(ordinal, slot);
            changes.inserted.push(slot);
        }
        LogEvent::Forgotten { from, to, .. } => {
            let gone: Vec<(u64, u32)> = slots.range(from..to).map(|(&ordinal, &slot)| (ordinal, slot)).collect();
            for (ordinal, slot) in gone {
                slots.remove(&ordinal);
                let key = cells.remove_row(slot)?;
                match changes.inserted.iter().position(|&held| held == slot) {
                    Some(at) => {
                        changes.inserted.swap_remove(at);
                    }
                    None => changes.deleted.push((slot, key)),
                }
            }
        }
        LogEvent::Named { .. } => {}
    }
    Ok(())
}

fn tap(part: &Part, kinds: &[Kind], columns: &[usize], from: u16, to: u16, tapped: &mut Tapped) -> io::Result<()> {
    if part.last < from || part.first > to {
        return Ok(());
    }
    for (at, &column) in columns.iter().enumerate() {
        part.tracks(at, list_column(kinds[column]), |slot, born, held| {
            for &(hour, event) in held {
                if hour < from || hour > to {
                    continue;
                }
                let change = match event {
                    Event::Difference(change) => Change::Difference(change),
                    Event::Value(value) => Change::Value(value),
                    Event::Missing => Change::Missing,
                    Event::Unrecorded | Event::List(lists::GONE, _) => Change::Unrecorded,
                    Event::List(..) => Change::Written,
                };
                tapped.events.push(TrackEvent { column: at, slot, born, hour, change });
            }
        })?;
    }
    part.reads(from, to, &mut tapped.read)
}

pub fn list_column(kind: Kind) -> bool {
    matches!(kind, Kind::U32List | Kind::U64List | Kind::Opaque)
}

pub fn numeric(kind: Kind) -> bool {
    !matches!(kind, Kind::Bytes | Kind::U32List | Kind::U64List | Kind::Opaque)
}

fn start_of(block: u32, first: bool, restored: u32, start: u16) -> u16 {
    if first && block == restored { start } else { 0 }
}
