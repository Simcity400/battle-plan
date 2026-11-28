use dashboard_text::{metadata::check_cancel, triggers::inputs::{Owner, Resolution, TableRead}};
use replay::campaign::{Replay, SeekResult};
use replay_columnar::manifest::Status;
use replay_frame::frame::DecodedFrame;
use replay_data::rows::TableId;
use replay_lanes::lanes::{Cell, Keys, StatusRun, Step};
use std::{collections::{BTreeMap, VecDeque}, io, sync::{Arc, Mutex, atomic::AtomicBool}};
use crate::{Source, products::scoped_text::{Lexical, ScriptedWorld, read_with_lexical}};

const MAX_CACHED_TABLE_DATES: usize = 256;

mod variable;
use variable::VariableDates;
mod prepared;
pub use prepared::DateSelection;
use prepared::{DateWindows, PreparedDates};

thread_local! {
    static READ_DATES: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

pub(super) fn without_read_dates<T>(read: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) { READ_DATES.set(self.0); }
    }
    let _restore = Restore(READ_DATES.replace(false));
    read()
}

pub(crate) fn reading_dates() -> bool {
    READ_DATES.get()
}

pub fn row_change_hours(steps: &[Step], end_column: Option<usize>, last: i32) -> impl Iterator<Item = i32> + '_ {
    steps.iter().enumerate().flat_map(move |(index, step)| {
        let end = end_column.and_then(|column| {
            let Cell::Number(value) = step.cells.as_ref()?.get(column)? else { return None };
            let end = i32::try_from(*value).ok()?;
            (step.hour < end && end <= steps.get(index + 1).map_or(last, |next| next.hour)).then_some(end)
        });
        std::iter::once(step.hour).chain(end)
    })
}

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Selection {
    fields: u64,
    rows: Option<Vec<(u128, u128)>>,
    ends: bool,
}

impl Selection {
    fn merge_unowned(&mut self, read: &TableRead) {
        self.fields |= read.fields;
        self.ends |= read.ends;
        match (read.rows, self.rows.as_mut()) {
            (Some(rows), Some(ranges)) => ranges.push(rows),
            _ => self.rows = None,
        }
    }

    fn merge_owned(&mut self, read: &TableRead, owner: Owner, id_ranges: Option<&[(u32, u32)]>) {
        self.fields |= read.fields;
        self.ends |= read.ends;
        let (Some(id_ranges), Some(ranges)) = (id_ranges, self.rows.as_mut()) else {
            self.rows = None;
            return;
        };
        if !push_owner_ranges(ranges, owner, read.rows, id_ranges) {
            self.rows = None;
        }
    }

    fn normalize(&mut self) {
        if let Some(ranges) = &mut self.rows {
            ranges.sort_unstable();
            ranges.dedup_by(|next, previous| {
                if next.0 > previous.1.saturating_add(1) { return false; }
                previous.1 = previous.1.max(next.1);
                true
            });
        }
    }

    fn dates(&self, replay: &Replay, run: u32, table: TableId, first: i32, last: i32) -> io::Result<Vec<i32>> {
        let columns: Vec<_> = (0..64).filter(|bit| self.fields & (1_u64 << bit) != 0).collect();
        let keys = self.rows.as_deref().map_or(Keys::All, Keys::Ranges);
        let history = replay_lanes::lanes::history(replay.recording(), run, table, &columns, keys, first, last)?;
        let mut dates: Vec<_> = history.statuses.iter().map(|status| status.first).collect();
        let end = self.ends.then(|| history.columns.iter().position(|&c| table.field_name(c) == Some("end_hour"))).flatten();
        for (_, steps) in history.keys {
            dates.extend(row_change_hours(&steps, end, last));
        }
        dates.sort_unstable();
        dates.dedup();
        Ok(dates)
    }
}

fn push_owner_ranges(
    ranges: &mut Vec<(u128, u128)>,
    owner: Owner,
    rows: Option<(u128, u128)>,
    id_ranges: &[(u32, u32)],
) -> bool {
    let Some(hi_shift) = owner.shift.checked_add(32).filter(|&s| s <= 128) else {
        return false;
    };
    let low_mask = if owner.shift == 0 { 0 } else { (1_u128 << owner.shift) - 1 };
    match rows {
        Some((lo, hi)) => {
            let prefix = if hi_shift < 128 {
                if (lo >> hi_shift) != (hi >> hi_shift) {
                    return false;
                }
                (lo >> hi_shift) << hi_shift
            } else {
                0
            };
            for &(id_lo, id_hi) in id_ranges {
                let r_lo = (prefix | (u128::from(id_lo) << owner.shift)).max(lo);
                let r_hi = (prefix | (u128::from(id_hi) << owner.shift) | low_mask).min(hi);
                if r_lo <= r_hi {
                    ranges.push((r_lo, r_hi));
                }
            }
            true
        }
        None => {
            for &(id_lo, id_hi) in id_ranges {
                let r_lo = u128::from(id_lo) << owner.shift;
                let r_hi = (u128::from(id_hi) << owner.shift) | low_mask;
                ranges.push((r_lo, r_hi));
            }
            true
        }
    }
}

