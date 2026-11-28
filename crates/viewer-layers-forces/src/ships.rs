
use viewer_layer_core::layers::{Carried, Settling, Dirty, Inputs, Layer, List, Num, Published, Retirement, Text, Upstream, View};
use viewer_layer_core::playhead_host::{Entry, mirror::{Cell, Mirror, Mirrors}, views::{Recording, self}};
use playhead::Step;
use replay_columnar::manifest::Status;
use viewer_layer_core::playhead_host::timeline::{self, Mark};
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::DecodedFrame;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_gpu::{camera::{MapDefines, MapSize, Placement}, province_admission::Admission};
use viewer_unit_reads::{counter_move::UnitRoute, task_force_visual::skip_of};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_unit_models::{
    equipment_graphics::Decided,
    unit_models::{Force, ForcePart, ModelReport, ResolvedRoster},
};
use viewer_unit_history::unit_history::NavalSources;

const ROSTER_TABLES: &[TableId] = &[TableId::ShipModels, TableId::Ships, TableId::TaskForces, TableId::VariantDefinitions, TableId::Countries, TableId::CountryLinks, TableId::Clock];

pub type Roster = Option<Result<ResolvedRoster, String>>;

pub trait Naval: Send + Sync {
    fn drawn_at(&self, height: f32) -> bool;
    fn bounds(&self) -> &BTreeMap<u32, [f32; 4]>;
    fn map(&self) -> (MapSize, MapDefines);
    fn region(&self, province: u32) -> Option<u32>;
    #[expect(clippy::too_many_arguments, reason = "one formation's every input, as the builder's core takes them")]
    fn figure(&self, force: &Force, route: Result<UnitRoute<'_>, &'static str>, link: &dyn Fn(u32) -> Option<u32>, vision: &Vision, skip: &dyn Fn(u32) -> Option<bool>, ordinal: Option<u32>, speed: Option<i32>, day: i32, roster: &mut dyn FnMut() -> Roster) -> ForcePart;
    fn rosters(&self, frame: &DecodedFrame, forces: &[u64]) -> io::Result<(Vec<(u64, Roster)>, Decided)>;
    fn decide_the_same(&self, frame: &DecodedFrame, decided: &Decided) -> bool;
}


#[derive(Clone, Copy)]
struct Columns {
    province: Num<u32>,
    logical_owner: Num<u32>,
    roster_owner: Num<u32>,
    expeditionary_owner: Num<u32>,
    mission: Num<u32>,
    target: Num<u32>,
    combat_role: Num<u8>,
    path: List,
    alternate_path: List,
    is_in_regions: Num<u8>,
    bombardment_region: Num<u32>,
    current_region: Num<u32>,
    mixed: List,
    task_force: Num<u64>,
    class_id: Num<i32>,
    model: [usize; 6],
    explicit_entity: usize,
    tag: Text,
    link_slot: Num<u32>,
    speed: Num<i32>,
}

#[derive(Clone, Default)]
struct Held {
    force: Option<Force>,
    admitted: bool,
    part: Option<ForcePart>,
    roster: Option<Arc<Roster>>,
    asked: bool,
    in_view_at: Option<Hour>,
    contested: bool,
}

struct Asked {
    source: Option<(Arc<Entry>, Recording)>,
    open: bool,
    stop: bool,
    head: Option<Hour>,
    seek: Option<u64>,
    views: Option<Arc<views::Reader>>,
    wanted: Vec<(u64, Hour)>,
    known: Vec<u64>,
    decided: Decided,
    answers: viewer_runtime::landed::Published<Vec<(u64, Arc<Roster>)>>,
    answered: viewer_runtime::landed::Published<Option<Hour>>,
    building: Vec<(u64, Hour)>,
    answered_at: Option<Instant>,
    tables: Vec<TableId>,
    builds: Vec<u64>,
    error: viewer_runtime::landed::Published<Option<String>>,
    refused: viewer_runtime::landed::Published<Option<String>>,
    woken: bool,
    let_go: bool,
    loading: bool,
    #[cfg(any(test, feature = "test-support"))]
    looks: u64,
}

impl Asked {
    fn rows_at(&self, hour: Hour) -> bool {
        self.refused.as_ref().is_some() || self.views.as_ref().is_some_and(|reader| reader.has_rows_at(hour))
    }

    fn made_at(&self, hour: Hour) -> bool {
        !self.open || (self.rows_at(hour) && self.wanted.is_empty() && self.building.is_empty() && self.answered.as_ref() == Some(&hour))
    }
}

struct Shared {
    asked: Mutex<Asked>,
    wake: Condvar,
    answered: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.answered.notify_all();
        self.wake.notify_all();
    }

    fn published(&self, asked: &mut Asked, maker: timeline::Maker) {
        asked.answered_at = Some(Instant::now());
        timeline::mark(maker, Mark::Published);
        if let Some(reader) = &asked.views {
            reader.notify_progress();
        }
        self.answered.notify_all();
    }
}

pub struct Rosters {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    maker: timeline::Maker,
}

