use super::{
    Columns, Content, Notes,
    fold::Fold,
    global::{self, Global, Source},
    kind,
};
use crate::{layers::{Due, Fed, Published, Retirement, RetirementFlag}, owned::{Shape, Standing, Table, Tables}, playhead_host::mirror::Mirrors};
use playhead::Step;
use replay_columnar::manifest::Status;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    mem,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::Instant,
};
use viewer_foundation::perspective::Choice;

#[derive(Default, PartialEq)]
struct Beside {
    statuses: BTreeMap<TableId, Option<Status>>,
    refusals: BTreeMap<TableId, String>,
}

impl Beside {
    fn of(tables: &Tables, status_only: &[(TableId, Standing)]) -> Self {
        Self {
            statuses: global::STATUS_ONLY.iter().map(|&table| (table, status_only.iter().find(|(held, _)| *held == table).and_then(|(_, standing)| standing.status()))).collect(),
            refusals: global::NAMED.iter().filter_map(|&table| Some((table, tables.get(table)?.not_recorded()?))).collect(),
        }
    }
}

#[expect(clippy::large_enum_variant, reason = "a reset is sent only when the worker starts a new epoch, and its fields move straight into the worker")]
enum Job {
    Reset { epoch: u64, tables: Box<Tables>, status_only: Vec<(TableId, Standing)>, kinds: BTreeMap<TableId, Option<Status>>, columns: Columns, choice: Choice, stamp: Option<Hour>, answers: u64 },
    Step { epoch: u64, step: Arc<Step> },
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    answers: u64,
    whole: Option<(Vec<u32>, Vec<u8>)>,
    bytes: Vec<(u32, u8)>,
    all: bool,
    sight: BTreeMap<u64, u32>,
    gap: Option<&'static str>,
    marked: [u64; 2],
    spent: [u64; 4],
    held: usize,
    worker_ns: u64,
    global: Option<Global>,
    rebuilds: [u64; 4],
    controllers: Vec<(u32, Option<u32>)>,
    notes: Option<Notes>,
}

const NO_FOG_CONTENT: &str = "fog content (topology or NIntel defines) failed to load";

struct Work {
    fold: Fold,
    tables: Tables,
    status_only: Vec<(TableId, Standing)>,
    kinds: BTreeMap<TableId, Option<Status>>,
    global: Global,
    choice: Choice,
    beside: Beside,
    reread: bool,
    rebuilds: [u64; 4],
    reported: ([u64; 2], [u64; 4]),
    controllers: BTreeMap<u32, u32>,
    controllers_dirty: BTreeSet<u32>,
    controllers_all: bool,
    columns: Columns,
    frame: u64,
    fresh: bool,
    sight: BTreeMap<u64, u32>,
    gap: Option<&'static str>,
    sources: u64,
    notes: Notes,
    in_gap: bool,
    stamp: Option<Hour>,
    answered: u64,
}

impl Work {
    fn new(tables: Tables, status_only: Vec<(TableId, Standing)>, kinds: BTreeMap<TableId, Option<Status>>, columns: Columns, choice: Choice) -> Self {
        let beside = Beside::of(&tables, &status_only);
        Self {
            fold: Fold::default(),
            tables,
            status_only,
            kinds,
            global: Global::default(),
            choice,
            beside,
            reread: true,
            rebuilds: [0; 4],
            reported: ([0; 2], [0; 4]),
            controllers: BTreeMap::new(),
            controllers_dirty: BTreeSet::new(),
            controllers_all: true,
            columns,
            frame: 0,
            fresh: true,
            sight: BTreeMap::new(),
            gap: None,
            sources: 0,
            notes: Notes::default(),
            in_gap: false,
            stamp: None,
            answered: 0,
        }
    }