struct OwnerHistory {
    field: u64,
    dates: Vec<i32>,
    statuses: Vec<StatusRun>,
    keys: Vec<(u32, Vec<(i32, Option<u32>)>)>,
}

impl OwnerHistory {
    fn load(replay: &Replay, run: u32, table: TableId, first: i32, last: i32) -> io::Result<Self> {
        let column = dashboard_declare::field(table, "canonical_slot")?;
        let history = replay_lanes::lanes::history(replay.recording(), run, table, &[column], Keys::All, first, last)?;
        let mut dates: Vec<_> = history.statuses.iter().map(|status| status.first).collect();
        let mut keys = Vec::with_capacity(history.keys.len());
        for (key, steps) in history.keys {
            dates.extend(row_change_hours(&steps, None, last));
            let Ok(id) = u32::try_from(key) else { continue };
            let slots = steps
                .into_iter()
                .map(|step| {
                    let slot = step.cells.as_deref().and_then(|cells| match cells.first() {
                        Some(Cell::Number(value)) => u32::try_from(*value).ok(),
                        _ => None,
                    });
                    (step.hour, slot)
                })
                .collect();
            keys.push((id, slots));
        }
        dates.sort_unstable();
        dates.dedup();
        Ok(Self { field: 1_u64 << column, dates, statuses: history.statuses, keys })
    }

    fn watched_id_ranges(&self, slot: u32, hour: i32) -> Option<Vec<(u32, u32)>> {
        let status = self.statuses.get(self.statuses.partition_point(|s| s.first <= hour).checked_sub(1)?)?;
        if status.status != Status::Complete {
            return None;
        }
        let mut ranges = Vec::new();
        let mut start = 0_u32;
        let mut exhausted = false;
        for (id, steps) in &self.keys {
            let Some(at) = steps.partition_point(|(step_hour, _)| *step_hour <= hour).checked_sub(1) else {
                continue;
            };
            let Some(linked_slot) = steps[at].1 else {
                continue;
            };
            if linked_slot == slot {
                continue;
            }
            if *id > start {
                ranges.push((start, *id - 1));
            }
            match id.checked_add(1) {
                Some(next) => start = next,
                None => {
                    exhausted = true;
                    break;
                }
            }
        }
        if !exhausted {
            ranges.push((start, u32::MAX));
        }
        Some(ranges)
    }
}

