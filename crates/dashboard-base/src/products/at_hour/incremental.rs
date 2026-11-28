use super::{placed::{Pool, Taken, Warm}, *};
use crate::catalogue::OwnRefusals;
use dashboard_follow::panel::{ProductReader, ProductStep};
use playhead::{Step, changes::Changes, lane::Lane, pages::WorldPages};
use replay_columnar::{manifest::Status, store::TableView as StoreView};
use replay_data::{rows::{RowChanges, TableRows}, scope::CaptureMode};
use replay_frame::frame::{SharedView, TableView, Unsayable, cells_rows_of};
use std::collections::hash_map::Entry;

#[cfg(test)]
mod selection_tests;

pub(super) struct Projection {
    run: usize,
    columns: Vec<usize>,
    format: u16,
    fields: u64,
    validity: u64,
    selection: Option<Arc<Keys>>,
    at: Option<i32>,
    passing: bool,
}

#[derive(Default)]
pub struct Edits {
    pub previous: BTreeMap<TableId, SharedView>,
    pub tables: BTreeMap<TableId, BTreeMap<u128, u64>>,
    pub replaced: BTreeSet<TableId>,
}

impl Edits {
    fn record(&mut self, table: TableId, keys: BTreeMap<u128, u64>) {
        match self.tables.entry(table) {
            std::collections::btree_map::Entry::Vacant(entry) => { entry.insert(keys); }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                for (key, fields) in keys { *entry.get_mut().entry(key).or_default() |= fields; }
            }
        }
    }

    pub fn fields(&self, table: TableId, mask: u64) -> bool {
        self.replaced.contains(&table) || self.tables.get(&table).is_some_and(|keys| keys.values().any(|fields| mask & fields != 0))
    }

    pub fn only_columns(&self, table: TableId, mask: u64) -> bool {
        self.replaced.is_empty() && self.tables.iter().all(|(id, rows)| *id == table && rows.values().all(|fields| fields & !mask == 0))
    }
}

fn field_mask(table: TableId, columns: Option<&[usize]>) -> u64 {
    columns.map_or(table.full_mask(), |v| v.iter().fold(0, |m, &c| m | 1 << c))
}

fn finite_keys(keys: &Keys) -> bool {
    match keys {
        Keys::All => false,
        Keys::These(keys) => keys.windows(2).all(|pair| pair[0] < pair[1]),
        Keys::Range(..) | Keys::Ranges(_) => true,
    }
}

pub(super) fn capture_mode(pages: &WorldPages) -> io::Result<CaptureMode> {
    CaptureMode::from_byte(pages.recording().root_identity().capture_mode)
}

impl Projection {
    fn new(input: &Input<'_>, table: TableId, columns: Option<Vec<usize>>, cold: Option<Vec<usize>>) -> Result<Self, TableView> {
        let pages = &input.pool.pages;
        let recording = pages.recording();
        if recording.predates(table.index()).is_some() {
            return Err(TableView::NotRecorded { mode: input.mode });
        }
        if let Some(reason) = pages.withheld(table.index()) {
            return Err(TableView::Unavailable { reason: reason.to_owned() });
        }
        let fields = field_mask(table, columns.as_deref());
        let omitted = field_mask(table, cold.as_deref()) & !fields;
        let mut validity = 0;
        for column in 0..64 {
            let bit = 1u64 << column;
            if omitted & bit != 0 && table.rows_from_wire_fields(&[0], bit, &mut |_, _| Ok(replay_data::rows::Field::NotRecorded)).is_err() {
                validity |= bit;
            }
        }
        let mut physical = match columns {
            Some(mut columns) => {
                columns.extend((0..64).filter(|&c| validity & (1 << c) != 0));
                columns
            }
            None => recording.read_columns(table.index()),
        };
        let recorded = recording.columns()[table.index()];
        physical.retain(|&column| column < recorded);
        physical.sort_unstable();
        physical.dedup();
        let passing = !playhead::lane::log::is_log(&**pages, table.index()).unwrap_or(true);
        Ok(Self { run: input.run, columns: physical, format: recording.format(), fields, validity, selection: None, at: None, passing })
    }