    fn step(&mut self, step: &Step) {
        self.frame += 1;
        self.stamp = self.stamp.max(Some(step.last()));
        if step.gap() {
            self.tables.gap();
            self.status_only.iter_mut().for_each(|(_, standing)| standing.gap());
            self.in_gap = true;
            return;
        }
        let frame = self.frame;
        let mut status_changed = Vec::new();
        for changes in step.tables() {
            let table = TableId::ALL[changes.table()];
            if let Some((status, _)) = changes.status()
                && let Some(held) = self.kinds.get_mut(&table)
                && *held != kind(Some(status))
            {
                *held = kind(Some(status));
                status_changed.push(table);
            }
            if let Some((_, standing)) = self.status_only.iter_mut().find(|(held, _)| *held == table) {
                standing.step(changes);
            }
            let Some(held) = self.tables.get_mut(table) else { continue };
            let Some(delta) = held.step(changes) else { continue };
            self.reread |= global::READ.contains(&table);
            let held: &Table = held;
            self.sources += self.fold.apply(table, &delta, held, &self.columns, frame);
            if table == TableId::Provinces {
                for &key in delta.deleted().keys().unwrap_or_default().iter().chain(delta.inserted().keys().unwrap_or_default()) {
                    self.controllers_dirty.insert(key as u32);
                }
                for written in delta.columns().iter().filter(|written| written.column() == self.columns.controller.column) {
                    self.controllers_dirty.extend(written.slots().iter().filter_map(|&slot| held.key(slot)).map(|key| key as u32));
                }
            }
        }
        for table in status_changed {
            self.reread |= global::READ.contains(&table) || global::STATUSES.contains(&table);
            self.controllers_all |= table == TableId::Provinces;
            if let Some(held) = self.tables.get(table) {
                self.sources += self.fold.status_changed(table, held, frame);
            }
        }
        let beside = Beside::of(&self.tables, &self.status_only);
        let moved = beside.statuses.iter().any(|(table, &status)| kind(self.beside.statuses.get(table).copied().flatten()) != kind(status));
        self.reread |= moved || beside.refusals != self.beside.refusals;
        self.beside = beside;
        if mem::take(&mut self.in_gap) {
            self.fold.mark_all(frame);
            (self.reread, self.controllers_all) = (true, true);
        }
    }

    fn unchanged(&self, epoch: u64) -> Made {
        Made {
            epoch,
            stamp: self.stamp,
            answers: self.answered,
            whole: None,
            bytes: Vec::new(),
            all: false,
            sight: self.sight.clone(),
            gap: self.gap,
            marked: [0; 2],
            spent: [0; 4],
            held: self.fold.held() + self.tables.bytes(),
            worker_ns: 0,
            global: None,
            rebuilds: [0; 4],
            controllers: Vec::new(),
            notes: None,
        }
    }

    fn reread(&mut self, content: &Content) -> Option<Global> {
        if !mem::take(&mut self.reread) {
            return None;
        }
        let frame = self.frame;
        let source = Source { tables: &self.tables, statuses: &self.beside.statuses, refusals: &self.beside.refusals };
        let global = Global::read(&source, &self.columns, &self.choice, content);
        let same = global == self.global;
        if !same {
            self.fold.touch_all();
        }
        let moved = if same { None } else { global.moved_terms(&self.global) };
        let relations = !same && (&global.fog_relations, &global.relations) != (&self.global.fog_relations, &self.global.relations);
        match moved {
            Some(part) => {
                self.rebuilds[part] += 1;
                self.fold.mark_all(frame);
            }
            None => self.fold.global_changed(frame, relations),
        }
        self.controllers_all |= self.fresh || global.links != self.global.links;
        let changed = (!same || self.fresh).then(|| global.clone());
        self.global = global;
        changed
    }

    fn settle_controllers(&mut self) -> Vec<(u32, Option<u32>)> {
        let ids: Vec<u32> = if mem::take(&mut self.controllers_all) {
            let mut ids: BTreeSet<u32> = self.controllers.keys().copied().collect();
            if let Some(rows) = self.tables.get(TableId::Provinces) {
                ids.extend(rows.rows().filter_map(|slot| rows.key(slot)).map(|key| key as u32));
            }
            self.controllers_dirty.clear();
            ids.into_iter().collect()
        } else {
            mem::take(&mut self.controllers_dirty).into_iter().collect()
        };
        let rows = self.tables.get(TableId::Provinces).filter(|rows| rows.complete());
        let mut changed = Vec::new();
        for province in ids {
            let controller = rows
                .and_then(|rows| rows.slot(u128::from(province)).and_then(|slot| rows.number(self.columns.controller, slot).value()))
                .and_then(|raw| self.global.links.get(&raw).copied());
            let old = match controller {
                Some(slot) => self.controllers.insert(province, slot),
                None => self.controllers.remove(&province),
            };
            if old != controller {
                changed.push((province, controller));
            }
        }
        changed
    }

