
use crate::cells::TableReader;
use replay_frame::frame::{DecodedFrame, Metrics, READ_TIME_NOT_RECORDED, RecordedFrame, RowChange, SharedView, Stamp, TableUpdate, TableView, Unsayable, cells_rows, cells_view, not_selected, table_field};
use replay_data::{rows::{DeltaRow, RowChanges, TABLE_COUNT, TableId, TableRows, full_mask}, scope::CaptureMode};
use rayon::prelude::*;
use replay_columnar::{lane::ClockHour, manifest::Status, unit::BLOCK};
use std::{io, sync::Arc, time::Instant};

mod campaign;
mod input;
mod reader;
pub use campaign::CampaignBlocks;
pub use input::{NotSaved, Source, is_not_saved};
pub use reader::{Damage, DamageScope, PageCounts, Recording, Run, SharedEntry, TableCoverage, TablePage, strings_of, table_name};
pub use replay_columnar::container::Open;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

pub(crate) const REQUIRED_DERIVED_VERSIONS: &[(TableId, u32, &str)] = &[(
    TableId::SupplyHealth,
    2,
    "SupplyHealth was derived by version 1 (from rounded display supply ratios) and cannot decide off-grid supply thresholds",
)];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedCoverage {
    pub(crate) table: usize,
    pub(crate) from_run: u32,
    pub(crate) from_ordinal: u32,
    pub(crate) reason: String,
}

pub fn encode_derived_from(from_run: u32, from_ordinal: u32) -> io::Result<Vec<u8>> {
    let mut writer = replay_columnar::leaf::Writer::new();
    writer.varint(u64::from(from_run))?;
    writer.varint(u64::from(from_ordinal))?;
    Ok(writer.into_bytes())
}

pub(crate) fn decode_derived_from(bytes: &[u8]) -> io::Result<(u32, u32)> {
    let mut reader = replay_columnar::leaf::Reader::new(bytes);
    let from_run = reader.varint_u32()?;
    let from_ordinal = reader.varint_u32()?;
    reader.finish()?;
    Ok((from_run, from_ordinal))
}

pub(crate) fn derived_withheld(root: &replay_columnar::container::Root, tables: usize, withheld: &mut Vec<(usize, String)>) {
    for &(table, required, reason) in REQUIRED_DERIVED_VERSIONS {
        let index = table.index();
        if index < tables
            && !withheld.iter().any(|(held, _)| *held == index)
            && root
                .section(replay_columnar::container::SectionKey::derived(index as u32))
                .map_or(1, |section| section.version)
                < required
        {
            withheld.push((index, reason.to_owned()));
        }
    }
}

pub fn view_heap_bytes(view: &TableView) -> usize {
    match view {
        TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.heap_bytes(),
        TableView::Unavailable { reason } => reason.capacity(),
        TableView::NotRecorded { .. } => 0,
    }
}

pub fn frame_heap_bytes(frame: &DecodedFrame) -> usize {
    frame.tables.iter().map(|view| view_heap_bytes(view)).sum()
}

pub struct Pool {
    pool: rayon::ThreadPool,
    threads: Joined,
    #[cfg(test)]
    entered: std::sync::atomic::AtomicUsize,
}

impl Drop for Pool {
    fn drop(&mut self) {
        if self.pool.current_thread_index().is_some() {
            self.threads.0.clear();
        }
    }
}

struct Joined(Vec<std::thread::JoinHandle<()>>);