impl Rosters {
    fn new(naval: Arc<dyn Naval>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            asked: Mutex::new(Asked { source: None, open: false, stop: false, head: None, seek: None, wanted: Vec::new(), known: Vec::new(), decided: Decided::new(), answers: Default::default(), answered: Default::default(), building: Vec::new(), answered_at: None, views: None, let_go: false, tables: ROSTER_TABLES.to_vec(), builds: Vec::new(), error: Default::default(), refused: Default::default(), loading: false, woken: false,
                #[cfg(any(test, feature = "test-support"))]
                looks: 0,
            }),
            wake: Condvar::new(),
            answered: Condvar::new(),
        });
        let working = Arc::clone(&shared);
        let maker = timeline::maker("ship_rosters");
        let worker = std::thread::Builder::new().name("ship rosters".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            resolve(&working, &*naval, maker)
        })?;
        Ok(Self { shared, worker: Some(worker), maker })
    }

    fn set_open(&self, open: bool) {
        let mut asked = self.shared.lock();
        if asked.open != open {
            asked.open = open;
            if !open {
                asked.wanted.clear();
                asked.known.clear();
                asked.decided.clear();
                asked.answers.clear();
            }
            asked.woken = true;
            drop(asked);
            self.shared.wake.notify_all();
        }
    }

    fn applied(&self, hour: Hour) {
        let mut asked = self.shared.lock();
        let seek = asked.source.as_ref().map(|(entry, _)| entry.playhead().epoch());
        if asked.head == Some(hour) && asked.seek == seek {
            return;
        }
        asked.head = Some(hour);
        timeline::mark(self.maker, Mark::Asked);
        if asked.seek.is_some_and(|before| Some(before) != seek) {
            asked.answers.clear();
        }
        asked.seek = seek.or(asked.seek);
        for (_, at) in &mut asked.wanted {
            *at = (*at).min(hour);
        }
        asked.woken = true;
        drop(asked);
        self.shared.wake.notify_all();
    }

    fn ask(&self, forces: impl IntoIterator<Item = u64>) {
        let mut asked = self.shared.lock();
        let before = asked.wanted.len();
        let at = asked.head.unwrap_or(Hour::MIN);
        asked.wanted.extend(forces.into_iter().map(|force| (force, at)));
        if asked.wanted.len() != before {
            asked.woken = true;
            drop(asked);
            self.shared.wake.notify_all();
        }
    }

    fn forget(&self, forces: impl IntoIterator<Item = u64>) {
        let gone: Vec<u64> = forces.into_iter().collect();
        if gone.is_empty() {
            return;
        }
        let mut asked = self.shared.lock();
        asked.known.retain(|force| !gone.contains(force));
        asked.wanted.retain(|(force, _)| !gone.contains(force));
        asked.answers.retain(|(force, _)| !gone.contains(force));
    }

    fn forget_all(&self) {
        let mut asked = self.shared.lock();
        asked.known.clear();
        asked.answers.clear();
    }

    fn take(&self) -> Vec<(u64, Arc<Roster>)> {
        self.shared.lock().answers.take_all().map(viewer_runtime::landed::Landed::into_inner).unwrap_or_default()
    }

    fn answered(&self) -> Option<Hour> {
        self.shared.lock().answered.as_ref().copied()
    }

    fn owed(&self) -> Vec<(u64, Hour)> {
        let asked = self.shared.lock();
        asked.wanted.iter().chain(&asked.building).copied().collect()
    }

    pub fn wait(&self, hour: Hour, limit: Duration) -> io::Result<()> {
        let began = Instant::now();
        let until = began + limit;
        let mut asked = self.shared.lock();
        if asked.head.is_none() || !asked.open {
            return Ok(());
        }
        let (mut slices, mut timeouts) = (0_u64, 0_u64);
        loop {
            if asked.stop {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if let Some(error) = asked.error.as_ref() {
                return Err(io::Error::other(format!("The ship rosters' worker stopped: {error}")));
            }
            if asked.made_at(hour) {
                waited_for(began, asked.answered_at, slices, timeouts, true);
                return Ok(());
            }
            if !asked.rows_at(hour) {
                waited_for(began, asked.answered_at, slices, timeouts, false);
                return Err(io::Error::new(io::ErrorKind::WouldBlock, format!("The ship rosters await {:?} at hour {hour}", asked.tables)));
            }
            let now = Instant::now();
            if now >= until {
                waited_for(began, asked.answered_at, slices, timeouts, false);
                return Err(io::Error::other(format!("The ship rosters were not answered at hour {hour} in {limit:?} (rows at {:?}, {} asked, loading {})", asked.answered.as_ref(), asked.wanted.len(), asked.loading)));
            }
            let (held, ended) = self.shared.answered.wait_timeout(asked, until - now).unwrap_or_else(std::sync::PoisonError::into_inner);
            asked = held;
            slices += 1;
            timeouts += u64::from(ended.timed_out());
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn closed(&self) {
        let mut asked = self.shared.lock();
        while !asked.let_go {
            asked = self.shared.answered.wait(asked).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending(&self) -> bool {
        !self.shared.lock().wanted.is_empty()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn looks(&self) -> u64 {
        self.shared.lock().looks
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn loading(&self) -> bool {
        self.shared.lock().loading
    }

    pub fn measured(&self) -> (Vec<u64>, Vec<TableId>) {
        let asked = self.shared.lock();
        (asked.builds.clone(), asked.tables.clone())
    }

}

impl Drop for Rosters {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn waited_for(began: Instant, answered_at: Option<Instant>, slices: u64, timeouts: u64, answered: bool) {
    use viewer_runtime::view_record::add;
    if !viewer_runtime::view_record::on() {
        return;
    }
    let late = answered_at.filter(|_| answered).map_or(0, |at| began.max(at).elapsed().as_micros() as u64);
    add("ship rosters", "waits", 1);
    add("ship rosters", "wait_us", began.elapsed().as_micros() as u64);
    add("ship rosters", "wait_late_us", late);
    add("ship rosters", "wait_slices", slices);
    add("ship rosters", "wait_timeouts", timeouts);
    add("ship rosters", "waits_unanswered", u64::from(!answered));
}

fn resolve(shared: &Shared, naval: &dyn Naval, maker: timeline::Maker) {
    let mut reader: Option<Arc<views::Reader>> = None;
    let mut last: Option<Arc<DecodedFrame>> = None;
    loop {
        let mut asked = shared.lock();
        if asked.stop {
            return;
        }
        if !asked.open {
            if let Some(held) = reader.take() {
                held.declare_only(&[]);
                asked.let_go = true;
                shared.answered.notify_all();
            }
            (last, asked.loading) = (None, false);
            if !std::mem::take(&mut asked.woken) {
                let _ = shared.wake.wait_timeout(asked, Duration::from_millis(17));
            }
            continue;
        }
        if let Some(why) = asked.refused.as_ref().cloned() {
            if let Some(held) = reader.take() {
                held.declare_only(&[]);
                asked.let_go = true;
            }
            asked.loading = false;
            let forces = std::mem::take(&mut asked.wanted);
            let refused = !forces.is_empty();
            asked.answers.extend_nonempty(forces.into_iter().map(|(force, _)| (force, Arc::new(Some(Err(why.clone()))))));
            if let Some(head) = asked.head.filter(|&head| refused || asked.answered.as_ref() != Some(&head)) {
                asked.answered.set_if_changed(Some(head));
                shared.published(&mut asked, maker);
            }
            if !std::mem::take(&mut asked.woken) {
                let _ = shared.wake.wait_timeout(asked, Duration::from_millis(17));
            }
            continue;
        }
        if reader.is_none() {
            reader = asked.views.clone();
            if let Some(reader) = &reader {
                asked.let_go = false;
                reader.declare_only(&asked.tables);
            }
        }
        let before = reader.as_ref().map(|reader| reader.published());
        let frame = reader.as_ref().and_then(|reader| reader.frame()).filter(|frame| viewer_layer_core::rebuilt::holds(frame, &asked.tables));
        let loading = reader.is_some() && frame.is_none();
        if loading && !asked.loading {
            shared.answered.notify_all();
        }
        asked.loading = loading;
        let Some(frame) = frame else {
            if let (Some(reader), Some(before)) = (&reader, before) {
                drop(asked);
                reader.wait_newer(before, Duration::from_millis(17));
            } else if !std::mem::take(&mut asked.woken) {
                let _ = shared.wake.wait_timeout(asked, Duration::from_millis(17));
            }
            continue;
        };
        #[cfg(any(test, feature = "test-support"))]
        {
            asked.looks += 1;
        }
        let newer = last.as_ref().is_none_or(|last| !Arc::ptr_eq(last, &frame));
        if newer {
            timeline::mark(maker, Mark::Rows);
        }
        let scripts_moved = newer && last.as_ref().is_some_and(|last| {
            (0..frame.tables.len()).any(|table| !ROSTER_TABLES.iter().any(|known| known.index() == table) && !replay_frame::frame::SharedView::ptr_eq(&frame.tables[table], &last.tables[table]))
        });
        let recheck = (newer && !scripts_moved && !asked.decided.is_empty()).then(|| asked.decided.clone());
        let mut limits_moved = false;
        if let Some(decided) = recheck {
            drop(asked);
            let (same, missed) = replay_frame::frame::discovering(|| naval.decide_the_same(&frame, &decided));
            limits_moved = !same || !missed.is_empty();
            asked = shared.lock();
        }
        let Some((through, seek)) = reader.as_ref().and_then(|reader| reader.through_seek(Arc::as_ptr(&frame))) else { continue };
        if asked.seek.is_some_and(|current| seek < current) {
            if !std::mem::take(&mut asked.woken) {
                let _ = shared.wake.wait_timeout(asked, Duration::from_millis(17));
            }
            continue;
        }
        let stands = through.max(frame.hour);
        let (mut batch, later): (Vec<(u64, Hour)>, Vec<(u64, Hour)>) = std::mem::take(&mut asked.wanted).into_iter().partition(|&(_, at)| at <= stands);
        asked.wanted = later;
        let whole = scripts_moved || limits_moved;
        if whole {
            batch.extend(asked.known.iter().map(|&force| (force, stands)));
        }
        let mut forces: Vec<u64> = batch.iter().map(|&(force, _)| force).collect();
        if forces.is_empty() {
            if newer {
                asked.answered.set_if_changed(Some(frame.hour));
                last = Some(frame);
                shared.published(&mut asked, maker);
            } else if let Some(stands) = reader.as_ref().and_then(|reader| reader.through(Arc::as_ptr(&frame)))
                && asked.answered.as_ref().is_some_and(|&answered| answered < stands)
            {
                asked.answered.advance(stands);
                shared.published(&mut asked, maker);
            }
            let seen = reader.as_ref().map_or(0, |reader| reader.published());
            let woken = std::mem::take(&mut asked.woken);
            match (woken, reader.as_ref()) {
                (true, _) => drop(asked),
                (false, Some(reader)) => {
                    drop(asked);
                    reader.wait_newer(seen, Duration::from_millis(17));
                }
                (false, None) => {
                    let _ = shared.wake.wait_timeout(asked, Duration::from_millis(17));
                }
            }
            continue;
        }
        forces.sort_unstable();
        forces.dedup();
        asked.building.clone_from(&batch);
        drop(asked);
        timeline::mark(maker, Mark::BuildBegan);
        let started = Instant::now();
        let (resolved, missed) = replay_frame::frame::discovering(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| naval.rosters(&frame, &forces))).unwrap_or_else(|panic| Err(io::Error::other(format!("The rosters' build panicked: {}", viewer_layer_core::rebuilt::panicked(&*panic)))))
        });
        let ns = started.elapsed().as_nanos() as u64;
        timeline::mark(maker, Mark::BuildEnded);
        let mut asked = shared.lock();
        asked.building.clear();
        if asked.seek.is_some_and(|current| seek < current) {
            let head = asked.head.unwrap_or(Hour::MIN);
            asked.wanted.extend(batch.into_iter().map(|(force, at)| (force, at.min(head))));
            last = None;
            continue;
        }
        if !missed.is_empty() {
            asked.wanted.extend(batch);
            let absent = asked.source.as_ref().map_or_else(Vec::new, |(entry, _)| viewer_layer_core::rebuilt::absent(entry, &missed));
            if !absent.is_empty() {
                asked.refused.set_if_changed(Some(viewer_layer_core::rebuilt::refusal("the ships' rosters", &absent)));
                continue;
            }
            asked.tables.extend(missed);
            asked.tables.sort_unstable_by_key(|table| table.index());
            asked.tables.dedup();
            if let Some(reader) = &reader {
                reader.declare_only(&asked.tables);
                asked.loading = true;
                shared.answered.notify_all();
            }
            last = None;
            continue;
        }
        if whole {
            asked.decided.clear();
        }
        let answers: Vec<(u64, Arc<Roster>)> = match resolved {
            Ok((resolved, decided)) => {
                asked.decided.extend(decided);
                resolved.into_iter().map(|(force, roster)| (force, Arc::new(roster))).collect()
            }
            Err(error) => forces.iter().map(|&force| (force, Arc::new(Some(Err(error.to_string()))))).collect(),
        };
        asked.builds.push(ns);
        asked.known.extend(forces);
        asked.known.sort_unstable();
        asked.known.dedup();
        asked.answers.extend_nonempty(answers);
        asked.answered.set_if_changed(Some(frame.hour));
        shared.published(&mut asked, maker);
        last = Some(frame);
    }
}

fn rows(mirror: Option<&Mirror>) -> Option<&Mirror> {
    mirror.filter(|mirror| matches!(mirror.status(), Some(Status::Complete | Status::Truncated { .. })))
}

fn complete(mirror: Option<&Mirror>) -> Option<&Mirror> {
    mirror.filter(|mirror| mirror.status() == Some(Status::Complete))
}

fn decode_u32(bytes: &[u8], out: &mut Vec<u32>) {
    out.clear();
    out.extend(bytes.chunks_exact(4).map(|item| u32::from_le_bytes([item[0], item[1], item[2], item[3]])));
}

fn motion_route<'a>(motion: Option<&Mirror>, columns: &Columns, force: u64, path: &'a mut Vec<u32>, alternate: &'a mut Vec<u32>) -> Result<UnitRoute<'a>, &'static str> {
    let motion = motion.ok_or("task-force motion census incomplete")?;
    let row = motion.slot(u128::from(force)).ok_or("task-force motion row absent")?;
    for (column, out) in [(columns.path, &mut *path), (columns.alternate_path, &mut *alternate)] {
        match motion.list(column, row) {
            Cell::Value(bytes) => decode_u32(bytes, out),
            _ => out.clear(),
        }
    }
    let combat_role = motion.number(columns.combat_role, row).value().unwrap_or(0);
    Ok(UnitRoute {
        class: viewer_runtime::unit_class::CounterClass::TaskForce,
        combat_role,
        combat_count: u32::from(combat_role != 0),
        combat_province: 0,
        target: motion.number(columns.target, row).value().unwrap_or(0),
        naval_transfer: false,
        path_total: path.len() as u32,
        path,
        alternate_total: alternate.len() as u32,
        alternate_path: alternate,
    })
}

fn skip_inputs(inputs: Option<&Mirror>, columns: &Columns, force: u64) -> Option<(u8, u32)> {
    let inputs = inputs?;
    let row = inputs.slot(u128::from(force))?;
    Some((inputs.number(columns.is_in_regions, row).value().unwrap_or(0), inputs.number(columns.bombardment_region, row).value().unwrap_or(0)))
}

pub struct Ships {
    naval: Arc<dyn Naval>,
    given: Mutex<Option<(Choice, Arc<Vision>)>>,
    vision: Option<(Choice, Arc<Vision>)>,
    columns: Option<Columns>,
    refused: Option<String>,
    forces: Vec<Held>,
    by_id: BTreeMap<u64, u32>,
    ship_force: HashMap<u64, u64>,
    in_province: HashMap<u32, Vec<u64>>,
    dirty: Dirty,
    figures: Dirty,
    camera: Option<(Placement, [u32; 2])>,
    admission: Option<Admission>,
    nearby: Option<Admission>,
    camera_dirty: bool,
    all_dirty: bool,
    rosters_dirty: bool,
    statuses: HashMap<usize, Status>,
    rosters: Rosters,
    reuse: bool,
    day: Option<i32>,
    frame: u64,
    marked: [usize; 3],
    path: Vec<u32>,
    alternate: Vec<u32>,
    window: bool,
    sight_epoch: Option<u64>,
    born: bool,
    like_today: bool,
    touched: u64,
    published: (u64, [usize; 2], u64),
    needs_today: Option<String>,
    warnings: Vec<String>,
    warned: Option<(u64, Option<String>)>,
    product_hour: Option<Hour>,
    head: Option<Hour>,
}

impl Ships {
    pub fn new(naval: Arc<dyn Naval>) -> io::Result<Self> {
        let rosters = Rosters::new(Arc::clone(&naval))?;
        Ok(Self {
            naval,
            given: Mutex::new(None),
            vision: None,
            columns: None,
            refused: Some("not composed yet".into()),
            forces: Vec::new(),
            by_id: BTreeMap::new(),
            ship_force: HashMap::new(),
            in_province: HashMap::new(),
            dirty: Dirty::default(),
            figures: Dirty::default(),
            camera: None,
            admission: None,
            nearby: None,
            camera_dirty: false,
            all_dirty: true,
            rosters_dirty: true,
            statuses: HashMap::new(),
            rosters,
            reuse: false,
            day: None,
            frame: 0,
            marked: [0; 3],
            path: Vec::new(),
            alternate: Vec::new(),
            window: false,
            sight_epoch: None,
            born: false,
            like_today: false,
            touched: 0,
            published: (0, [0; 2], 0),
            needs_today: None,
            warnings: Vec::new(),
            warned: None,
            product_hour: None,
            head: None,
        })
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn warn(&mut self) {
        let refused = self.refused.clone().filter(|_| self.like_today && self.columns.is_some());
        if self.warned.as_ref().is_some_and(|(seq, held)| (*seq, held) == (self.published.0, &refused)) {
            return;
        }
        let mut report = ModelReport::default();
        for part in self.parts().into_values().flatten() {
            report.absorb(&part.report);
        }
        if let Some(why) = &refused {
            report.error(why.clone());
        }
        self.warnings = report.warnings();
        self.warned = Some((self.published.0, refused));
    }

    pub fn in_window(mut self) -> Self {
        self.window = true;
        self
    }

    pub fn stands_in(&self) -> bool {
        if self.columns.is_none() {
            return true;
        }
        match self.refused {
            None => self.needs_today.is_none(),
            Some(_) => self.like_today,
        }
    }

    pub fn needs_today(&self) -> Option<&str> {
        self.needs_today.as_deref()
    }

    pub fn seq(&self) -> u64 {
        self.published.0
    }

    pub fn handover(&self) -> (NavalSources, ModelReport) {
        let (sources, mut report) = self.sources();
        if let Some(why) = self.refused.as_deref().filter(|_| self.like_today && self.columns.is_some()) {
            report.error(why.to_owned());
        }
        (sources, report)
    }

    fn publish(&mut self) {
        let at = (self.marked[0], self.marked[1], self.touched);
        if self.dirty.len() > 0 || self.figures.len() > 0 || (self.published.1, self.published.2) == ([at.0, at.1], at.2) {
            return;
        }
        self.published = (self.published.0 + 1, [at.0, at.1], at.2);
        self.needs_today = self.today_needed();
    }

    fn today_needed(&self) -> Option<String> {
        self.by_id.values().map(|&slot| &self.forces[slot as usize]).filter(|held| held.admitted).find_map(|held| {
            let candidate = held.part.as_ref()?.candidate.as_ref()?;
            if matches!(candidate.slot, 9 | 10) {
                return Some(format!("a task force is in a naval combat in province {}", candidate.province));
            }
            (held.contested && candidate.slot != 19).then(|| format!("a land occupant shares province {} with a task force", candidate.province))
        })
    }

    pub fn set_source(&self, entry: Arc<Entry>, recording: Recording) -> io::Result<()> {
        let mut asked = self.rosters.shared.lock();
        if asked.source.as_ref().is_some_and(|(held, _)| Arc::ptr_eq(held, &entry)) {
            return Ok(());
        }
        let absent = viewer_layer_core::rebuilt::absent(&entry, &asked.tables);
        asked.refused.set_if_changed((!absent.is_empty()).then(|| viewer_layer_core::rebuilt::refusal("the ships' rosters", &absent)));
        asked.source = Some((entry, recording));
        asked.woken = true;
        drop(asked);
        self.rosters.shared.wake.notify_all();
        Ok(())
    }

    pub fn set_views(&self, store: &views::Store) {
        store.use_columns(TableId::VariantDefinitions);
        let reader = store.reader("ship_rosters", &[]);
        let mut asked = self.rosters.shared.lock();
        asked.views = Some(Arc::new(reader));
        asked.woken = true;
        drop(asked);
        self.rosters.shared.wake.notify_all();
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        *self.given.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((choice, vision));
    }

    pub fn perspective(&self) -> Option<&Choice> {
        self.vision.as_ref().map(|(choice, _)| choice)
    }

    pub fn rosters(&self) -> &Rosters {
        &self.rosters
    }

    pub fn drawn(&self) -> bool {
        self.columns.is_some()
    }

    pub fn head(&self) -> Option<Hour> {
        self.head
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    pub fn counts(&self) -> [usize; 2] {
        [self.by_id.len(), self.by_id.values().filter(|&&slot| self.forces[slot as usize].admitted).count()]
    }

    pub fn parts(&self) -> BTreeMap<u64, Option<&ForcePart>> {
        self.by_id.iter().filter(|(_, slot)| self.forces[**slot as usize].admitted).map(|(&id, &slot)| (id, self.forces[slot as usize].part.as_ref())).collect()
    }

    pub fn front(&self, size: f32) -> Option<([f32; 2], usize)> {
        let mut cells: HashMap<(i32, i32), usize> = HashMap::new();
        for part in self.parts().into_values().flatten() {
            let Some(placement) = &part.position else { continue };
            let [x, _, z] = placement.position.at();
            *cells.entry(((x / size).floor() as i32, (z / size).floor() as i32)).or_default() += 1;
        }
        cells.into_iter().max_by_key(|&(cell, count)| (count, std::cmp::Reverse(cell))).map(|((x, z), count)| ([(x as f32 + 0.5) * size, (z as f32 + 0.5) * size], count))
    }

    pub fn sources(&self) -> (NavalSources, ModelReport) {
        let (mut sources, mut report) = (NavalSources::default(), ModelReport::default());
        for part in self.parts().into_values().flatten() {
            part.clone().into(&mut sources, &mut report);
        }
        (sources, report)
    }

    pub fn fresh(&self) -> io::Result<Self> {
        let mut fresh = Self::new(Arc::clone(&self.naval))?;
        (fresh.columns, fresh.camera, fresh.camera_dirty, fresh.vision, fresh.reuse) = (self.columns, self.camera, true, self.vision.clone(), true);
        for (&id, &slot) in &self.by_id {
            if let Some(roster) = &self.forces[slot as usize].roster {
                fresh.forces.resize(fresh.forces.len().max(slot as usize + 1), Held::default());
                fresh.forces[slot as usize].roster = Some(Arc::clone(roster));
                let _ = id;
            }
        }
        Ok(fresh)
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        let (mine, theirs) = (self.sources().0, other.sources().0);
        let (_, differing, first) = viewer_unit_models::unit_models::naval_differ(&mine, &theirs);
        let refused = u64::from(self.refused() != other.refused());
        (differing + refused, first.or_else(|| (refused > 0).then(|| format!("refused {:?} against {:?}", self.refused(), other.refused()))))
    }

    fn columns(&self) -> Columns {
        self.columns.expect("the ships layer is declared before it settles")
    }

    fn admits(&self, province: u32) -> bool {
        self.admission.as_ref().is_none_or(|admission| admission.admits_province(self.naval.bounds(), province))
    }

    fn nears(&self, province: u32) -> bool {
        self.nearby.as_ref().is_none_or(|nearby| nearby.admits_province(self.naval.bounds(), province))
    }

    fn readmit(&mut self) {
        self.camera_dirty = false;
        self.touched += 1;
        let (size, defines) = self.naval.map();
        let camera = self.camera.filter(|(_, viewport)| *viewport != [0, 0]);
        self.admission = camera.map(|(placement, viewport)| Admission::new(placement.on(size, defines), viewport));
        self.nearby = camera.map(|(placement, viewport)| {
            let eye = placement.on(size, defines);
            Admission::padded(eye, viewport, eye.height)
        });
        let held: Vec<u32> = self.by_id.values().copied().collect();
        let head = self.head;
        for slot in held {
            let at = slot as usize;
            let admitted = self.forces[at].force.is_some_and(|force| self.admits(force.province));
            self.came_into_view(at, admitted, head);
            if admitted && self.forces[at].part.is_none() {
                self.figures.mark(slot, self.frame);
            }
        }
        let warming: Vec<u64> = self
            .by_id
            .iter()
            .filter(|&(_, &slot)| {
                let held = &self.forces[slot as usize];
                !held.admitted && held.roster.is_none() && !held.asked && held.force.is_some_and(|force| self.nears(force.province))
            })
            .map(|(&id, _)| id)
            .collect();
        for id in warming {
            let slot = self.by_id[&id];
            self.forces[slot as usize].asked = true;
            self.marked[2] += 1;
            self.rosters.ask([id]);
        }
    }

    fn came_into_view(&mut self, at: usize, admitted: bool, head: Option<Hour>) {
        let held = &mut self.forces[at];
        held.in_view_at = match (held.admitted && held.in_view_at.is_some(), admitted) {
            (_, false) => None,
            (true, true) => held.in_view_at,
            (false, true) => head,
        };
        held.admitted = admitted;
    }

    fn mark_force(&mut self, slot: u32) {
        if self.dirty.mark(slot, self.frame) {
            self.marked[0] += 1;
        }
    }

    fn mark_force_id(&mut self, mirrors: &Mirrors, id: u64) {
        if let Some(slot) = mirrors.get(TableId::TaskForces).and_then(|forces| forces.slot(u128::from(id))) {
            self.mark_force(slot);
        }
    }

    fn stale_roster(&mut self, mirrors: &Mirrors, id: u64) {
        if let Some(slot) = mirrors.get(TableId::TaskForces).and_then(|forces| forces.slot(u128::from(id))) {
            if self.forces.len() <= slot as usize {
                self.forces.resize(slot as usize + 1, Held::default());
            }
            let held = &mut self.forces[slot as usize];
            (held.roster, held.asked) = (None, true);
            self.marked[2] += 1;
            self.rosters.ask([id]);
            self.mark_force(slot);
        }
    }

    fn check(&self, mirrors: &Mirrors) -> Result<(), String> {
        if self.vision.is_none() {
            return Err("The ships layer is refused: it is given no vision of the viewing country yet".into());
        }
        for table in [TableId::CountryLinks, TableId::Countries] {
            if let Some(why) = mirrors.get(table).ok_or(format!("{table:?} is not read yet"))?.not_recorded() {
                return Err(format!("{why}; the core tables carry the political map itself, so this hour cannot be drawn"));
            }
        }
        for table in [TableId::ShipModels, TableId::Ships, TableId::TaskForces] {
            if complete(mirrors.get(table)).is_none() {
                return Err(format!("No ship is drawn: {table:?} is not wholly recorded this hour"));
            }
        }
        Ok(())
    }

    fn mark_all(&mut self, mirrors: &Mirrors) {
        self.all_dirty = false;
        self.touched += 1;
        self.by_id.clear();
        self.in_province.clear();
        self.dirty.clear();
        self.figures.clear();
        for held in &mut self.forces {
            (held.force, held.part, held.admitted) = (None, None, false);
        }
        self.camera_dirty = true;
        self.statuses = mirrors.iter().filter_map(|mirror| mirror.status().map(|status| (mirror.table().index(), status))).collect();
        if let Some(forces) = mirrors.get(TableId::TaskForces) {
            for slot in forces.rows() {
                self.dirty.mark(slot, self.frame);
            }
        }
    }

    fn all_rosters(&mut self, mirrors: &Mirrors) {
        self.rosters_dirty = false;
        self.ship_force.clear();
        let columns = self.columns();
        if let Some(ships) = mirrors.get(TableId::Ships) {
            for slot in ships.rows() {
                if let (Some(ship), Some(force)) = (ships.key(slot), ships.number(columns.task_force, slot).value()) {
                    self.ship_force.insert(ship as u64, force);
                }
            }
        }
        if !self.reuse {
            for held in &mut self.forces {
                (held.roster, held.asked) = (None, false);
            }
            self.rosters.forget_all();
        }
        self.all_dirty = true;
    }

    fn read_force(&mut self, slot: u32, mirrors: &Mirrors) {
        let columns = self.columns();
        let at = slot as usize;
        if self.forces.len() <= at {
            self.forces.resize(at + 1, Held::default());
        }
        let row = mirrors.get(TableId::TaskForces).filter(|forces| forces.live(slot)).and_then(|forces| {
            let number = |column: Num<u32>| forces.number(column, slot).value().unwrap_or(0);
            Some(Force { id: forces.key(slot)? as u64, province: number(columns.province), logical_owner: number(columns.logical_owner), roster_owner: number(columns.roster_owner), expeditionary_owner: number(columns.expeditionary_owner) })
        });
        let held = &self.forces[at];
        let mut gone: Vec<u64> = Vec::new();
        if let Some(old) = held.force {
            if row.is_none_or(|new| new.province != old.province) {
                if let Some(list) = self.in_province.get_mut(&old.province) {
                    list.retain(|&id| id != old.id);
                }
            }
            if row.is_none_or(|new| new.id != old.id) {
                self.by_id.remove(&old.id);
                gone.push(old.id);
            }
        }
        if let Some(new) = row {
            if held.force.is_none_or(|old| old.province != new.province || old.id != new.id) {
                self.in_province.entry(new.province).or_default().push(new.id);
            }
            if held.force.is_some_and(|old| old.roster_owner != new.roster_owner || old.expeditionary_owner != new.expeditionary_owner || old.id != new.id) {
                gone.push(new.id);
            }
            self.by_id.insert(new.id, slot);
        }
        if !gone.is_empty() {
            (self.forces[at].roster, self.forces[at].asked) = (None, false);
            self.rosters.forget(gone);
        }
        let admitted = row.is_some_and(|force| force.province != 0 && self.admits(force.province));
        let head = self.head;
        self.came_into_view(at, admitted, head);
        let held = &mut self.forces[at];
        (held.force, held.part) = (row, None);
        if admitted {
            if held.roster.is_none() && !held.asked {
                held.asked = true;
                self.marked[2] += 1;
                if let Some(force) = row {
                    self.rosters.ask([force.id]);
                }
            }
            self.figures.mark(slot, self.frame);
        }
    }

    fn place(&mut self, slot: u32, mirrors: &Mirrors) {
        let columns = self.columns();
        let at = slot as usize;
        let Some(held) = self.forces.get(at).filter(|held| held.admitted) else { return };
        let Some(force) = held.force else { return };
        let roster = held.roster.clone();
        let naval = Arc::clone(&self.naval);
        let (path, alternate) = (&mut self.path, &mut self.alternate);
        let route = motion_route(complete(mirrors.get(TableId::TaskForceMapMotion)), &columns, force.id, path, alternate);
        let links = mirrors.get(TableId::CountryLinks);
        let link = |raw: u32| links.and_then(|links| links.slot(u128::from(raw))).and_then(|row| links?.number(columns.link_slot, row).value());
        let inputs = skip_inputs(rows(mirrors.get(TableId::TaskForceCounterInputs)), &columns, force.id);
        let current = complete(mirrors.get(TableId::TaskForceMissions)).map(|missions| missions.slot(u128::from(force.id)).and_then(|row| missions.number(columns.current_region, row).value()).unwrap_or(0));
        let mission = mirrors.get(TableId::TaskForces).and_then(|forces| forces.number(columns.mission, slot).value()).unwrap_or(0);
        let region = naval.region(force.province);
        let skip = |kind| skip_of(kind, inputs, mission, current, region);
        let forces = mirrors.get(TableId::TaskForces);
        let mut contested = false;
        let ordinal = complete(mirrors.get(TableId::ProvinceUnitOrder)).and_then(|order| {
            let row = order.slot(u128::from(force.province))?;
            let Cell::Value(bytes) = order.list(columns.mixed, row) else { return None };
            let ids = || bytes.chunks_exact(8).map(|item| u64::from_le_bytes(item.try_into().expect("eight bytes")));
            contested = ids().any(|id| forces.is_none_or(|forces| forces.slot(u128::from(id)).is_none()));
            ids().position(|id| id == force.id).map(|at| at as u32)
        });
        let clock = complete(mirrors.get(TableId::Clock));
        let speed = clock.and_then(|clock| clock.rows().next().and_then(|row| clock.number(columns.speed, row).value()));
        let day = mirrors.get(TableId::TaskForces).and_then(Mirror::head).map_or(0, |hour| (hour - 43_800_000) / 24);
        let vision = &self.vision.as_ref().expect("a layer places with a vision").1;
        let mut waiting = false;
        let part = naval.figure(&force, route, &link, vision, &skip, ordinal, speed, day, &mut || match &roster {
            Some(roster) => (**roster).clone(),
            None => {
                waiting = true;
                None
            }
        });
        self.forces[at].contested = contested;
        if waiting {
            return;
        }
        self.marked[1] += 1;
        self.forces[at].part = Some(part);
    }

    fn settle_dirty(&mut self, mirrors: &Mirrors) {
        self.settle_marked(mirrors);
        self.head = mirrors.get(TableId::TaskForces).and_then(Mirror::head);
        let stood = self.product_hour.is_some() || (self.marks() == 0 && self.waiting() == 0);
        if self.columns.is_some() && self.refused.is_none() && stood {
            self.product_hour = self.standing().or(self.product_hour);
        }
        self.publish();
        self.warn();
    }

    fn standing(&self) -> Option<Hour> {
        let head = self.head?;
        let rows = self.rosters.answered().map_or(head, |answered| answered.min(head));
        let mut stands = rows;
        for (force, at) in self.rosters.owed() {
            let Some(held) = self.by_id.get(&force).map(|&slot| &self.forces[slot as usize]).filter(|held| held.admitted) else { continue };
            let from = held.in_view_at.map_or(at, |seen| at.max(seen));
            if from > Hour::MIN {
                stands = stands.min(from - 1);
            }
        }
        Some(stands)
    }

    fn marks(&self) -> usize {
        self.dirty.len() + self.figures.len() + usize::from(self.all_dirty) + usize::from(self.rosters_dirty)
    }

    fn waiting(&self) -> usize {
        self.by_id.values().filter(|&&slot| self.forces[slot as usize].admitted && self.forces[slot as usize].part.is_none()).count()
    }

    fn settle_marked(&mut self, mirrors: &Mirrors) {
        self.frame += 1;
        if self.columns.is_none() {
            return;
        }
        let given = self.given.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let renewed = given.is_some();
        if given.is_some() {
            self.vision = given;
        }
        if let Err(why) = self.check(mirrors) {
            if self.refused.as_ref() != Some(&why) {
                self.mark_all(mirrors);
                self.refused = Some(why);
                self.like_today = self.vision.is_some();
            }
            self.dirty.clear();
            self.figures.clear();
            return;
        }
        if self.refused.take().is_some() || renewed {
            (self.all_dirty, self.like_today) = (true, false);
        }
        if self.rosters_dirty {
            self.all_rosters(mirrors);
        }
        if self.all_dirty {
            self.mark_all(mirrors);
        }
        if self.camera_dirty {
            self.readmit();
        }
        if let Some(hour) = mirrors.get(TableId::TaskForces).and_then(Mirror::head) {
            self.rosters.applied(hour);
        }
        let day = mirrors.get(TableId::TaskForces).and_then(Mirror::head).map(|hour| (hour - 43_800_000) / 24);
        if day != self.day {
            self.day = day;
            self.touched += 1;
            for part in self.forces.iter_mut().filter_map(|held| held.part.as_mut()) {
                if let (Some(candidate), Some(day)) = (part.candidate.as_mut(), day) {
                    candidate.day = day;
                }
            }
        }
        for (force, roster) in self.rosters.take() {
            if let Some(slot) = mirrors.get(TableId::TaskForces).and_then(|forces| forces.slot(u128::from(force))) {
                if self.forces.len() <= slot as usize {
                    self.forces.resize(slot as usize + 1, Held::default());
                }
                let held = &mut self.forces[slot as usize];
                let changed = held.roster.as_deref() != Some(&*roster);
                (held.roster, held.asked) = (Some(roster), false);
                if changed || held.part.is_none() {
                    self.touched += u64::from(held.part.is_some());
                    held.part = None;
                    if held.admitted {
                        self.figures.mark(slot, self.frame);
                    }
                }
            }
        }
        let mut dirty = std::mem::take(&mut self.dirty);
        let _ = dirty.settle(|slot| {
            self.read_force(slot, mirrors);
            Ok(())
        });
        self.dirty = dirty;
        let mut figures = std::mem::take(&mut self.figures);
        let mut waiting = Vec::new();
        let _ = figures.settle(|slot| {
            self.place(slot, mirrors);
            if self.forces.get(slot as usize).is_some_and(|held| held.admitted && held.part.is_none()) {
                waiting.push(slot);
            }
            Ok(())
        });
        self.figures = figures;
        for slot in waiting {
            let held = &mut self.forces[slot as usize];
            if held.roster.is_none() && !held.asked {
                held.asked = true;
                self.marked[2] += 1;
                if let Some(force) = held.force {
                    self.rosters.ask([force.id]);
                }
            }
        }
    }

    fn take_vision(&mut self, upstream: &Upstream<'_>, mirrors: &Mirrors, always: bool) {
        use viewer_layer_core::vision::Vision as Layered;
        if !self.window || self.columns.is_none() {
            return;
        }
        let Some(vision) = upstream.get::<Layered>() else { return };
        let given = self.given.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some();
        let marked = match upstream.changed::<Layered>() {
            viewer_layer_core::layers::Change::Whole => None,
            viewer_layer_core::layers::Change::Marked(marked) => Some(marked),
        };
        let whole = always || marked.is_none() || (self.vision.is_none() && !given) || self.vision.as_ref().is_some_and(|(choice, _)| choice != vision.choice()) || self.sight_epoch != Some(vision.sight_epoch());
        let named: Vec<u32> = match marked.filter(|_| !whole) {
            None => Vec::new(),
            Some(marked) => marked.of("unit").iter().filter_map(|unit| self.by_id.get(unit).copied()).collect(),
        };
        if !whole && named.is_empty() && !self.born && !self.all_dirty {
            return;
        }
        let forces = mirrors.get(TableId::TaskForces);
        let ids = forces.into_iter().flat_map(|forces| forces.rows().filter_map(move |slot| forces.key(slot)).map(|key| key as u64));
        let Some(seen) = vision.unit_vision(ids) else { return };
        (self.sight_epoch, self.born) = (Some(vision.sight_epoch()), false);
        if whole {
            self.set_vision(vision.choice().clone(), Arc::new(seen));
        } else {
            self.vision = Some((vision.choice().clone(), Arc::new(seen)));
            named.into_iter().for_each(|slot| self.mark_force(slot));
        }
    }

    fn by_force(changes: &playhead::changes::Changes, mirror: &Mirror, read: &[usize]) -> Vec<u64> {
        let mut ids: Vec<u64> = changes.deleted().keys().unwrap_or_default().iter().chain(changes.inserted().keys().unwrap_or_default()).map(|&key| key as u64).collect();
        for written in changes.columns().iter().filter(|written| read.contains(&written.column())) {
            ids.extend(written.slots().iter().filter_map(|&slot| mirror.key(slot)).map(|key| key as u64));
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

impl Layer for Ships {
    fn retirement(&self) -> Option<Retirement> {
        let shared = Arc::clone(&self.rosters.shared);
        Some(Box::new(move || shared.stop()))
    }

    fn name(&self) -> &'static str {
        "ships"
    }

    fn placed(&mut self, view: &View) -> Option<u64> {
        let camera = view.camera.map(|camera| (camera, view.viewport));
        if camera != self.camera {
            self.camera = camera;
            self.camera_dirty = true;
        }
        Some(u64::from(view.camera.is_none_or(|camera| self.naval.drawn_at(camera.height))))
    }

    fn views(&mut self, store: &viewer_layer_core::playhead_host::views::Store) -> io::Result<()> {
        self.set_views(store);
        Ok(())
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let camera = view.camera.map(|camera| (camera, view.viewport));
        if camera != self.camera {
            self.camera = camera;
            self.camera_dirty = true;
        }
        let drawn = view.camera.is_none_or(|camera| self.naval.drawn_at(camera.height));
        self.rosters.set_open(drawn);
        if !drawn {
            self.columns = None;
            self.refused = Some("no ship model is drawn at this view".into());
            self.forces = Vec::new();
            self.by_id = BTreeMap::new();
            self.ship_force = HashMap::new();
            self.in_province = HashMap::new();
            self.dirty = Dirty::default();
            self.figures = Dirty::default();
            self.touched += 1;
            self.publish();
            self.warn();
            return Ok(());
        }
        if self.window {
            inputs.after("vision");
        }
        for table in [TableId::TaskForces, TableId::TaskForceMapMotion, TableId::TaskForceCounterInputs, TableId::TaskForceMissions, TableId::ProvinceUnitOrder, TableId::Ships, TableId::ShipModels, TableId::Countries, TableId::CountryLinks, TableId::Clock] {
            inputs.table(table, true)?;
        }
        let model = ["variant", "hull_sprite", "fallback_sprite", "model_number", "formation_flags", "counter"];
        let mut columns = [0; 6];
        for (at, field) in model.into_iter().enumerate() {
            columns[at] = match field {
                "hull_sprite" | "fallback_sprite" => inputs.text(TableId::ShipModels, field)?.column,
                "counter" => inputs.list(TableId::ShipModels, field)?.column,
                "variant" | "formation_flags" => inputs.number::<u64>(TableId::ShipModels, field)?.column,
                _ => inputs.number::<i32>(TableId::ShipModels, field)?.column,
            };
        }
        self.columns = Some(Columns {
            province: inputs.number(TableId::TaskForces, "province")?,
            logical_owner: inputs.number(TableId::TaskForces, "logical_owner")?,
            roster_owner: inputs.number(TableId::TaskForces, "roster_owner")?,
            expeditionary_owner: inputs.number(TableId::TaskForces, "expeditionary_owner")?,
            mission: inputs.number(TableId::TaskForces, "mission")?,
            target: inputs.number(TableId::TaskForceMapMotion, "target")?,
            combat_role: inputs.number(TableId::TaskForceMapMotion, "combat_role")?,
            path: inputs.list(TableId::TaskForceMapMotion, "path")?,
            alternate_path: inputs.list(TableId::TaskForceMapMotion, "alternate_path")?,
            is_in_regions: inputs.number(TableId::TaskForceCounterInputs, "is_in_regions")?,
            bombardment_region: inputs.number(TableId::TaskForceCounterInputs, "bombardment_region")?,
            current_region: inputs.number(TableId::TaskForceMissions, "current_region")?,
            mixed: inputs.list(TableId::ProvinceUnitOrder, "mixed")?,
            task_force: inputs.number(TableId::Ships, "task_force")?,
            class_id: inputs.number(TableId::Ships, "class_id")?,
            model: columns,
            explicit_entity: inputs.watch(TableId::VariantDefinitions, "explicit_entity")?,
            tag: inputs.text(TableId::Countries, "tag")?,
            link_slot: inputs.number(TableId::CountryLinks, "canonical_slot")?,
            speed: inputs.number(TableId::Clock, "speed")?,
        });
        Ok(())
    }

    fn viewed(&mut self, _view: &View, _mirrors: &Mirrors) {
        if self.columns.is_some() && self.camera_dirty && self.refused.is_none() {
            self.readmit();
        }
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if self.columns.is_none() {
            return Ok(());
        }
        let marked = self.marked;
        (self.all_dirty, self.rosters_dirty, self.like_today) = (true, true, false);
        self.refused = Some("composing".into());
        self.settle_dirty(mirrors);
        self.marked = marked;
        Ok(())
    }

    fn compose_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<()> {
        self.take_vision(upstream, mirrors, true);
        self.compose(mirrors)
    }

    fn ask(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<bool> {
        self.take_vision(upstream, mirrors, false);
        self.settle_dirty(mirrors);
        Ok(true)
    }

    fn join(&mut self, mirrors: &Mirrors, _upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        timeline::mark(self.rosters.maker, Mark::JoinBegan);
        if self.window
            && let Some(hour) = mirrors.get(TableId::TaskForces).and_then(Mirror::head)
        {
            let answered = self.rosters.wait(hour, settling.waiting(Duration::from_secs(30)));
            timeline::mark(self.rosters.maker, Mark::Waited);
            match answered {
                Ok(()) => self.settle_dirty(mirrors),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(_) => {}
            }
        }
        timeline::mark(self.rosters.maker, Mark::Joined);
        Ok(())
    }

    fn settle_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        self.ask(mirrors, upstream)?;
        self.join(mirrors, upstream, settling)
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        self.rosters.applied(step.last());
        if step.gap() || self.columns.is_none() {
            return;
        }
        let columns = self.columns();
        for changes in step.tables() {
            let table = TableId::ALL[changes.table()];
            let Some(mirror) = mirrors.get(table) else { continue };
            if changes.status().is_some_and(|(status, _)| self.statuses.insert(changes.table(), status) != Some(status)) {
                self.all_dirty = true;
                if matches!(table, TableId::Ships | TableId::ShipModels | TableId::VariantDefinitions | TableId::Countries | TableId::CountryLinks) {
                    self.rosters_dirty = true;
                }
                continue;
            }
            let any = |read: &[usize]| !changes.deleted().is_empty() || !changes.inserted().is_empty() || changes.columns().iter().any(|written| !written.is_empty() && read.contains(&written.column()));
            match table {
                TableId::TaskForces => {
                    let read = [columns.province.column, columns.logical_owner.column, columns.roster_owner.column, columns.expeditionary_owner.column, columns.mission.column];
                    let slots: Vec<u32> = changes.deleted().slots().iter().chain(changes.inserted().slots()).copied().chain(changes.columns().iter().filter(|written| read.contains(&written.column())).flat_map(|written| written.slots().iter().copied())).collect();
                    slots.into_iter().for_each(|slot| self.mark_force(slot));
                    let born: Vec<u64> = changes.inserted().keys().unwrap_or_default().iter().map(|&key| key as u64).collect();
                    self.born |= !born.is_empty();
                    born.into_iter().for_each(|id| self.stale_roster(mirrors, id));
                }
                TableId::TaskForceMapMotion => {
                    let read = [columns.target.column, columns.combat_role.column, columns.path.column, columns.alternate_path.column];
                    Self::by_force(changes, mirror, &read).into_iter().for_each(|id| self.mark_force_id(mirrors, id));
                }
                TableId::TaskForceCounterInputs => {
                    let read = [columns.is_in_regions.column, columns.bombardment_region.column];
                    Self::by_force(changes, mirror, &read).into_iter().for_each(|id| self.mark_force_id(mirrors, id));
                }
                TableId::TaskForceMissions => Self::by_force(changes, mirror, &[columns.current_region.column]).into_iter().for_each(|id| self.mark_force_id(mirrors, id)),
                TableId::ProvinceUnitOrder => {
                    for province in Self::by_force(changes, mirror, &[columns.mixed.column]) {
                        let forces = self.in_province.get(&(province as u32)).cloned().unwrap_or_default();
                        forces.into_iter().for_each(|id| self.mark_force_id(mirrors, id));
                    }
                }
                TableId::Ships => {
                    for ship in Self::by_force(changes, mirror, &[columns.task_force.column, columns.class_id.column]) {
                        let now = mirror.slot(u128::from(ship)).and_then(|slot| mirror.number(columns.task_force, slot).value());
                        let was = match now {
                            Some(force) => self.ship_force.insert(ship, force),
                            None => self.ship_force.remove(&ship),
                        };
                        for force in [was, now].into_iter().flatten() {
                            self.stale_roster(mirrors, force);
                        }
                    }
                }
                TableId::ShipModels => {
                    for ship in Self::by_force(changes, mirror, &columns.model) {
                        if let Some(&force) = self.ship_force.get(&ship) {
                            self.stale_roster(mirrors, force);
                        }
                    }
                }
                TableId::VariantDefinitions if any(&[columns.explicit_entity]) => self.rosters_dirty = true,
                TableId::Countries if any(&[columns.tag.column]) => self.rosters_dirty = true,
                TableId::CountryLinks if any(&[columns.link_slot.column]) => self.rosters_dirty = true,
                TableId::Clock if any(&[columns.speed.column]) => self.all_dirty = true,
                _ => {}
            }
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        self.settle_dirty(mirrors);
        Ok(())
    }


    fn carried(&self) -> Carried {
        if self.columns.is_none() || self.refused.as_deref().is_some_and(|why| why != "composing") {
            return Carried::default();
        }
        Carried { entities: self.marks() }
    }

    fn published(&self) -> Published {
        if self.columns.is_none() || self.refused.as_deref().is_some_and(|why| why != "composing") {
            return Published::Current;
        }
        if self.waiting() > 0 {
            return self.product_hour.map_or(Published::Nothing, Published::At);
        }
        let drawn = self.by_id.values().any(|&slot| self.forces[slot as usize].admitted && self.forces[slot as usize].part.is_some());
        if !drawn {
            return Published::Current;
        }
        match (self.product_hour, self.head) {
            (Some(stands), Some(head)) if stands < head => Published::Cadence(stands),
            _ => Published::Current,
        }
    }

    fn made_at(&self, hour: Hour) -> bool {
        !self.window || self.columns.is_none() || self.refused.as_deref().is_some_and(|why| why != "composing")
            || (self.product_hour == Some(hour) && self.waiting() == 0 && self.rosters.shared.lock().made_at(hour))
    }

    fn held(&self) -> usize {
        let parts: usize = self.forces.iter().filter_map(|held| held.part.as_ref()).map(|part| std::mem::size_of::<ForcePart>() + part.candidate.as_ref().map_or(0, |candidate| candidate.route.capacity() * 4) + part.formation.as_ref().map_or(0, |formation| formation.roster.capacity() * std::mem::size_of::<viewer_unit_history::unit_history::naval::Hull>())).sum();
        self.forces.capacity() * std::mem::size_of::<Held>() + parts + self.by_id.len() * 40 + self.ship_force.capacity() * 24 + self.in_province.capacity() * 40 + self.dirty.bytes() + self.figures.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests;