    fn notes(&self, content: &Content, gap: Option<&'static str>) -> Notes {
        use viewer_foundation::tables::{Limitations, Perspective};
        let global = &self.global;
        if global.failed.is_some() {
            return Notes::default();
        }
        let columns = &self.columns;
        let tag = self.tables.get(TableId::Countries).and_then(|countries| {
            let row = countries.rows().filter(|&slot| countries.number(columns.country_slot, slot).value() == Some(global.ours)).min_by_key(|&slot| countries.key(slot))?;
            Some(match countries.named_text(columns.country_tag, row) {
                Ok(crate::playhead_host::mirror::Cell::Value(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
                Ok(_) => String::new(),
                Err(id) => format!("country slot {} (its tag, string {id}, is not named at the head)", global.ours),
            })
        });
        let mut perspective = Limitations::default();
        perspective.note_perspective(&Perspective { tag: tag.unwrap_or_else(|| format!("country slot {}", global.ours)), resolution: global.resolution.clone(), fog: global.fog });
        let gap = if content.fog.is_err() { Some(NO_FOG_CONTENT) } else { gap };
        let (mut sight, mut truncated) = (Limitations::default(), Vec::new());
        if global.fog {
            if let Some(table) = global.undecided {
                sight.note_undecided_sight(table);
            }
            if let Some(input) = gap {
                sight.note_visibility_gap(input);
            }
            let country_intel = self.tables.get(TableId::CountryIntel).is_some_and(|table| matches!(table.status(), Some(Status::Truncated { .. })));
            for (table, held) in [("UnitIntel", global.truncated_unit_intel), ("CountryIntel", country_intel)] {
                if held {
                    let mut line = Limitations::default();
                    map_products::note_truncated_perspective(&mut line, table);
                    truncated.extend(line.into_lines());
                }
            }
        }
        Notes { perspective: perspective.into_lines(), fog: global.fog, sight: sight.into_lines(), fog_drawn: global.fog && gap.is_none(), truncated }
    }

    fn make(&mut self, content: &Content, epoch: u64) -> Made {
        let started = Instant::now();
        let frame = self.frame;
        let fresh = self.fresh;
        let global = self.reread(content);
        let controllers = self.settle_controllers();
        self.fold.settle(content, &self.global, &self.columns, &self.tables, frame);
        let changed = std::mem::take(&mut self.fold.changed);
        let sight = self.fold.sight();
        let gap = self.fold.gap(content);
        let marked = match changed.read() {
            crate::layers::Change::Whole => None,
            crate::layers::Change::Marked(marked) => Some(marked),
        };
        let whole = (self.fresh || marked.is_none()).then(|| {
            let bytes = self.fold.province_bytes();
            (bytes.keys().copied().collect(), bytes.values().copied().collect())
        });
        self.fresh = false;
        let bytes = match marked.filter(|_| whole.is_none()) {
            None => Vec::new(),
            Some(marked) => marked.of("province").iter().map(|&province| (province as u32, self.fold.byte_of(province as u32))).collect(),
        };
        let all = whole.is_some() || gap != self.gap;
        (self.sight, self.gap) = (sight.clone(), gap);
        let (marked, spent) = ([self.fold.marked(), self.sources], self.fold.spent());
        let notes = self.notes(content, gap);
        let notes = (fresh || notes != self.notes).then(|| {
            self.notes = notes.clone();
            notes
        });
        let made = Made {
            epoch,
            stamp: self.stamp,
            answers: self.answered,
            whole,
            bytes,
            all,
            sight,
            gap,
            marked: std::array::from_fn(|at| marked[at] - self.reported.0[at]),
            spent: std::array::from_fn(|at| spent[at] - self.reported.1[at]),
            held: self.fold.held() + self.tables.bytes(),
            worker_ns: started.elapsed().as_nanos() as u64,
            global,
            rebuilds: mem::take(&mut self.rebuilds),
            controllers,
            notes,
        };
        self.reported = (marked, spent);
        made
    }
}

fn shapes(columns: &Columns) -> Vec<Shape> {
    use TableId as T;
    let shape = Shape::new;
    vec![
        shape(T::CountryLinks, &[columns.link_slot.column], &[]),
        shape(T::Countries, &[columns.country_slot.column], &[]).with_texts(&[columns.country_tag.column]),
        shape(T::LocalPlayer, &[columns.player_country.column, columns.player_observer.column], &[]),
        shape(T::WarRelations, &[], &[]),
        shape(T::Subjects, &[], &[]),
        shape(T::RelationLinks, &[columns.relation_links.column], &[]),
        shape(T::CountryVisibility, &[columns.unit_modifier.column], &[]),
        shape(T::SimulationFeatures, &[columns.dlc_mask.column], &[]),
        shape(T::Provinces, &[columns.controller.column, columns.province_state.column], &[]),
        shape(T::States, &[columns.state_owner.column, columns.state_controller.column], &[]),
        shape(T::StateVisibility, &[columns.network_modifier.column], &[]),
        shape(T::Divisions, &[columns.roster_owner.column, columns.division_owner.column, columns.division_province.column, columns.reveal_skip.column], &[]),
        shape(T::DivisionMovement, &[], &[(columns.path.column, true)]),
        shape(T::TaskForces, &[columns.task_fleet.column, columns.task_province.column, columns.task_owner.column], &[]),
        shape(T::Fleets, &[columns.fleet_owner.column], &[]),
        shape(T::LandTheatres, &[columns.theatre_owner.column, columns.theatre_ordinal.column, columns.volunteers.column], &[]),
        shape(T::TheatreAreas, &[], &[(columns.area_provinces.column, false)]),
        shape(T::StationedUnits, &[columns.stationed_unit.column], &[]),
        shape(T::VolunteerHosts, &[columns.host.column], &[]),
        shape(T::RadarCoverage, &[], &[(columns.radar_provinces.column, false)]),
        shape(T::CountryIntel, &[columns.army.column], &[]),
        shape(T::ReservedCountryNaming, &[columns.reserved_faction.column], &[(columns.reverse_visibility.column, false), (columns.intel_sample.column, false)]),
        shape(T::CountryCounterInputs, &[], &[(columns.zero_war.column, false), (columns.zero_subject.column, false), (columns.forward_visibility.column, false)]),
        shape(T::FactionMembers, &[columns.faction.column], &[]),
    ]
}

fn superseded(next: Option<&Job>, epoch: u64, due: &Due) -> bool {
    match next {
        Some(Job::Reset { .. }) => true,
        Some(Job::Step { epoch: of, step }) => *of == epoch && due.reached(step.last()),
        _ => false,
    }
}

fn run(content: Arc<Content>, jobs: Receiver<Job>, made: viewer_runtime::landed::Sender<Made>, due: Due) {
    let mut work: Option<Work> = None;
    let mut epoch = 0;
    while let Ok(job) = jobs.recv() {
        let mut queue: VecDeque<Job> = std::iter::once(job).chain(jobs.try_iter()).collect();
        while let Some(job) = queue.pop_front() {
            let product = match job {
                Job::Reset { epoch: next, tables, status_only, kinds, columns, choice, stamp, answers } => {
                    epoch = next;
                    let work = work.insert(Work::new(*tables, status_only, kinds, columns, choice));
                    (work.stamp, work.answered) = (stamp, answers);
                    work.make(&content, epoch)
                }
                Job::Step { epoch: of, step } if of == epoch => {
                    let Some(work) = &mut work else { continue };
                    work.step(&step);
                    let gap = step.gap();
                    drop(step);
                    if superseded(queue.front(), epoch, &due) {
                        continue;
                    }
                    if gap { work.unchanged(epoch) } else { work.make(&content, epoch) }
                }
                Job::Step { .. } => continue,
            };
            if made.send(product).is_err() {
                return;
            }
        }
    }
}

pub(super) struct Folder {
    retiring: RetirementFlag,
    content: Arc<Content>,
    jobs: Option<Sender<Job>>,
    made: Option<viewer_runtime::landed::Receiver<Made>>,
    shapes: Vec<Shape>,
    epoch: u64,
    fed: Fed<Made>,
    kept: VecDeque<Arc<Step>>,
    asked: u64,
    arrived: u64,
    answered: u64,
    asked_at: Option<Hour>,
    present: Vec<u32>,
    held: Vec<bool>,
    bytes: Vec<u8>,
    sight: BTreeMap<u64, u32>,
    gap: Option<&'static str>,
    pub(super) changed: crate::layers::Changed,
    marked: [u64; 2],
    spent: [u64; 4],
    worker_held: usize,
    worker_ns: u64,
    worker_runs: u64,
    global: Option<Global>,
    rebuilds: [u64; 4],
    controllers: Vec<(u32, Option<u32>)>,
    notes: Notes,
}

impl Folder {
    pub(super) fn new(content: Arc<Content>) -> Self {
        Self::with_retirement(content, RetirementFlag::default())
    }

    pub(super) fn with_retirement(content: Arc<Content>, retiring: RetirementFlag) -> Self {
        Self {
            retiring,
            content,
            jobs: None,
            made: None,
            shapes: Vec::new(),
            epoch: 0,
            fed: Fed::default(),
            kept: VecDeque::new(),
            asked: 0,
            arrived: 0,
            answered: 0,
            asked_at: None,
            present: Vec::new(),
            held: Vec::new(),
            bytes: Vec::new(),
            sight: BTreeMap::new(),
            gap: None,
            changed: crate::layers::Changed::default(),
            marked: [0; 2],
            spent: [0; 4],
            worker_held: 0,
            worker_ns: 0,
            worker_runs: 0,
            global: None,
            rebuilds: [0; 4],
            controllers: Vec::new(),
            notes: Notes::default(),
        }
    }

    pub(super) fn reset(&mut self, mirrors: &Mirrors, columns: &Columns, choice: &Choice) {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = viewer_runtime::landed::channel();
            let (content, due) = (Arc::clone(&self.content), self.fed.due());
            if std::thread::Builder::new().name("vision fold".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                run(content, receive, send, due)
            }).is_ok() {
                (self.jobs, self.made) = (Some(jobs), Some(made));
            }
        }
        self.shapes = shapes(columns);
        self.epoch += 1;
        self.fed.compose(self.shapes.first().and_then(|shape| mirrors.get(shape.table)).and_then(|mirror| mirror.head()));
        (self.asked, self.arrived, self.answered, self.asked_at) = (0, 0, 0, None);
        (self.global, self.controllers) = (None, Vec::new());
        self.kept.clear();
        self.start(mirrors, columns, choice, None);
    }