    fn lane<'s>(&self, table: TableId, hour: i32, step: ProductStep<'s>) -> Option<(&'s Step, &'s Lane, &'s Changes)> {
        if step.step.last() != hour { return None; }
        let lane = step.lanes.get(table.index())?;
        let change = step.step.table(table.index())?;
        (lane.run() == self.run && change.head() == Some(hour) && change.log().is_none() && lane.keyed()
            && self.columns.iter().all(|column| lane.requested().contains(column))).then_some((step.step, lane, change))
    }

    fn read(&mut self, input: &mut Input<'_>, read: &Read, sample: bool, view: &mut SharedView, edits: &mut Edits) -> io::Result<()> {
        let reselected = self.selection.as_deref() != Some(&read.keys);
        let previous = self.at;
        let Input { pool, hour, count, step, placed, mode, gap, exact, .. } = input;
        let (run, table) = (self.run, read.table.index());
        let stepped = if let Some((step, lane, change)) = step.and_then(|step| self.lane(read.table, *hour, step)) {
            self.from_lane(read, sample, view, edits, count, lane, Some((step, change)), reselected, *mode)?
        } else if *exact && let Some((made, sampled)) = empty_read(&*pool.pages, (run, table), *hour, read, *mode)? {
            if matches!(made, TableView::Complete(_) | TableView::Truncated { .. }) || view.held() != &made {
                settle(read.table, Ok(made), sampled, sample, view, edits)?;
            }
            false
        } else {
            let warm = if step.is_some_and(|step| step.lanes_table(table)) {
                let columns = self.columns.clone();
                let wake = step.and_then(|step| step.wake.cloned());
                pool.warm(run, table, &columns, *hour, wake, |lane| self.from_lane(read, sample, view, edits, count, lane, None, reselected, *mode))
            } else { Warm::Cold };
            match warm {
                Warm::Read(result) => result?,
                Warm::Warming => return Err(io::Error::new(io::ErrorKind::WouldBlock, format!("{:?} is being placed beside the step", read.table))),
                Warm::Cold => {
                    let placed = placed.get_or_insert_with(|| pool.take(run, *hour));
                    let selected = if *exact && !matches!(read.keys, Keys::All) {
                        placed.selected(table, &self.columns, *hour, &read.keys)?
                    } else { None };
                    if let Some(selected) = selected {
                        self.whole(read, &selected.view(&read.keys)?, Status::Complete, None, selected.sampled, sample, view, edits, count)?;
                    } else {
                        let lane = placed.lane(table, &self.columns, *hour)?;
                        self.from_lane(read, sample, view, edits, count, lane, None, reselected, *mode)?;
                    }
                    false
                }
            }
        };
        *gap |= reselected || !stepped && previous.is_none_or(|at| at.saturating_add(1) < *hour);
        if reselected { self.selection = Some(Arc::new(read.keys.clone())); }
        *view = view.clone().with_keys(self.selection.clone());
        self.at = Some(*hour);
        Ok(())
    }

    #[expect(clippy::too_many_arguments, reason = "one table's read, its lane and the reply's edits")]
    fn from_lane(&self, read: &Read, sample: bool, view: &mut SharedView, edits: &mut Edits, count: &mut u64, lane: &Lane, step: Option<(&Step, &Changes)>, reselected: bool, mode: CaptureMode) -> io::Result<bool> {
        let table = read.table;
        let continues = |step: &Step, change: &Changes| self.at.is_some_and(|at| at.checked_add(1) == Some(step.first())) && !change.reset();
        let one_hour = step.is_some_and(|(step, _)| step.first() == step.last());
        let continuous = step.is_some_and(|(step, change)| continues(step, change) && change.status_left().is_none() && (one_hour || lane.passing()));
        let proven = !reselected && continuous;
        let (status, sampled) = lane.status();
        let other = match status {
            Status::Complete | Status::Truncated { .. } => None,
            Status::NotRecorded => Some(TableView::NotRecorded { mode }),
            Status::Unavailable { .. } => Some(TableView::Unavailable { reason: lane.reason().and_then(|reason| std::str::from_utf8(reason).ok()).unwrap_or("A table's reason is not text").to_owned() }),
        };
        if let Some(made) = other {
            if *view != made {
                *view = made.into();
                edits.replaced.insert(table);
            }
            return Ok(proven);
        }
        let cells = lane.view();
        if let (TableView::Truncated { missing, .. }, Status::Truncated { missing: still }, Some((step, change))) = (view.held(), status, step)
            && !reselected && *missing == still && continues(step, change) && !change.read_rows_changed() && change.renumber().is_none()
            && change.inserted().is_empty() && change.deleted().is_empty() && change.columns().iter().all(|written| written.is_empty()) {
            return Ok(proven && one_hour);
        }
        let (held, change) = match (view.held(), step) {
            (TableView::Complete(held), Some((step, change))) if status == Status::Complete && continues(step, change) => (held, change),
            _ => return self.whole(read, &cells, status, lane.read_rows(), sampled, sample, view, edits, count).map(|()| false),
        };
        let added = if reselected {
            let added = self.selection.as_deref()
                .filter(|old| continuous && self.validity == 0 && finite_keys(old) && finite_keys(&read.keys))
                .and_then(|old| self.entering(old, read, &cells).ok());
            let Some(added) = added else {
                return self.whole(read, &cells, status, lane.read_rows(), sampled, sample, view, edits, count).map(|()| false);
            };
            *count += added.len() as u64;
            Some(added)
        } else { None };
        let mut dirty: BTreeMap<u128, u64> = BTreeMap::new();
        for &key in change.deleted().keys().into_iter().flatten().chain(change.inserted().keys().into_iter().flatten()).chain(change.passing().keys().into_iter().flatten()) {
            if read.keys.contains(key) { dirty.insert(key, u64::MAX); }
        }
        let renumber = change.renumber();
        let mut evidenced = true;
        let now = |slot: u32| match renumber {
            Some(old) => old.binary_search(&slot).ok().map(|at| at as u32),
            None => Some(slot),
        };
        for column in change.columns() {
            let mask = 1u64 << column.column();
            if (self.fields | self.validity) & mask == 0 { continue; }
            let validity = (self.fields & mask == 0).then(|| lane.cells().column(column.column())).flatten();
            for slot in column.slots().iter().filter_map(|&slot| now(slot)) {
                if validity.is_some_and(|cells| cells.recorded(slot)) {
                    evidenced &= one_hour || lane.cells().key(slot).is_none_or(|key| !read.keys.contains(key));
                    continue;
                }
                if let Some(key) = lane.cells().key(slot).filter(|&k| read.keys.contains(k)) {
                    *dirty.entry(key).or_default() |= mask;
                }
            }
        }
        if added.is_some() {
            dirty.extend(held.iter().map(|row| row.key()).filter(|&key| !read.keys.contains(key)).map(|key| (key, u64::MAX)));
        }
        if dirty.is_empty() && added.as_ref().is_none_or(TableRows::is_empty) {
            if view.sampled_at() != sampled {
                *view = view.clone().with_sampled_at(sampled);
                if sample { edits.replaced.insert(table); }
            }
            return Ok(proven && evidenced);
        }
        let mut keyed = Vec::new();
        let mut deleted = Vec::new();
        for &key in dirty.keys() {
            if added.as_ref().is_some_and(|rows| rows.contains_key(key)) { continue; }
            if let Some(slot) = cells.slot(key).filter(|_| added.is_none() || read.keys.contains(key)) { keyed.push((key, slot)); }
            else if held.contains_key(key) { deleted.push(key); }
        }
        *count += keyed.len() as u64;
        let fresh = match cells_rows_of(table, &cells, &keyed, self.format, self.fields | self.validity) {
            Ok(rows) => rows,
            Err(e) if added.is_some() || Unsayable::of(&e).is_some() => return self.whole(read, &cells, status, None, sampled, sample, view, edits, count).map(|()| false),
            Err(e) => return Err(e),
        };
        let changes = if let Some(added) = added {
            let before: Vec<_> = dirty.keys().copied().filter(|&key| held.contains_key(key)).collect();
            let before = held.rows_at(&before, Vec::new())?;
            let fresh = fresh.apply_changes(RowChanges::new(added, Vec::new()))?;
            let changes = fresh.changes_since(before.rows())?.expect("the same table");
            dirty = changes.rows().iter().map(|row| row.key()).chain(changes.deleted().iter().copied()).map(|key| (key, u64::MAX)).collect();
            if dirty.is_empty() {
                if view.sampled_at() != sampled {
                    *view = view.clone().with_sampled_at(sampled);
                    if sample { edits.replaced.insert(table); }
                }
                return Ok(false);
            }
            changes
        } else { RowChanges::new(fresh, deleted) };
        if let TableView::Complete(old) = &mut **view {
            let previous = std::mem::replace(old, TableRows::empty(table));
            *old = previous.apply_changes(changes)?;
        }
        *view = view.clone().with_sampled_at(sampled);
        edits.record(table, dirty);
        Ok(proven && evidenced)
    }

    fn entering(&self, old: &Keys, read: &Read, cells: &StoreView<'_>) -> io::Result<TableRows> {
        let keys = match &read.keys {
            Keys::These(keys) => keys.iter().copied().filter(|&key| !old.contains(key)).collect(),
            _ => {
                let mut keys: Vec<_> = cells.rows().filter_map(|slot| cells.key(slot)).filter(|&key| read.keys.contains(key) && !old.contains(key)).collect();
                keys.sort_unstable();
                keys
            }
        };
        if keys.is_empty() { return Ok(TableRows::empty(read.table)); }
        let slots = keys.into_iter().map(|key| cells.slot(key).map(|slot| (key, slot))
            .ok_or_else(|| io::Error::other("An entering key is not held"))).collect::<io::Result<Vec<_>>>()?;
        cells_rows_of(read.table, cells, &slots, self.format, self.fields | self.validity)
    }

    #[expect(clippy::too_many_arguments, reason = "one table's cells at an hour and the reply's edits")]
    fn whole(&self, read: &Read, cells: &StoreView<'_>, status: Status, read_rows: Option<&[u32]>, sampled: Option<i32>, sample: bool, view: &mut SharedView, edits: &mut Edits, count: &mut u64) -> io::Result<()> {
        let mut keyed = Vec::new();
        let mut keep = |slot| {
            if let Some(key) = cells.key(slot).filter(|&k| read.keys.contains(k)) { keyed.push((key, slot)); }
        };
        if let Some(rows) = read_rows { rows.iter().copied().for_each(keep); }
        else if let Keys::These(keys) = &read.keys { keyed.extend(keys.iter().filter_map(|&key| Some((key, cells.slot(key)?)))); }
        else { cells.rows().for_each(&mut keep); }
        keyed.sort_unstable();
        *count += keyed.len() as u64;
        let made = cells_rows_of(read.table, cells, &keyed, self.format, self.fields | self.validity).map(|rows| match status {
            Status::Truncated { missing } => TableView::Truncated { rows, missing },
            _ => TableView::Complete(rows),
        });
        settle(read.table, made, sampled, sample, view, edits)
    }
}