#[derive(Default)]
pub struct ReadDates {
    span: Option<(u32, i32, i32)>,
    tables: BTreeMap<(TableId, Selection), Vec<i32>>,
    order: VecDeque<(TableId, Selection)>,
    links: Option<OwnerHistory>,
    countries: Option<OwnerHistory>,
    variables: Option<Arc<Mutex<VariableDates>>>,
    prepared: Option<(DateWindows, &'static [DateSelection], Arc<Mutex<PreparedDates>>)>,
}

#[derive(Default)]
struct ScriptedReadDates(BTreeMap<&'static [DateSelection], Arc<Mutex<ReadDates>>>);

impl ReadDates {
    pub fn prepare_scripted(source: &Source, run: u32) -> io::Result<Option<Arc<crate::products::read_ahead::Preparation>>> {
        VariableDates::prepare(&source.state::<VariableDates>(), source.replay(), run)
    }

    pub fn prepare_selected(
        source: &Source, run: u32, span: (i32, i32), specs: &'static [DateSelection],
        extra: impl FnOnce(&Replay, u32) + Send + 'static,
    ) -> io::Result<Option<Arc<crate::products::read_ahead::Preparation>>> {
        let replay = source.replay();
        let recorded = replay.recording().run(run)?;
        if span == (recorded.first_hour, recorded.last_hour()) {
            PreparedDates::prepare(source, run, DateWindows::Run, specs, extra)
        } else {
            Self::prepare_scripted(source, run)
        }
    }

    pub fn prepare_block_dates(source: &Source, run: u32, specs: &'static [DateSelection]) -> io::Result<Option<Arc<crate::products::read_ahead::Preparation>>> {
        PreparedDates::prepare(source, run, DateWindows::Blocks, specs, |_, _| {})
    }

    pub fn for_selected_source(source: &Source, specs: &'static [DateSelection]) -> Arc<Mutex<Self>> {
        let kept = source.state::<ScriptedReadDates>();
        Arc::clone(kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.entry(specs).or_insert_with(|| Arc::new(Mutex::new(Self {
            variables: Some(source.state::<VariableDates>()),
            prepared: Some((DateWindows::Run, specs, source.state::<PreparedDates>())), ..Self::default()
        }))))
    }

    fn insert_dates(&mut self, table: TableId, selection: Selection, dates: Vec<i32>) {
        let key = (table, selection);
        if self.tables.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|held| held == &key) {
                self.order.remove(pos);
            }
        } else if self.order.len() >= MAX_CACHED_TABLE_DATES
            && let Some(oldest) = self.order.pop_front()
        {
            self.tables.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.tables.insert(key, dates);
    }

    fn owner_history(&mut self, replay: &Replay, run: u32, by: Resolution, first: i32, last: i32) -> io::Result<&OwnerHistory> {
        let (table, already) = match by {
            Resolution::Links => (TableId::CountryLinks, self.links.is_some()),
            Resolution::Countries => (TableId::Countries, self.countries.is_some()),
        };
        if !already {
            let loaded = OwnerHistory::load(replay, run, table, first, last)?;
            let selection = Selection { fields: loaded.field, rows: None, ends: false };
            self.insert_dates(table, selection, loaded.dates.clone());
            match by {
                Resolution::Links => self.links = Some(loaded),
                Resolution::Countries => self.countries = Some(loaded),
            }
        }
        Ok(match by {
            Resolution::Links => self.links.as_ref(),
            Resolution::Countries => self.countries.as_ref(),
        }
        .expect("owner history loaded"))
    }

    pub fn through(&mut self, replay: &Replay, run: u32, reads: &[TableRead], first: i32, hour: i32, last: i32, cancelled: &AtomicBool) -> io::Result<i32> {
        if hour >= last {
            check_cancel(cancelled)?;
            return Ok(last);
        }
        if self.span != Some((run, first, last)) {
            self.tables.clear();
            self.order.clear();
            self.links = None;
            self.countries = None;
            self.span = Some((run, first, last));
        }
        let mut through = last;
        let mut needs_links = false;
        let mut needs_countries = false;
        for read in reads {
            if let Some(owner) = read.owner {
                match owner.by {
                    Resolution::Links => needs_links = true,
                    Resolution::Countries => needs_countries = true,
                }
            }
        }
        for (needed, by) in [(needs_links, Resolution::Links), (needs_countries, Resolution::Countries)] {
            if !needed {
                continue;
            }
            check_cancel(cancelled)?;
            let Ok(history) = self.owner_history(replay, run, by, first, last) else { return Ok(hour) };
            if let Some(&next) = history.dates.get(history.dates.partition_point(|&at| at <= hour)) {
                through = through.min(next - 1);
                if through == hour {
                    return Ok(hour);
                }
            }
        }
        let mut tables = BTreeMap::<TableId, Selection>::new();
        for read in reads {
            let selection = tables
                .entry(read.table)
                .or_insert_with(|| Selection { rows: Some(Vec::new()), ..Default::default() });
            match read.owner {
                None => selection.merge_unowned(read),
                Some(owner) => {
                    let history = match owner.by {
                        Resolution::Links => self.links.as_ref(),
                        Resolution::Countries => self.countries.as_ref(),
                    };
                    let id_ranges = history.and_then(|history| history.watched_id_ranges(owner.slot, hour));
                    selection.merge_owned(read, owner, id_ranges.as_deref());
                }
            }
        }
        let prepared = self.prepared.as_ref().and_then(|(windows, specs, prepared)| {
            prepared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(run, *windows, specs)
        });
        let mut uncached = Vec::new();
        for (table, mut selection) in tables {
            selection.normalize();
            let key = (table, selection);
            let next = self.tables.get(&key)
                .map(|dates| dates.get(dates.partition_point(|&at| at <= hour)).copied())
                .or_else(|| prepared.as_ref()?.next(table, &key.1, (first, last), hour));
            if let Some(next) = next {
                if let Some(next) = next {
                    through = through.min(next - 1);
                    if through == hour {
                        return Ok(hour);
                    }
                }
            } else {
                uncached.push(key);
            }
        }
        for (table, selection) in uncached {
            check_cancel(cancelled)?;
            let indexed = match self.variables.as_ref() {
                Some(variables) if matches!(table, TableId::ScriptVariableValues | TableId::ScriptVariableNames) => {
                    variables.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                        .dates(replay.recording(), run, table, first, last, &selection)
                }
                _ => None,
            };
            let Ok(dates) = indexed.map_or_else(|| selection.dates(replay, run, table, first, last), Ok) else { return Ok(hour) };
            let next = dates.get(dates.partition_point(|&at| at <= hour)).copied();
            self.insert_dates(table, selection, dates);
            if let Some(next) = next {
                through = through.min(next - 1);
                if through == hour { break; }
            }
        }
        Ok(through)
    }
}

pub fn reads_through(replay: &Replay, run: u32, reads: &[TableRead], hour: i32, through: i32, cancelled: &AtomicBool) -> io::Result<i32> {
    ReadDates::default().through(replay, run, reads, hour, hour, through, cancelled)
}

pub struct ScriptedInputs {
    replay: Arc<Replay>,
    dates: Mutex<ReadDates>,
    lexical: Option<Lexical>,
    world: Option<Arc<Mutex<ScriptedWorld>>>,
}

impl ScriptedInputs {
    pub fn new(replay: Arc<Replay>) -> Self {
        Self { replay, dates: Mutex::default(), lexical: None, world: None }
    }