    fn start(&self, mirrors: &Mirrors, columns: &Columns, choice: &Choice, stamp: Option<Hour>) {
        let tables = Box::new(Tables::from_mirrors(mirrors, &self.shapes));
        let status_only = global::STATUS_ONLY.iter().filter_map(|&table| Some((table, Standing::of(mirrors.get(table)?)))).collect();
        let kinds = self.shapes.iter().map(|shape| shape.table).chain(global::STATUS_ONLY).filter_map(|table| Some((table, kind(mirrors.get(table)?.status())))).collect();
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Reset { epoch: self.epoch, tables, status_only, kinds, columns: *columns, choice: choice.clone(), stamp, answers: self.asked });
        }
    }

    pub(super) fn restart(&mut self, mirrors: &Mirrors, columns: &Columns, choice: &Choice) {
        if self.jobs.is_none() {
            return;
        }
        if self.asked == self.answered {
            self.asked_at = self.fed.shown();
        }
        self.asked += 1;
        self.epoch += 1;
        self.fed.restart();
        self.start(mirrors, columns, choice, self.fed.applied_hour());
        if let Some(jobs) = &self.jobs {
            for step in &self.kept {
                let _ = jobs.send(Job::Step { epoch: self.epoch, step: Arc::clone(step) });
            }
        }
    }

    pub(super) fn void(&mut self, mirrors: &Mirrors, columns: &Columns, choice: &Choice) {
        self.kept.clear();
        self.fed.void();
        self.restart(mirrors, columns, choice);
    }

    pub(super) fn hand(&mut self, steps: &[Arc<Step>]) {
        let Some(jobs) = &self.jobs else { return };
        let through = self.fed.through();
        for step in self.fed.hand(steps) {
            let _ = jobs.send(Job::Step { epoch: self.epoch, step: Arc::clone(step) });
            if through.is_none_or(|through| step.last() > through) {
                self.kept.push_back(Arc::clone(step));
            }
        }
    }

    pub(super) fn applied(&mut self, step: &Step) {
        self.fed.applied(step);
        while self.kept.front().is_some_and(|kept| kept.last() <= step.last()) {
            self.kept.pop_front();
        }
    }

    pub(super) fn touch_all(&mut self) {
        self.changed.everything();
    }

    pub(super) fn mark(&mut self, kind: &'static str, id: u64) {
        self.changed.mark(kind, id);
    }

    pub(super) fn retirement(&self) -> Retirement {
        self.retiring.handle()
    }

    pub(super) fn retirement_flag(&self) -> RetirementFlag {
        self.retiring.clone()
    }

    pub(super) fn exchange(&mut self, wait: bool) -> std::io::Result<()> {
        if let Some(receive) = &self.made {
            loop {
                let caught_up = self.fed.arrived() && self.arrived >= self.asked;
                if self.retiring.requested() {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let made = if wait && !caught_up {
                    match receive.recv_timeout(RetirementFlag::POLL) {
                        Ok(made) => Some(made),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
                    }
                } else {
                    receive.try_recv().ok()
                };
                let Some(made) = made else { break };
                if made.epoch != self.epoch {
                    continue;
                }
                self.arrived = self.arrived.max(made.answers);
                self.fed.receive(made.stamp, made);
            }
        }
        let mut due = Vec::new();
        self.fed.take(|made| due.push(made.into_inner()));
        for made in due {
            self.take(made);
        }
        Ok(())
    }

    fn take(&mut self, made: Made) {
        self.answered = self.answered.max(made.answers);
        if let Some((present, bytes)) = made.whole {
            let largest = present.last().map_or(0, |&id| id as usize + 1);
            let (mut held, mut values) = (vec![false; largest], vec![0; largest]);
            for (&id, &byte) in present.iter().zip(&bytes) {
                held[id as usize] = true;
                values[id as usize] = byte;
            }
            for id in 0..largest.max(self.held.len()) {
                let before = self.held.get(id).copied().unwrap_or(false).then(|| self.bytes[id]);
                let after = held.get(id).copied().unwrap_or(false).then(|| values[id]);
                if before != after {
                    self.changed.mark("province", id as u64);
                }
            }
            (self.held, self.bytes, self.present) = (held, values, present);
        }
        if made.all {
            self.touch_all();
        }
        for (province, byte) in made.bytes {
            if let Some(held) = self.bytes.get_mut(province as usize) {
                *held = byte;
                self.changed.mark("province", u64::from(province));
            }
        }
        if made.sight != self.sight {
            let moved = |a: &BTreeMap<u64, u32>, b: &BTreeMap<u64, u32>| a.iter().filter(|(unit, province)| b.get(unit) != Some(province)).map(|(&unit, _)| unit).collect::<Vec<u64>>();
            for unit in moved(&made.sight, &self.sight).into_iter().chain(moved(&self.sight, &made.sight)) {
                self.changed.mark("unit", unit);
            }
        }
        (self.sight, self.gap) = (made.sight, made.gap);
        if made.global.is_some() {
            self.global = made.global;
        }
        self.controllers.extend(made.controllers);
        if let Some(notes) = made.notes {
            self.notes = notes;
        }
        for (held, made) in self.rebuilds.iter_mut().zip(made.rebuilds) {
            *held += made;
        }
        for (held, made) in self.marked.iter_mut().zip(made.marked) {
            *held += made;
        }
        for (held, made) in self.spent.iter_mut().zip(made.spent) {
            *held += made;
        }
        self.worker_held = made.held;
        self.worker_ns += made.worker_ns;
        self.worker_runs += 1;
    }

    pub(super) fn take_controllers(&mut self) -> Vec<(u32, Option<u32>)> {
        mem::take(&mut self.controllers)
    }

    pub(super) fn take_global(&mut self) -> Option<Global> {
        self.global.take()
    }

    pub(super) fn rebuilds(&self) -> [u64; 4] {
        self.rebuilds
    }

    pub(super) fn gap(&self, content: &Content) -> Option<&'static str> {
        if content.fog.is_err() {
            return Some(NO_FOG_CONTENT);
        }
        self.gap
    }

    pub(super) fn notes(&self) -> &Notes {
        &self.notes
    }

    pub(super) fn province_bytes(&self) -> BTreeMap<u32, u8> {
        self.present.iter().map(|&id| (id, self.bytes[id as usize])).collect()
    }

    pub(super) fn bytes_copy(&self) -> (Vec<u32>, Vec<u8>) {
        (self.present.clone(), self.bytes.clone())
    }

    pub(super) fn sight(&self) -> BTreeMap<u64, u32> {
        self.sight.clone()
    }

    pub(super) fn sight_of(&self, unit: u64) -> Option<u32> {
        self.sight.get(&unit).copied()
    }

    pub(super) fn province_byte(&self, province: u32) -> Option<u8> {
        self.held.get(province as usize).copied().unwrap_or(false).then(|| self.bytes[province as usize])
    }

    pub(super) fn intel_byte(&self, province: u32) -> u8 {
        if self.held.get(province as usize).copied().unwrap_or(false) { self.bytes[province as usize] } else { u8::MAX }
    }

    pub(super) fn marked(&self) -> [u64; 2] {
        self.marked
    }

    pub(super) fn spent(&self) -> [u64; 4] {
        self.spent
    }

    pub(super) fn worker(&self) -> (u64, u64) {
        (self.worker_ns, self.worker_runs)
    }

    pub(super) fn restarting(&self) -> bool {
        self.answered < self.asked
    }

    pub(super) fn published(&self) -> Published {
        if self.answered < self.asked {
            return self.asked_at.map_or(Published::Nothing, Published::At);
        }
        self.fed.published()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn made_through(&self) -> Option<Hour> {
        self.fed.made_through()
    }

    pub(super) fn held(&self) -> usize {
        self.present.capacity() * 4 + self.held.capacity() + self.bytes.capacity() + self.sight.len() * 24 + self.worker_held
    }

}