fn empty_read(pages: &WorldPages, (run, table): (usize, usize), hour: i32, read: &Read, mode: CaptureMode) -> io::Result<Option<(TableView, Option<i32>)>> {
    let empty = match &read.keys {
        Keys::All => false,
        Keys::These(keys) => keys.is_empty(),
        Keys::Range(low, high) => low > high,
        Keys::Ranges(ranges) => ranges.iter().all(|(low, high)| low > high),
    };
    if !empty { return Ok(None); }
    let (status, sampled, reason) = playhead::lane::table_status(pages, run, table, hour)?;
    let made = match status {
        Status::Complete => TableView::Complete(TableRows::empty(read.table)),
        Status::Truncated { missing } => TableView::Truncated { rows: TableRows::empty(read.table), missing },
        Status::NotRecorded => TableView::NotRecorded { mode },
        Status::Unavailable { .. } => TableView::Unavailable { reason: reason.expect("an unavailable status has a reason") },
    };
    Ok(Some((made, sampled)))
}

fn settle(table: TableId, made: io::Result<TableView>, sampled: Option<i32>, sample: bool, view: &mut SharedView, edits: &mut Edits) -> io::Result<()> {
    let made = match made {
        Ok(made) => made,
        Err(e) if Unsayable::of(&e).is_some() => TableView::Unavailable { reason: e.to_string() },
        Err(e) => return Err(e),
    };
    if let (TableView::Complete(held), TableView::Complete(rows)) = (view.held(), &made) {
        let changes = rows.changes_since(held)?.ok_or_else(|| io::Error::other(format!("{table:?} rows of another table")))?;
        let keys: BTreeMap<u128, u64> = changes.rows().iter().map(|row| row.key()).chain(changes.deleted().iter().copied()).map(|key| (key, u64::MAX)).collect();
        if keys.is_empty() {
            if view.sampled_at() != sampled {
                *view = view.clone().with_sampled_at(sampled);
                if sample { edits.replaced.insert(table); }
            }
            return Ok(());
        }
        *view = SharedView::from(made).with_sampled_at(sampled);
        edits.record(table, keys);
        return Ok(());
    }
    if matches!(made, TableView::Truncated { .. }) && view.held() == &made { return Ok(()); }
    *view = match made {
        TableView::Complete(rows) => SharedView::from(TableView::Complete(rows)).with_sampled_at(sampled),
        made => made.into(),
    };
    edits.replaced.insert(table);
    Ok(())
}