    pub fn with_lexical(mut self, source: &Source) -> Self {
        self.lexical = Some(Lexical::for_source(source));
        self
    }

    pub fn with_prepared_block_dates(mut self, source: &Source, specs: &'static [DateSelection]) -> Self {
        let dates = self.dates.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner);
        dates.variables = Some(source.state::<VariableDates>());
        dates.prepared = Some((DateWindows::Blocks, specs, source.state::<PreparedDates>()));
        self
    }

    pub fn with_retained_world(mut self, source: &Source) -> Self {
        self.world = Some(source.state::<ScriptedWorld>());
        self
    }

    pub fn release_world(&self) {
        if let Some(world) = &self.world { world.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release(); }
    }

    pub fn release_world_for(source: &Source) {
        source.state::<ScriptedWorld>().lock().unwrap_or_else(std::sync::PoisonError::into_inner).release();
    }

    pub fn evaluate<T>(&self, frame: &DecodedFrame, tables: &[TableId], cancelled: &AtomicBool, evaluate: impl FnOnce(&DecodedFrame) -> T) -> io::Result<(T, i32)> {
        self.evaluate_with_reads(frame, tables, cancelled, |world| (evaluate(world), Vec::new()))
    }

    pub fn evaluate_with_reads<T>(&self, frame: &DecodedFrame, tables: &[TableId], cancelled: &AtomicBool, evaluate: impl FnOnce(&DecodedFrame) -> (T, Vec<TableRead>)) -> io::Result<(T, i32)> {
        let Some(block) = self.replay.block_at(frame.branch, frame.hour) else {
            if let Some(lexical) = &self.lexical { lexical.release(); }
            self.release_world();
            return Err(io::Error::other("Scripted text hour is not recorded"));
        };
        if self.lexical.is_some() { check_cancel(cancelled)?; }
        let recorded = read_with_lexical(self.lexical.as_ref(), &self.replay, (block.run, frame.hour), tables, |tables| {
            let exact = || self.replay.seek_selected_with(frame.branch, frame.hour, Some(tables), || check_cancel(cancelled));
            let read = if let Some(world) = self.world.as_ref().filter(|_| READ_DATES.get()) {
                check_cancel(cancelled)?;
                let result = world.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                    .read(&self.replay, frame.branch, frame.hour, tables);
                result.or_else(|_| exact())?
            } else {
                exact()?
            };
            match read {
                SeekResult::Frame(recorded) => Ok(recorded),
                SeekResult::Gap { .. } => Err(io::Error::other("Scripted text hour is not recorded")),
            }
        })?;
        let ((value, mut watched), inputs) = dashboard_text::triggers::inputs::capture(&recorded, || evaluate(&recorded));
        if !READ_DATES.get() {
            check_cancel(cancelled)?;
            return Ok((value, frame.hour));
        }
        let through = match inputs.footprint() {
            None => frame.hour,
            Some([]) if watched.is_empty() => i32::MAX,
            Some(reads) => {
                watched.extend_from_slice(reads);
                self.dates.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                    .through(&self.replay, block.run, &watched, block.first_hour, frame.hour, block.last_hour, cancelled)?
            }
        };
        Ok((value, through))
    }
}