impl Drop for Joined {
    fn drop(&mut self) {
        for thread in self.0.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Pool {
    pub fn new(threads: usize, name: &'static str, start: fn()) -> io::Result<Self> {
        let count = threads.max(1);
        let mut threads = Joined(Vec::new());
        engine_allocation::reserve(&mut threads.0, count)?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(count)
            .thread_name(move |at| format!("{name} {at}"))
            .start_handler(move |_| start())
            .spawn_handler(|thread| {
                let mut builder = std::thread::Builder::new();
                if let Some(name) = thread.name() {
                    builder = builder.name(name.to_owned());
                }
                if let Some(size) = thread.stack_size() {
                    builder = builder.stack_size(size);
                }
                threads.0.push(builder.spawn(|| thread.run())?);
                Ok(())
            })
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            pool,
            threads,
            #[cfg(test)]
            entered: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub fn install<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        self.pool.install(|| {
            #[cfg(test)]
            {
                assert!(self.pool.current_thread_index().is_some());
                self.entered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            work()
        })
    }

    #[cfg(test)]
    pub(crate) fn entered(&self) -> usize {
        self.entered.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn on<R: Send>(pool: Option<&Self>, work: impl FnOnce() -> R + Send) -> R {
        match pool {
            Some(pool) => pool.install(work),
            None => work(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(replay_spool::Spooled)]
pub struct Segment {
    pub branch: u64,
    pub run: u32,
    pub first: u32,
    pub last: u32,
}

struct Slot {
    id: TableId,
    reader: Option<TableReader>,
    lost: Option<String>,
    view: Option<SharedView>,
    update: Option<Arc<TableUpdate>>,
    spent: Spent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    pub patched: u64,
    pub in_place: u64,
    pub rebuilt: u64,
    pub snapshots_patched: u64,
    pub move_ns: u64,
    pub view_ns: u64,
    pub update_ns: u64,
}

pub struct Frames<R: std::ops::Deref<Target = Recording>> {
    recording: R,
    slots: Vec<Slot>,
    run: Option<u32>,
    ordinal: u32,
    first_hour: i32,
    clock: Option<(u32, Vec<ClockHour>)>,
    mode: CaptureMode,
    format: u16,
    anew: bool,
    facts: bool,
    built: Option<(u32, u32)>,
    stepped: bool,
    patching: bool,
    timed: bool,
}

impl<R: std::ops::Deref<Target = Recording>> Frames<R> {
    pub fn new(recording: R, tables: &[TableId]) -> io::Result<Self> {
        let mut chosen = [false; TABLE_COUNT];
        for table in tables {
            chosen[table.index()] = true;
        }
        let slots = TableId::ALL.into_iter().filter(|table| chosen[table.index()]).map(|id| Slot { id, reader: None, lost: None, view: None, update: None, spent: Spent::default() }).collect();
        Ok(Self {
            slots,
            run: None,
            ordinal: 0,
            first_hour: 0,
            clock: None,
            mode: CaptureMode::from_byte(recording.root_identity().capture_mode)?,
            format: recording.format(),
            anew: true,
            facts: false,
            built: None,
            stepped: false,
            patching: true,
            timed: false,
            recording,
        })
    }

    pub fn rebuilding(mut self) -> Self {
        self.patching = false;
        self
    }

    pub fn timed(mut self) -> Self {
        self.timed = true;
        self
    }

    pub fn spent(&self) -> impl Iterator<Item = (TableId, Spent)> + '_ {
        self.slots.iter().map(|slot| (slot.id, slot.spent))
    }

    pub fn with_change_facts(mut self) -> Self {
        self.facts = true;
        self
    }

    pub fn selected(&self) -> impl Iterator<Item = TableId> + '_ {
        self.slots.iter().map(|slot| slot.id)
    }

    pub fn at(&self) -> Option<(u32, i32)> {
        self.run.map(|run| (run, self.first_hour + self.ordinal as i32))
    }

    pub fn reader(&self, table: TableId) -> Option<&TableReader> {
        self.slots.iter().find(|slot| slot.id == table)?.reader.as_ref()
    }

    pub fn seek(&mut self, run: u32, ordinal: u32) -> io::Result<()> {
        self.seek_with(run, ordinal, TableReader::go_to)
    }

    pub fn frame_with_parked_readers(&mut self, branch: u64, run: u32, ordinal: u32, budget: usize) -> io::Result<(DecodedFrame, Option<usize>)> {
        self.seek_with(run, ordinal, TableReader::resume_frame)?;
        let frame = self.frame(branch)?;
        Ok((frame, self.park_readers(budget)))
    }

    fn park_readers(&mut self, budget: usize) -> Option<usize> {
        let fixed = self.slots.capacity() * size_of::<Slot>()
            + self.clock.as_ref().map_or(0, |(_, hours)| hours.capacity() * size_of::<ClockHour>());
        let mut remaining = budget.checked_sub(fixed)?;
        let mut candidates = engine_allocation::vec(self.slots.len()).ok()?;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            slot.view = None;
            slot.update = None;
            slot.lost = None;
            if let Some(bytes) = slot.reader.as_mut().and_then(TableReader::park_frame) {
                candidates.push((bytes, index));
            } else {
                slot.reader = None;
            }
        }
        candidates.sort_unstable();
        for (bytes, index) in candidates {
            if let Some(left) = remaining.checked_sub(bytes) {
                remaining = left;
            } else {
                self.slots[index].reader = None;
            }
        }
        Some(budget - remaining)
    }

    fn seek_with(&mut self, run: u32, ordinal: u32, move_reader: fn(&mut TableReader, &Recording, u32) -> io::Result<()>) -> io::Result<()> {
        let recording: &Recording = &self.recording;
        let held = recording.run(run)?;
        if ordinal >= held.ordinals {
            return Err(invalid(format!("Run {run} holds no ordinal {ordinal}")));
        }
        let new_run = self.run != Some(run);
        for slot in self.slots.iter_mut().filter(|slot| new_run || slot.reader.is_none()) {
            slot.lost = None;
            slot.reader = match TableReader::new(recording, run, slot.id, None) {
                Ok(reader) => Some(reader),
                Err(error) => {
                    slot.lost = Some(lost(error)?);
                    None
                }
            };
        }
        if new_run {
            self.clock = None;
        }
        self.run = Some(run);
        self.first_hour = held.first_hour;
        self.ordinal = ordinal;
        self.anew = true;
        self.stepped = false;
        let timed = self.timed;
        self.slots.par_iter_mut().try_for_each(|slot| move_slot(slot, timed, |reader| move_reader(reader, recording, ordinal)))
    }

    pub fn step(&mut self) -> io::Result<bool> {
        let recording: &Recording = &self.recording;
        let Some(run) = self.run else { return Err(invalid("Frames step before their first seek")) };
        if self.ordinal + 1 >= recording.run(run)?.ordinals {
            return Ok(false);
        }
        self.stepped = self.built == Some((run, self.ordinal));
        self.ordinal += 1;
        let ordinal = self.ordinal;
        let timed = self.timed;
        self.slots.par_iter_mut().try_for_each(|slot| {
            if slot.reader.is_none()
                && recording.predates(slot.id.index()).is_none()
                && matches!(recording.table_coverage(slot.id.index(), run), TableCoverage::WithheldBefore { ordinal: valid, .. } if ordinal == valid)
            {
                slot.lost = None;
                slot.view = None;
                slot.update = None;
                slot.reader = match TableReader::new(recording, run, slot.id, None) {
                    Ok(reader) => Some(reader),
                    Err(error) => {
                        slot.lost = Some(lost(error)?);
                        None
                    }
                };
                move_slot(slot, timed, |reader| reader.go_to(recording, ordinal))
            } else {
                move_slot(slot, timed, |reader| reader.step(recording).map(drop))
            }
        })?;
        Ok(true)
    }

    pub fn go_to(&mut self, run: u32, ordinal: u32) -> io::Result<()> {
        if self.run == Some(run) && ordinal == self.ordinal + 1 {
            self.step().map(drop)
        } else if self.run == Some(run) && ordinal == self.ordinal {
            Ok(())
        } else {
            self.seek(run, ordinal)
        }
    }

    pub fn keyframe(&mut self) {
        self.anew = true;
    }

    pub fn metrics(&mut self) -> io::Result<Metrics> {
        let Some(run) = self.run else { return Ok(Metrics::default()) };
        let block = self.ordinal / BLOCK;
        if self.clock.as_ref().is_none_or(|(held, _)| *held != block) {
            self.clock = Some((block, self.recording.clock(run, block)?));
        }
        let hour = self.clock.as_ref().and_then(|(_, clock)| clock.get((self.ordinal % BLOCK) as usize)).copied().unwrap_or_default();
        let mut table_ns = [0; TABLE_COUNT];
        for slot in &self.slots {
            if slot.reader.as_ref().is_some_and(TableReader::was_read) {
                table_ns[slot.id.index()] = READ_TIME_NOT_RECORDED;
            }
        }
        Ok(Metrics { total_ns: hour.total_ns, table_ns, pool_bytes: hour.pool_bytes, dropped_frames: hour.dropped_frames })
    }

    pub fn frame(&mut self, branch: u64) -> io::Result<DecodedFrame> {
        self.build_frame(branch, false)
    }

    pub fn frame_recorded(&mut self, branch: u64) -> io::Result<(DecodedFrame, RecordedFrame)> {
        let keyframe = self.anew;
        let frame = self.build_frame(branch, true)?;
        let mut slots = [None; TABLE_COUNT];
        let mut updates = Vec::with_capacity(self.slots.len());
        for (at, slot) in self.slots.iter().enumerate() {
            slots[slot.id.index()] = Some(at);
            updates.push(slot.update.clone().ok_or_else(|| invalid("A table's update was not built"))?);
        }
        let recorded = RecordedFrame::from_updates(branch, frame.hour, keyframe, frame.metrics.clone(), Arc::new(slots), updates, Arc::new(TableUpdate::Unavailable { reason: String::new() }))?;
        Ok((frame, recorded))
    }

    fn build_frame(&mut self, branch: u64, updates: bool) -> io::Result<DecodedFrame> {
        let hour = self.first_hour + self.ordinal as i32;
        let build = Build {
            hour,
            mode: self.mode,
            format: self.format,
            anew: self.anew,
            stepped: self.patching && self.stepped,
            facts: self.facts.then_some(self.stepped && !self.anew),
            updates,
            timed: self.timed,
        };
        self.built = None;
        self.stepped = false;
        self.slots.par_iter_mut().try_for_each(|slot| build_slot(slot, &build))?;
        self.anew = false;
        self.built = self.run.map(|run| (run, self.ordinal));
        let mut tables: [SharedView; TABLE_COUNT] = std::array::from_fn(|_| not_selected());
        for slot in &self.slots {
            if let Some(view) = &slot.view {
                tables[slot.id.index()] = view.clone();
            }
        }
        Ok(DecodedFrame { branch, hour, tables, metrics: self.metrics()? })
    }

    pub fn held_bytes(&self) -> usize {
        self.slots.iter().filter_map(|slot| slot.reader.as_ref()).map(TableReader::held_bytes).sum()
    }
}

fn lost(error: io::Error) -> io::Result<String> {
    if error.kind() != io::ErrorKind::InvalidData {
        return Err(error);
    }
    Ok(error.to_string())
}

fn move_slot(slot: &mut Slot, timed: bool, walk: impl FnOnce(&mut TableReader) -> io::Result<()>) -> io::Result<()> {
    let Some(reader) = slot.reader.as_mut() else { return Ok(()) };
    let started = timed.then(Instant::now);
    let walked = walk(reader);
    lap(&mut slot.spent.move_ns, started);
    if let Err(error) = walked {
        slot.lost = Some(lost(error)?);
        slot.reader = None;
        slot.view = None;
    }
    Ok(())
}

fn lap(ns: &mut u64, started: Option<Instant>) {
    if let Some(started) = started {
        *ns += started.elapsed().as_nanos() as u64;
    }
}

struct Build {
    hour: i32,
    mode: CaptureMode,
    format: u16,
    anew: bool,
    stepped: bool,
    facts: Option<bool>,
    updates: bool,
    timed: bool,
}

fn build_slot(slot: &mut Slot, build: &Build) -> io::Result<()> {
    let Slot { id, reader, lost, view, update, spent } = slot;
    let id = *id;
    let Some(reader) = reader.as_ref() else {
        unavailable(view, update, lost.clone().unwrap_or_default(), build.updates);
        return Ok(());
    };
    let (hour, format) = (build.hour, build.format);
    let changes = reader.changes();
    if !build.anew && view.is_some() && changes.quiet() {
        *update = build.updates.then(|| Arc::new(TableUpdate::Unchanged { baseline: hour - 1 }));
        return Ok(());
    }
    let started = build.timed.then(Instant::now);
    let previous = view.as_ref().map(SharedView::held);
    let complete_before = matches!(previous, Some(TableView::Complete(_)));
    let withheld = !matches!(previous, None | Some(TableView::Complete(_) | TableView::Truncated { .. }));
    let snapshot = build.anew || changes.anew || view.is_none() || changes.status_changed || withheld;
    let patchable = build.stepped && !changes.anew && complete_before && reader.status() == Status::Complete;
    let stamped = matches!(build.facts, Some(true)) && !changes.anew && complete_before;
    let edits = (build.updates || patchable || stamped).then(|| hour_edits(reader));
    let stamp = match (build.facts, view.as_ref()) {
        (Some(true), Some(previous)) if !changes.anew && complete_before => Some(edited(id, edits.as_ref().expect("edits built for stamp"), previous, hour, format)?),
        (Some(_), _) => Some(Stamp::new(hour)),
        (None, _) => None,
    };
    let mut patched = None;
    if patchable {
        let edits = edits.as_ref().expect("edits built for patch");
        let held = view.take().ok_or_else(|| invalid("A patched table holds no view"))?;
        let deleted = present(held.held(), &edits.deleted);
        let kept_deleted = build.updates.then(|| deleted.clone());
        if let Ok((rows, in_place)) = patch(id, reader, held, edits, deleted, format) {
            spent.patched += 1;
            spent.in_place += u64::from(in_place);
            patched = Some((TableView::Complete(rows), kept_deleted));
        }
    }
    let (made, deleted) = match patched {
        Some((made, deleted)) => (made, deleted),
        None => {
            spent.rebuilt += 1;
            match cells_view(id, &reader.view(), reader.status(), reader.read_rows(), build.mode, format) {
                Ok(made) => (made, None),
                Err(error) if Unsayable::of(&error).is_some() => {
                    unavailable(view, update, error.to_string(), build.updates);
                    lap(&mut spent.view_ns, started);
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
    };
    let made_update = if build.updates {
        lap(&mut spent.view_ns, started);
        let started = build.timed.then(Instant::now);
        let before = update.take();
        let edits = edits.as_ref().expect("edits built for updates");
        let made_update = match (&made, snapshot) {
            (TableView::Complete(rows), true) => {
                let (rows, reused) = snapshot_rows(rows, before, edits, deleted)?;
                spent.snapshots_patched += u64::from(reused);
                TableUpdate::Snapshot(rows)
            }
            (TableView::Complete(_), false) => delta(id, edits, hour - 1, format),
            (TableView::Truncated { rows, missing }, _) => TableUpdate::Truncated { rows: rows.try_clone_validated()?, missing: *missing },
            (TableView::Unavailable { reason }, _) => TableUpdate::Unavailable { reason: reason.clone() },
            (TableView::NotRecorded { mode }, _) => TableUpdate::NotRecorded { mode: *mode },
        };
        Some((Arc::new(made_update), started))
    } else {
        None
    };
    let complete = matches!(made, TableView::Complete(_));
    let mut made = SharedView::from(made);
    if complete {
        made = made.with_sampled_at(reader.sampled_at());
        if let Some(stamp) = stamp {
            made = made.with_stamp(stamp);
        }
    }
    *view = Some(made);
    if let Some((made_update, update_started)) = made_update {
        *update = Some(made_update);
        lap(&mut spent.update_ns, update_started);
    } else {
        *update = None;
        lap(&mut spent.view_ns, started);
    }
    Ok(())
}

fn unavailable(view: &mut Option<SharedView>, update: &mut Option<Arc<TableUpdate>>, reason: String, updates: bool) {
    let existing = matches!(view.as_ref().map(SharedView::held), Some(TableView::Unavailable { reason: held }) if *held == reason);
    if updates {
        if !existing {
            *view = Some(TableView::Unavailable { reason: reason.clone() }.into());
        }
        *update = Some(Arc::new(TableUpdate::Unavailable { reason }));
    } else {
        if !existing {
            *view = Some(TableView::Unavailable { reason }.into());
        }
        *update = None;
    }
}

fn present(view: &TableView, deleted: &[u128]) -> Vec<u128> {
    let TableView::Complete(rows) = view else { return Vec::new() };
    deleted.iter().copied().filter(|&key| rows.contains_key(key)).collect()
}

fn patch(id: TableId, reader: &TableReader, held: SharedView, edits: &Edits, deleted: Vec<u128>, format: u16) -> io::Result<(TableRows, bool)> {
    if edits.written.iter().any(|&(_, _, fields)| fields == 0) {
        return Err(invalid("A written column has no field bit"));
    }
    let (mut rows, in_place) = match held.try_unwrap() {
        Ok(TableView::Complete(rows)) => (rows, true),
        Ok(_) => return Err(invalid("A patched view is not complete")),
        Err(shared) => match shared.held() {
            TableView::Complete(rows) => (rows.try_clone_validated()?, false),
            _ => return Err(invalid("A patched view is not complete")),
        },
    };
    let cells = reader.view();
    if !edits.written.is_empty() {
        let patches = engine_allocation::collect_vec(edits.written.iter().map(|&(key, _, fields)| (key, fields)))?;
        rows.patch_from_wire(&patches, &mut |at, column| {
            table_field(id, &cells, edits.written[at].1, column, format)
        })?;
    }
    let inserted = cells_rows(id, &cells, &edits.inserted, format)?;
    let rows = rows.apply_changes(RowChanges::new(inserted, deleted))?;
    Ok((rows, in_place))
}

fn snapshot_rows(rows: &TableRows, before: Option<Arc<TableUpdate>>, edits: &Edits, deleted: Option<Vec<u128>>) -> io::Result<(TableRows, bool)> {
    if let (Some(deleted), Some(before)) = (deleted, before)
        && let Ok(TableUpdate::Snapshot(held)) = Arc::try_unwrap(before)
    {
        let keys = engine_allocation::collect_vec(edits.rows().map(|(key, _)| key))?;
        return Ok((held.apply_changes(rows.rows_at(&keys, deleted)?)?, true));
    }
    Ok((rows.try_clone_validated()?, false))
}

fn edited(id: TableId, edits: &Edits, previous: &SharedView, hour: i32, format: u16) -> io::Result<Stamp> {
    let TableView::Complete(rows) = previous.held() else { return Err(invalid("A table's change facts follow a view that is not complete")) };
    let recorded = id.last_change().fields(id, format);
    let unrecorded = if recorded >= u64::BITS as usize { 0 } else { id.full_mask() & (u64::MAX << recorded) };
    let changed = edits.masks(recorded).map(|(key, mask)| RowChange { key, fields: mask | unrecorded, inserted: !rows.contains_key(key) }).collect();
    let deleted = present(previous.held(), &edits.deleted);
    Ok(previous.edited(changed, deleted, hour))
}

fn delta(id: TableId, edits: &Edits, baseline: i32, format: u16) -> TableUpdate {
    let fields = id.last_change().fields(id, format);
    let changed = edits.masks(fields).map(|(key, mask)| DeltaRow { format, key, mask, recorded_fields: fields, values: Vec::new() }).collect();
    TableUpdate::Delta { baseline, changed, deleted: edits.deleted.clone() }
}

struct Edits {
    inserted: Vec<(u128, u32)>,
    written: Vec<(u128, u32, u64)>,
    deleted: Vec<u128>,
}

impl Edits {
    fn rows(&self) -> impl Iterator<Item = (u128, bool)> + '_ {
        let mut inserted = self.inserted.iter().map(|&(key, _)| key).peekable();
        let mut written = self.written.iter().map(|&(key, _, _)| key).peekable();
        std::iter::from_fn(move || match (inserted.peek(), written.peek()) {
            (Some(&new), Some(&old)) if new <= old => inserted.next().map(|key| (key, true)),
            (Some(_), Some(_)) | (None, Some(_)) => written.next().map(|key| (key, false)),
            (Some(_), None) => inserted.next().map(|key| (key, true)),
            (None, None) => None,
        })
    }

    fn masks(&self, fields: usize) -> impl Iterator<Item = (u128, u64)> + '_ {
        let mut written = self.written.iter();
        self.rows().map(move |(key, inserted)| {
            if inserted {
                return (key, full_mask(fields));
            }
            let mask = written.next().map_or(0, |&(_, _, mask)| mask);
            (key, mask)
        })
    }
}

fn hour_edits(reader: &TableReader) -> Edits {
    let changes = reader.changes();
    let cells = reader.cells();
    let mut inserted: Vec<(u128, u32)> = changes.inserted.iter().filter_map(|&slot| Some((cells.key(slot)?, slot))).collect();
    inserted.sort_unstable();
    let mut written: Vec<(u128, u32, u64)> = Vec::new();
    for (at, slots) in changes.written.iter().enumerate() {
        let column = reader.columns()[at];
        let bit = if column < 64 { 1_u64 << column } else { 0 };
        for &slot in slots {
            if changes.inserted.binary_search(&slot).is_ok() {
                continue;
            }
            if let Some(key) = cells.key(slot) {
                written.push((key, slot, bit));
            }
        }
    }
    written.sort_unstable_by_key(|&(key, _, _)| key);
    written.dedup_by(|later, kept| {
        let same = later.0 == kept.0;
        if same {
            kept.2 |= later.2;
        }
        same
    });
    let mut deleted: Vec<u128> = changes.deleted.iter().map(|&(_, key)| key).filter(|key| inserted.binary_search_by_key(key, |&(held, _)| held).is_err()).collect();
    deleted.sort_unstable();
    deleted.dedup();
    Edits { inserted, written, deleted }
}

pub fn available(status: Status) -> bool {
    matches!(status, Status::Complete | Status::Truncated { .. })
}

pub fn complete(status: Status) -> bool {
    status == Status::Complete
}