type Projections = HashMap<TableId, Result<Projection, TableView>>;

fn declared((&table, projection): (&TableId, &Result<Projection, TableView>)) -> Option<Declared> {
    let projection = projection.as_ref().ok()?;
    Some(Declared { table: table.index(), columns: projection.columns.clone(), keyed: true, passing: projection.passing, hourly: false })
}

#[derive(Default)]
pub(super) struct StepFrame(Option<(u32, Box<DecodedFrame>, Projections)>);

impl StepFrame {
    pub(super) fn lanes(&self) -> Vec<Declared> {
        self.0.iter().flat_map(|(_, _, tables)| tables.iter().filter_map(declared)).collect()
    }
}

pub struct DatedStep<'a> {
    pub step: Option<ProductStep<'a>>,
    pub(super) frame: &'a mut StepFrame,
}

impl DatedStep<'_> {
    pub fn frame(&mut self, source: &Source, query: &Query<'_>, hour: i32, reads: &[Read], columns: fn(TableId) -> Option<Vec<usize>>) -> io::Result<&DecodedFrame> {
        let prior = self.frame.0.take();
        let run = query.timeline.at(hour).ok_or_else(|| dashboard_core::refused("The recording holds no hour here"))?.run;
        let pool = Pool::of(source)?;
        let read_run = pool.pages.run_of(run).ok_or_else(|| dashboard_core::refused("The recording cannot read this run"))?;
        let (_, mut frame, mut tables) = prior.filter(|(held, frame, tables)| *held == run && frame.branch == query.timeline.id && frame.hour <= hour
            && tables.len() == reads.len() && reads.iter().all(|read| tables.contains_key(&read.table)))
            .unwrap_or_else(|| (run, Box::new(empty(query.timeline.id, hour)), Projections::new()));
        frame.hour = hour;
        let mut count = 0;
        let mut input = Input { pool: &pool, run: read_run, hour, mode: capture_mode(&pool.pages)?, count: &mut count, step: self.step, placed: None, gap: false, exact: false };
        for read in reads {
            let fields = columns(read.table);
            if let Err(error) = input.read(&mut tables, read, &mut frame.tables[read.table.index()], || (fields.clone(), fields.clone()), false, &mut Edits::default()) {
                if error.kind() == io::ErrorKind::WouldBlock { self.frame.0 = Some((run, frame, tables)); }
                return Err(error);
            }
        }
        count_rows(|| count);
        Ok(&self.frame.0.insert((run, frame, tables)).1)
    }
}