#[cfg(test)]
mod tests {
    use super::{Work, global, kind, shapes};
    use crate::{owned::{Standing, Tables}, vision::Vision};
    use crate::test_support::vision_recording;
    use replay_data::rows::TableId;
    use viewer_foundation::perspective::{Choice, Country};

    #[test]
    fn a_tag_whose_string_is_not_named_is_refused_by_name_not_read_as_empty() {
        let content = vision_recording::vision_content();
        let mut checked = false;
        vision_recording::play(vision_recording::vision_world(), Box::new(Vision::new(vision_recording::vision_content())), 16, |_, mirrors, layer| {
            if std::mem::replace(&mut checked, true) {
                return;
            }
            let columns = layer.any().downcast_ref::<Vision>().unwrap().columns.unwrap();
            let work = |choice: Choice, native: u128| {
                let mut tables = Tables::from_mirrors(mirrors, &shapes(&columns));
                let countries = tables.get_mut(TableId::Countries).unwrap();
                let row = countries.slot(native).expect("the country's row");
                countries.unname(columns.country_tag, row, 77);
                let status_only = global::STATUS_ONLY.iter().filter_map(|&table| Some((table, Standing::of(mirrors.get(table)?)))).collect();
                let kinds = shapes(&columns).iter().map(|shape| shape.table).chain(global::STATUS_ONLY).filter_map(|table| Some((table, kind(mirrors.get(table)?.status())))).collect();
                Work::new(tables, status_only, kinds, columns, choice)
            };
            let made = work(Choice::default(), 101).make(&content, 1);
            let notes = made.notes.expect("a fresh worker makes its notes");
            let line = &notes.perspective()[0];
            assert!(line.contains("country slot 1 (its tag, string 77, is not named at the head)"), "the viewer's tag is refused by name: {line}");
            assert!(!line.starts_with("Viewing country :"), "never an empty tag: {line}");
            let chosen = Choice { country: Country::Tag("text 1".into()), ..Choice::default() };
            let made = work(chosen, 102).make(&content, 1);
            let failed = made.global.expect("a fresh worker makes its whole parts").failed;
            assert_eq!(failed.as_deref(), Some("the country chosen, text 1, cannot be found: country slot 2's tag is string 77, which is not named at the head"));
        });
        assert!(checked);
    }
}