pub(super) struct Input<'a> {
    pub(super) pool: &'a Arc<Pool>,
    pub(super) run: usize,
    pub(super) hour: i32,
    pub(super) mode: CaptureMode,
    pub(super) count: &'a mut u64,
    pub(super) step: Option<ProductStep<'a>>,
    pub(super) placed: Option<Taken>,
    pub(super) gap: bool,
    pub(super) exact: bool,
}

impl Input<'_> {
    pub(super) fn frame(&mut self, reads: &[Read], columns: fn(TableId) -> Option<Vec<usize>>, branch: u64) -> io::Result<DecodedFrame> {
        let mut frame = empty(branch, self.hour);
        let mut tables = Projections::new();
        for read in reads {
            let fields = columns(read.table);
            self.read(&mut tables, read, &mut frame.tables[read.table.index()], || (fields.clone(), fields.clone()), false, &mut Edits::default())?;
        }
        count_frame(&frame, reads);
        Ok(frame)
    }

    fn read(&mut self, tables: &mut Projections, read: &Read, view: &mut SharedView, columns: impl Fn() -> (Option<Vec<usize>>, Option<Vec<usize>>), sample: bool, edits: &mut Edits) -> io::Result<()> {
        let table = read.table;
        let (selected, cold) = columns();
        let slot = match tables.entry(table) {
            Entry::Occupied(mut entry) if entry.get().as_ref().is_ok_and(|held| held.fields != field_mask(table, selected.as_deref())) => {
                *entry.get_mut() = Projection::new(self, table, selected, cold);
                entry.into_mut()
            }
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(Projection::new(self, table, selected, cold)),
        };
        let missing = match slot {
            Ok(projection) => match projection.read(self, read, sample, view, edits) {
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    tables.remove(&table);
                    TableView::Unavailable { reason: e.to_string() }
                }
                result => return result,
            },
            Err(missing) if view.held().is_not_selected() => missing.clone(),
            Err(_) => return Ok(()),
        };
        *view = missing.into();
        edits.replaced.insert(table);
        Ok(())
    }
}

#[derive(Default)]
struct Supplemental {
    reads: Vec<Read>,
    columns: HashMap<TableId, Vec<usize>>,
    tables: Projections,
    views: HashMap<TableId, SharedView>,
}

impl Supplemental {
    fn set(&mut self, live: &Live, covered: impl Fn(&Group) -> bool) {
        self.reads = supplemental(live, covered);
        self.columns.clear();
        for group in live.groups.iter().filter(|group| self.reads.iter().any(|read| read.table == group.table)) {
            let columns = self.columns.entry(group.table).or_default();
            columns.extend(&group.columns);
            columns.sort_unstable();
            columns.dedup();
        }
        self.views.retain(|table, _| self.columns.contains_key(table));
        self.tables.retain(|table, _| self.columns.contains_key(table));
    }

    fn advance<P: AtHour>(&mut self, input: &mut Input<'_>) -> io::Result<bool> {
        let mut edits = Edits::default();
        for read in &self.reads {
            let view = self.views.entry(read.table).or_insert_with(not_selected);
            let columns = self.columns.get(&read.table).cloned();
            input.read(&mut self.tables, read, view, || (columns.clone(), P::columns(read.table)), false, &mut edits)?;
        }
        Ok(!edits.tables.is_empty() || !edits.replaced.is_empty())
    }

    fn values(&mut self, live: &Live, frame: &mut DecodedFrame) -> Value {
        for (table, view) in &mut self.views { std::mem::swap(view, &mut frame.tables[table.index()]); }
        let values = live.values(frame);
        for (table, view) in &mut self.views { std::mem::swap(view, &mut frame.tables[table.index()]); }
        live.json(&values.into_iter().map(GroupState::Values).collect::<Vec<_>>())
    }
}

#[derive(Default)]
struct RepeatedReads {
    first: HashMap<TableId, SharedView>,
    stages: HashMap<usize, (Projections, HashMap<TableId, SharedView>)>,
}

struct Stream {
    tables: Projections,
    repeated: RepeatedReads,
    extra: Supplemental,
    plans: Vec<(Vec<Read>, i32)>,
    frame: Box<DecodedFrame>,
    run: u32,
    context_revision: u64,
    identity_through: i32,
    reply_through: i32,
    live: Arc<Live>,
    data: Arc<PanelData>,
    values_at: Option<i32>,
    reply: Option<Box<dyn std::any::Any + Send>>,
    sampled: Option<BTreeSet<TableId>>,
    rebuild: bool,
    from: i32,
}

impl Stream {
    fn lanes(&self) -> Vec<Declared> {
        let mut lanes: Vec<_> = self.tables.iter().filter_map(declared).collect();
        for extra in self.extra.tables.iter().filter_map(declared) {
            if let Some(main) = lanes.iter_mut().find(|lane| lane.table == extra.table) {
                main.columns.extend(extra.columns);
                main.columns.sort_unstable();
                main.columns.dedup();
                main.passing |= extra.passing;
            } else {
                lanes.push(extra);
            }
        }
        lanes
    }
}

pub struct Held<'a>(&'a mut Arc<PanelData>, &'a Finish);

impl Held<'_> {
    pub fn warnings(&mut self, mut warnings: Vec<String>, own: &OwnRefusals) {
        self.1.seal(&mut warnings, own);
        self["warnings"] = warnings.into();
    }
}

impl std::ops::Deref for Held<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        static NONE: Value = Value::Null;
        self.0.as_ref().as_ref().unwrap_or(&NONE)
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut Value {
        let data = Arc::make_mut(self.0);
        if data.is_err() { *data = Ok(Value::Null); }
        match data { Ok(value) => value, Err(_) => unreachable!("made a value above") }
    }
}

fn current_values<P: AtHour>(state: &mut Stream, input: &mut Input<'_>) -> io::Result<Arc<PanelData>> {
    if state.values_at != Some(input.hour) && state.data.is_ok() {
        if !state.live.is_empty() {
            state.extra.advance::<P>(input)?;
            let live = state.extra.values(&state.live, &mut state.frame);
            if let Ok(Value::Object(data)) = Arc::make_mut(&mut state.data) { data.insert("live".into(), live); }
        }
        if let Some(date) = P::DATE_HELD_REPLY && let Ok(data) = Arc::make_mut(&mut state.data) {
            date(data, input.hour);
            state.from = input.hour;
        }
        state.values_at = Some(input.hour);
    }
    Ok(state.data.clone())
}

fn sampling<T>(frame: &DecodedFrame, sampled: &mut Option<BTreeSet<TableId>>, compute: impl FnOnce() -> T) -> T {
    let (value, views) = replay_frame::frame::view_reads::samples(compute);
    let tables = sampled.get_or_insert_with(BTreeSet::new);
    for view in &views {
        if let Some(&table) = TableId::ALL.iter().find(|table| view.is(&frame.tables[table.index()])) { tables.insert(table); }
    }
    value
}

pub(super) fn reader<P: AtHour>(source: &Source, pool: Arc<Pool>, params: Arc<P::Params>, context: Arc<P::Context>, query: Query<'_>, finish: Finish) -> ProductReader
where P::Context: Sync, P::Params: Sync {
    let (timeline, holders, executable) = (query.timeline, query.holders, source.tables());
    use dashboard_query::identity::{Country, Holder};
    let owner = P::owner(&params);
    let owned = |tag: &str, run: &dashboard_query::identity::HolderRun| match owner {
        None => true,
        Some(Country::Tag(owned)) => owned == tag,
        Some(Country::Slot(slot)) => matches!(run.holder, Holder::Found { slot: held, .. } if held == *slot),
    };
    let mut identity_edges: Vec<i32> = holders.tags.iter()
        .flat_map(|(tag, runs)| runs.iter().filter(move |run| owned(tag, run)))
        .flat_map(|r| [Some(r.first), r.last.checked_add(1)].into_iter().flatten()).collect();
    identity_edges.sort_unstable();
    identity_edges.dedup();
    let stream = Mutex::new(None::<Stream>);
    Arc::new(move |hour, values, cancelled, step| {
        let mut held = stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut count = 0;
        let result = engine_executable::executable_tables::scope(executable, || -> io::Result<_> {
            let stretch = timeline.at(hour);
            if let Some(reset) = P::RESET_READERS
                && (stretch.is_none() || step.is_none_or(|input| input.step.reset() || input.step.gap()))
            {
                reset(&context);
            }
            let run = stretch.ok_or_else(|| dashboard_core::refused(format!("{} hour is not recorded", P::TITLE)))?.run;
            let read_run = pool.pages.run_of(run).ok_or_else(|| dashboard_core::refused(format!("{} hour is in a run the recording cannot read", P::TITLE)))?;
            let mut input = Input { pool: &pool, run: read_run, hour, mode: capture_mode(&pool.pages)?, count: &mut count, step, placed: None, gap: false, exact: false };
            if let Some(state) = held.as_mut().filter(|s| s.frame.hour == hour && s.run == run && !s.rebuild) {
                return if values { current_values::<P>(state, &mut input) } else { Ok(state.data.clone()) };
            }
            let owner = holder::<P>(&holders, &params, hour)?;
            let seeking = P::RESEED_ON_SEEK && values && step.is_none_or(|input| input.step.reset());
            let seed = seeking || held.as_ref().is_none_or(|s| s.run != run || hour < s.frame.hour);
            if seed {
                *held = Some(Stream { tables: HashMap::new(), repeated: RepeatedReads::default(), extra: Supplemental::default(), values_at: None, plans: Vec::new(), frame: Box::new(empty(timeline.id, hour)), run, context_revision: 0, identity_through: hour, reply_through: i32::MAX, live: Arc::default(), data: Arc::new(Err(String::new())), reply: None, sampled: None, rebuild: true, from: hour });
            }
            let state = held.as_mut().expect("seeded");
            let previous_hour = state.frame.hour;
            state.frame.hour = hour;
            let identity_changed = seed || hour > state.identity_through;
            if identity_changed {
                state.identity_through = identity_edges.get(identity_edges.partition_point(|&h| h <= hour)).map_or(i32::MAX, |&h| h - 1);
            }
            let mut edits = Edits::default();
            let mut selected = BTreeSet::<TableId>::new();
            let at = ReadsAt { holders: &holders, context: &*context, run, hour, since: (!seed).then_some(previous_hour) };
            let mut tried: HashMap<TableId, Vec<u64>> = HashMap::new();
            loop {
                selected.clear();
                for stage in 0.. {
                    crate::metadata::check_cancel(cancelled)?;
                    let new_stage = stage == state.plans.len();
                    if new_stage || state.rebuild || identity_changed || hour > state.plans[stage].1 || P::plan_changes(stage, &edits, &state.frame, &at) {
                        let mut inputs = empty(timeline.id, hour);
                        for &table in &selected {
                            inputs.tables[table.index()] = state.frame.tables[table.index()].clone();
                        }
                        let planned = sampling(&inputs, &mut state.sampled, || P::retained_reads(&params, owner, stage, &inputs, &at));
                        if new_stage {
                            state.plans.push(planned);
                        } else {
                            state.plans[stage] = planned;
                        }
                    }
                    let (asks, _) = &state.plans[stage];
                    if asks.is_empty() { state.plans.truncate(stage + 1); break; }
                    for read in asks {
                        let table = read.table;
                        let first = selected.insert(table);
                        let view = &mut state.frame.tables[table.index()];
                        if !seed && P::compares_previous(table) { edits.previous.entry(table).or_insert_with(|| view.clone()); }
                        let sample = state.sampled.as_ref().is_none_or(|sampled| sampled.contains(&table));
                        let columns = || (P::projection(&params, table, &state.plans, &context), P::columns(table));
                        if first {
                            if let Some(prior) = state.repeated.first.get_mut(&table) {
                                input.read(&mut state.tables, read, prior, columns, sample, &mut edits)?;
                                *view = prior.clone();
                            } else {
                                input.read(&mut state.tables, read, view, columns, sample, &mut edits)?;
                            }
                        } else {
                            state.repeated.first.entry(table).or_insert_with(|| view.clone());
                            let (projections, views) = state.repeated.stages.entry(stage).or_default();
                            let repeated = views.entry(table).or_insert_with(not_selected);
                            input.read(projections, read, repeated, columns, sample, &mut edits)?;
                            *view = repeated.clone();
                        }
                    }
                }
                let projected = |table: TableId| field_mask(table, P::projection(&params, table, &state.plans, &context).as_deref());
                let moved: Vec<(TableId, u64, u64)> = state.tables.iter()
                    .filter_map(|(&table, held)| held.as_ref().ok().filter(|held| selected.contains(&table) && held.fields != projected(table)).map(|held| (table, held.fields, projected(table))))
                    .collect();
                if moved.is_empty() { break; }
                for (table, read, asked) in moved {
                    let masks = tried.entry(table).or_default();
                    masks.push(read);
                    if masks.contains(&asked) { return Err(io::Error::other(format!("{} reads {table:?} under projections that do not settle", P::TITLE))); }
                }
            }
            state.repeated.first.retain(|table, _| selected.contains(table));
            state.repeated.stages.retain(|stage, (projections, views)| {
                let asked = |table: &TableId| state.plans.get(*stage).is_some_and(|(reads, _)| reads.iter().any(|read| read.table == *table));
                projections.retain(|table, _| asked(table));
                views.retain(|table, _| {
                    let kept = asked(table);
                    if !kept { edits.replaced.insert(*table); }
                    kept
                });
                !views.is_empty()
            });
            state.tables.retain(|table, _| {
                if selected.contains(table) { true } else {
                    state.frame.tables[table.index()] = not_selected();
                    edits.replaced.insert(*table);
                    false
                }
            });
            let cells = Cells::default();
            let at = At { owner, holders: &holders, context: &*context, cancelled, hour, live: &cells };
            let revision = P::context_revision(&context);
            let expired = hour > state.reply_through || revision != state.context_revision;
            if state.rebuild || identity_changed || input.gap || expired || !edits.tables.is_empty() || !edits.replaced.is_empty() {
                state.from = hour;
            }
            if !state.rebuild && !identity_changed {
                let changed = !edits.tables.is_empty() || !edits.replaced.is_empty();
                if !changed && !expired {
                    return if values { current_values::<P>(state, &mut input) } else { Ok(state.data.clone()) };
                }
                if state.data.is_ok() && hour <= state.reply_through && (revision == state.context_revision || P::UPDATES_REVISION) && selected.iter().all(|table| matches!(state.frame.tables[table.index()].held(), TableView::Complete(_))) {
                    let kept = state.data.clone();
                    if P::advance(&state.frame, &params, &at, &edits, &state.live, &mut Held(&mut state.data, &finish))? {
                        state.reply = None;
                        state.context_revision = revision;
                        state.reply_through = state.reply_through.min(P::valid_through(&state.frame, &params, &at));
                        return if values { current_values::<P>(state, &mut input) } else { Ok(state.data.clone()) };
                    }
                    state.data = kept;
                }
            }
            let mut own = OwnRefusals::default();
            #[cfg(any(test, feature = "test-support"))]
            crate::test_fixtures::product_called();
            let result = sampling(&state.frame, &mut state.sampled, || finish.scope(&mut own, || P::playback_reply(&state.frame, &params, &at)));
            let mut reply = match result {
                Ok(reply) => reply,
                Err(error) if dashboard_core::refusal(&error).is_some() && P::holds_refusal(&state.frame) => {
                    state.data = Arc::new(Err(Finish::refusal(&error)));
                    state.live = Arc::default();
                    state.extra = Supplemental::default();
                    state.reply = None;
                    state.reply_through = sampling(&state.frame, &mut state.sampled, || P::valid_through(&state.frame, &params, &at));
                    state.context_revision = revision;
                    state.rebuild = false;
                    state.values_at = Some(hour);
                    return Ok(state.data.clone());
                }
                Err(error) => return Err(error),
            };
            let partial = selected.iter().any(|table| matches!(state.frame.tables[table.index()].held(), TableView::Truncated { .. }));
            let reply_through = sampling(&state.frame, &mut state.sampled, || P::valid_reply_through(&state.frame, &params, &at, &reply, partial));
            finish.seal(reply.warnings_mut(), &own);
            let live = cells.finish();
            let projected = |table: TableId| P::projection(&params, table, &state.plans, &context);
            state.extra.set(&live, |g| state.plans.iter().flat_map(|(reads, _)| reads).rev().find(|r| r.table == g.table)
                .is_some_and(|r| covers(r, g) && projected(g.table).is_none_or(|fields| g.columns.iter().all(|column| fields.contains(column)))));
            state.context_revision = revision;
            state.reply_through = reply_through;
            if !seed && !state.rebuild && state.data.is_ok() && *state.live == live
                && state.reply.as_ref().and_then(|last| last.downcast_ref::<P::Reply>()).is_some_and(|last| *last == reply) {
                return if values { current_values::<P>(state, &mut input) } else { Ok(state.data.clone()) };
            }
            state.live = Arc::new(live);
            state.extra.advance::<P>(&mut input)?;
            let live = (!state.live.is_empty()).then(|| state.extra.values(&state.live, &mut state.frame));
            let data = serde_json::to_value(WithLive { reply: &reply, live })?;
            state.reply = Some(Box::new(reply));
            state.rebuild = false;
            state.values_at = Some(hour);
            state.data = Arc::new(Ok(data));
            Ok(state.data.clone())
        });
        let pending = result.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock) && held.is_some();
        let result = match result {
            Ok(data) => data,
            Err(_) if pending => {
                let state = held.as_mut().expect("held while pending");
                state.rebuild = true;
                state.data.clone()
            }
            Err(e) => { *held = None; Arc::new(Err(Finish::refusal(&e))) }
        };
        let fresh = !pending && held.as_ref().is_some_and(|s| s.values_at == Some(hour));
        let live = held.as_ref().map(|s| s.live.clone()).filter(|live| !live.is_empty());
        let lanes = held.as_ref().map_or_else(Vec::new, Stream::lanes);
        let from = held.as_ref().map_or(hour, |s| s.from);
        dashboard_follow::panel::ProductRead { value: result, rows: count, fresh, pending, live, lanes, from }
    })
}
