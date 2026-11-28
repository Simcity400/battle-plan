use replay_derive::{
    air_mission_counts::{Counts, MISSIONS, count_of, registration},
    air_region_derive::{Filter, Retained, last_pass},
};

use viewer_layer_core::{layers::{Carried, Settling, Dirty, Inputs, Layer, List, Num, Published, Retirement, RetirementFlag, View}, owned::opaque};
use viewer_layer_core::playhead_host::mirror::{Cell, Mirror, Mirrors};
use playhead::{Step, changes::Changes};
use replay_columnar::manifest::Status;
use replay_data::{
    BranchId, Hour,
    rows::{AirCombatRecord, AirSupplySummary, Field, NativeValue, Opaque, Recorded, TableId},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use viewer_counters::counters::CounterContent;
use viewer_counters_naval::air_missions::{
    AirPasses, AirValues, Ground,
    counts_withheld, ground_of, interception_gate, interception_received_of, region_alerts, supplied, supply_refusal,
};
use viewer_foundation::{mode_visibility::PriorityTable, perspective::{Choice, Vision}};

const WINDOW: i32 = 24;

const WHOLE: Duration = Duration::from_secs(60);
const TIMEOUT_UNKNOWN: &str = "NGame.AIR_LOG_TIMEOUT_HOURS is unavailable";
const ENABLED_UNKNOWN: &str = "Ground Crews writer certificate or enabled-byte read is unavailable";
const CENSUS: &str = "the regional combat census is incomplete";

pub type Passes = Arc<dyn AirPasses + Send + Sync>;

pub struct Content {
    timeout: Result<i32, String>,
    ground: Result<(i64, i64), String>,
}

impl Content {
    pub fn new(counters: &CounterContent) -> Self {
        Self { timeout: counters.air_log_timeout().map_err(str::to_owned), ground: counters.ground_crew_defines().map_err(str::to_owned) }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn of(timeout: Result<i32, String>, ground: Result<(i64, i64), String>) -> Self {
        Self { timeout, ground }
    }
}

#[derive(Clone, Copy)]
struct Columns {
    ordinal: Num<u32>,
    owner: Num<u32>,
    planes: Num<i32>,
    mission_mask: Num<u32>,
    mission_region: Num<u32>,
    gate: Num<u32>,
    own: List,
    foreign: List,
    enabled: Num<u32>,
    supply: List,
    supply_severity: Num<i32>,
    supply_needed: Num<i64>,
    supply_supplied: Num<i64>,
    supply_deficit_extra: Num<i64>,
    link_slot: Num<u32>,
    country_slot: Num<u32>,
    command_power: Num<i64>,
}

fn supply_of(crews: &Mirror, columns: Columns, slot: u32) -> Recorded<Option<AirSupplySummary>> {
    match (
        crews.number(columns.supply_severity, slot),
        crews.number(columns.supply_needed, slot),
        crews.number(columns.supply_supplied, slot),
        crews.number(columns.supply_deficit_extra, slot),
    ) {
        (
            Cell::Value(severity),
            Cell::Value(needed),
            Cell::Value(supplied),
            Cell::Value(deficit_extra),
        ) => Recorded::Value(Some(AirSupplySummary::from_parts(
            severity,
            needed,
            supplied,
            deficit_extra,
        ))),
        (Cell::Missing, Cell::Missing, Cell::Missing, Cell::Missing) => Recorded::Value(None),
        (Cell::Unrecorded, Cell::Unrecorded, Cell::Unrecorded, Cell::Unrecorded) => {
            opaque::<Option<AirSupplySummary>>(crews, columns.supply, slot)
                .unwrap_or(Recorded::NotRecorded)
        }
        _ => Recorded::NotRecorded,
    }
}

const TABLES: [TableId; 7] = [TableId::AirRegionalMissions, TableId::AirWings, TableId::AirMissionInputs, TableId::AirRegionCombat, TableId::AirGroundCrews, TableId::CountryLinks, TableId::Countries];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionValue {
    pub counts: Counts,
    pub alerts: u32,
    pub uncertain: Vec<(usize, &'static str)>,
    pub supply: Option<AirSupplySummary>,
    pub ground: Option<Result<Ground, String>>,
}

#[derive(Clone, Debug)]
struct Registration {
    order: (u32, u64),
    counted: Result<Option<(u32, i32, u32)>, String>,
}

struct Asker {
    send: mpsc::Sender<(BranchId, Hour, u32)>,
    shared: Arc<(Mutex<Answers>, Condvar)>,
    failed: Option<String>,
}

#[derive(Default)]
struct Answers {
    given: viewer_runtime::landed::Published<HashMap<(BranchId, Hour, u32), Result<bool, String>>>,
    asked: BTreeSet<(BranchId, Hour, u32)>,
}

impl Asker {
    fn new(passes: Passes) -> Self {
        let (send, take) = mpsc::channel::<(BranchId, Hour, u32)>();
        let shared: Arc<(Mutex<Answers>, Condvar)> = Arc::default();
        let held = Arc::clone(&shared);
        let spawned = std::thread::Builder::new().name("air mission passes".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            while let Ok(question) = take.recv() {
                let (branch, pass, slot) = question;
                let answer = passes.visited(branch, pass, slot);
                let (answers, signal) = &*held;
                let mut answers = answers.lock().unwrap_or_else(|error| error.into_inner());
                answers.asked.remove(&question);
                answers.given.insert(question, answer);
                signal.notify_all();
            }
        });
        let failed = spawned.err().map(|error| format!("the daily air pass worker could not start: {error}"));
        Self { send, shared, failed }
    }
}

type Records = (Arc<Vec<AirCombatRecord>>, Arc<Vec<AirCombatRecord>>);

pub struct AirMissionCounters {
    retiring: RetirementFlag,
    content: Arc<Content>,
    columns: Option<Columns>,
    refused: Option<String>,
    withheld: Option<String>,
    published: Option<Arc<AirValues>>,
    registrations: Vec<Option<Registration>>,
    failures: BTreeMap<(u32, u64), String>,
    wings: BTreeMap<u32, BTreeMap<(u32, u64), (i32, u32)>>,
    holders: BTreeMap<u32, BTreeSet<u128>>,
    records: HashMap<u128, (u32, Option<Records>)>,
    combat_changed: BTreeSet<u128>,
    ours_of: RefCell<HashMap<u32, bool>>,
    supply_complete: bool,
    supply_unrecorded: bool,
    supply_dirty: bool,
    power: Option<i64>,
    power_dirty: bool,
    values: BTreeMap<u32, RegionValue>,
    wake: BTreeMap<u32, Hour>,
    hourly: BTreeSet<u32>,
    registrations_dirty: Dirty,
    regions: Dirty,
    waiting: BTreeMap<(BranchId, Hour, u32), BTreeSet<u32>>,
    product_hour: Option<Hour>,
    hour: Option<Hour>,
    day: Option<Hour>,
    all: bool,
    hidden: bool,
    vision: Mutex<Option<(Choice, Arc<Vision>)>>,
    vision_new: AtomicBool,
    passes: Mutex<Option<(Passes, BranchId)>>,
    passes_new: AtomicBool,
    asker: Option<Asker>,
    statuses: HashMap<usize, Status>,
    frame: u64,
    marked: usize,
    phases: [(u64, u64); PHASES.len()],
}

pub const PHASES: [&str; 4] = ["registrations", "holders", "regions", "by the hour"];

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn complete(mirrors: &Mirrors, table: TableId) -> Option<&Mirror> {
    mirrors.get(table).filter(|mirror| mirror.complete())
}

fn not_whole(mirror: Option<&Mirror>, table: TableId, hour: Hour) -> Option<String> {
    let Some(mirror) = mirror else { return Some(format!("hour {hour} {table:?} is not read yet")) };
    match mirror.status() {
        Some(Status::Complete) => None,
        Some(Status::Truncated { missing }) => Some(format!("hour {hour} {table:?} is truncated ({missing} missing rows)")),
        Some(Status::Unavailable { .. }) => Some(format!("hour {hour} {table:?} is unavailable: {}", mirror.reason().unwrap_or_default())),
        Some(Status::NotRecorded) => Some(match mirror.capture() {
            Some(mode) => format!("hour {hour} {table:?} is not recorded in a {mode} recording"),
            None => format!("hour {hour} {table:?} is not recorded"),
        }),
        None => mirror.not_recorded(),
    }
}

fn records_of(mirror: &Mirror, column: List, slot: u32) -> Option<Arc<Vec<AirCombatRecord>>> {
    let bytes = mirror.list(column, slot).value()?;
    <Arc<Vec<AirCombatRecord>> as NativeValue>::from_field(Field::Opaque(Opaque::Encoded(bytes, mirror.format()))).ok()
}

impl AirMissionCounters {
    pub fn new(content: Arc<Content>) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            content,
            columns: None,
            refused: Some("not composed yet".into()),
            withheld: None,
            published: None,
            registrations: Vec::new(),
            failures: BTreeMap::new(),
            wings: BTreeMap::new(),
            holders: BTreeMap::new(),
            records: HashMap::new(),
            combat_changed: BTreeSet::new(),
            ours_of: RefCell::new(HashMap::new()),
            supply_complete: false,
            supply_unrecorded: false,
            supply_dirty: true,
            power: None,
            power_dirty: true,
            values: BTreeMap::new(),
            wake: BTreeMap::new(),
            hourly: BTreeSet::new(),
            registrations_dirty: Dirty::default(),
            regions: Dirty::default(),
            waiting: BTreeMap::new(),
            product_hour: None,
            hour: None,
            day: None,
            all: true,
            hidden: false,
            vision: Mutex::new(None),
            vision_new: AtomicBool::new(false),
            passes: Mutex::new(None),
            passes_new: AtomicBool::new(false),
            asker: None,
            statuses: HashMap::new(),
            frame: 0,
            marked: 0,
            phases: [(0, 0); PHASES.len()],
        }
    }

    pub fn fresh(&self) -> Self {
        let fresh = Self { columns: self.columns, hidden: self.hidden, ..Self::new(Arc::clone(&self.content)) };
        if let Some((choice, vision)) = self.perspective() {
            fresh.set_vision(choice, vision);
        }
        if let Some((passes, branch)) = lock(&self.passes).clone() {
            fresh.set_passes(passes, branch);
        }
        fresh
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        let mut held = lock(&self.vision);
        if held.as_ref().is_none_or(|(_, held)| held.ours() != vision.ours()) {
            self.vision_new.store(true, Ordering::Release);
        }
        *held = Some((choice, vision));
    }

    pub fn set_passes(&self, passes: Passes, branch: BranchId) {
        let mut held = lock(&self.passes);
        if held.as_ref().is_none_or(|(held, at)| !Arc::ptr_eq(held, &passes) || *at != branch) {
            self.passes_new.store(true, Ordering::Release);
        }
        *held = Some((passes, branch));
    }

    pub fn perspective(&self) -> Option<(Choice, Arc<Vision>)> {
        lock(&self.vision).clone()
    }

    pub fn passes_branch(&self) -> Option<BranchId> {
        lock(&self.passes).as_ref().map(|(_, branch)| *branch)
    }

    fn columns(&self) -> Columns {
        self.columns.expect("the air mission layer is declared before it composes")
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    pub fn withheld(&self) -> Option<&str> {
        self.withheld.as_deref()
    }

    pub fn values(&self) -> &BTreeMap<u32, RegionValue> {
        &self.values
    }

    pub fn published(&self) -> Option<&Arc<AirValues>> {
        self.published.as_ref().filter(|_| self.refused.is_none() && !self.hidden)
    }

    fn publish(&mut self) {
        if self.refused.is_some() || self.hidden {
            return;
        }
        let mut out = AirValues { withheld: self.withheld.clone(), supply_complete: self.supply_complete, supply_unrecorded: self.supply_unrecorded, ..AirValues::default() };
        if out.withheld.is_none() {
            for (&region, value) in &self.values {
                out.regions.insert(region, value.counts);
                out.alerts.extend((0..MISSIONS).filter(|kind| value.alerts & (1 << kind) != 0).map(|kind| (region, kind)));
                out.uncertain.extend(value.uncertain.iter().map(|&(kind, reason)| ((region, kind), reason)));
                if let Some(supply) = value.supply {
                    out.supply.insert(region, supply);
                }
                if let Some(ground) = &value.ground {
                    out.ground.insert(region, ground.clone());
                }
            }
        }
        if self.published.as_deref() != Some(&out) {
            self.published = Some(Arc::new(out));
        }
    }

    pub fn supply(&self) -> (bool, bool) {
        (self.supply_complete, self.supply_unrecorded)
    }

    pub fn marked(&self) -> usize {
        self.marked
    }

    pub fn phases(&self) -> impl Iterator<Item = (&'static str, u64, u64)> + '_ {
        PHASES.iter().zip(&self.phases).map(|(&name, &(entities, ns))| (name, entities, ns))
    }

    pub fn held_regions(&self) -> usize {
        self.holders.values().filter(|keys| !keys.is_empty()).count()
    }

    fn mark_region(&mut self, region: u32) {
        if self.regions.mark(region, self.frame) {
            self.marked += 1;
        }
    }

    fn mark_all_regions(&mut self) {
        let regions: BTreeSet<u32> = self.values.keys().chain(self.wings.keys()).copied().collect();
        regions.into_iter().for_each(|region| self.mark_region(region));
        self.supply_dirty = true;
    }

    fn ours(&self) -> u32 {
        self.perspective().map_or(0, |(_, vision)| vision.ours())
    }

    fn is_ours(&self, links: Option<&Mirror>, raw: u32, ours: u32) -> bool {
        *self.ours_of.borrow_mut().entry(raw).or_insert_with(|| {
            let columns = self.columns();
            links.and_then(|links| links.number(columns.link_slot, links.slot(u128::from(raw))?).value()) == Some(ours)
        })
    }

    fn recompose(&mut self, mirrors: &Mirrors) {
        self.registrations.clear();
        self.failures.clear();
        self.wings.clear();
        self.holders.clear();
        self.records.clear();
        self.combat_changed.clear();
        self.ours_of.borrow_mut().clear();
        self.values.clear();
        self.wake.clear();
        self.hourly.clear();
        self.registrations_dirty.clear();
        self.regions.clear();
        self.waiting.clear();
        (self.supply_complete, self.supply_unrecorded, self.power) = (false, false, None);
        self.supply_dirty = true;
        self.power_dirty = true;
        self.hour = None;
        self.day = None;
        self.statuses = mirrors.iter().filter_map(|mirror| mirror.status().map(|status| (mirror.table().index(), status))).collect();
        if let Some(registrations) = mirrors.get(TableId::AirRegionalMissions) {
            let slots: Vec<u32> = registrations.rows().collect();
            slots.into_iter().for_each(|slot| {
                self.registrations_dirty.mark(slot, self.frame);
            });
        }
        if let Some(combat) = mirrors.get(TableId::AirRegionCombat) {
            let (links, ours) = (mirrors.get(TableId::CountryLinks), self.ours());
            let keys: Vec<u128> = combat.rows().filter_map(|slot| combat.key(slot)).filter(|key| self.is_ours(links, (key >> 32) as u32, ours)).collect();
            self.combat_changed.extend(keys);
        }
    }

    fn release(&mut self) {
        let empty = Self::new(Arc::clone(&self.content));
        let kept = std::mem::replace(self, empty);
        self.retiring = kept.retiring;
        (self.columns, self.hidden, self.frame, self.marked, self.phases) = (kept.columns, kept.hidden, kept.frame, kept.marked, kept.phases);
        (self.vision, self.vision_new, self.passes, self.passes_new) = (kept.vision, kept.vision_new, kept.passes, kept.passes_new);
    }

    fn core(&self, mirrors: &Mirrors) -> Result<u32, String> {
        let (_, vision) = self.perspective().ok_or("the viewing country's vision is not set")?;
        for table in TABLES {
            if mirrors.get(table).is_none() {
                return Err(format!("{table:?} is not read yet"));
            }
        }
        Ok(vision.ours())
    }

    fn settle_registration(&mut self, mirrors: &Mirrors, slot: u32, ours: u32) {
        let at = slot as usize;
        if self.registrations.len() <= at {
            self.registrations.resize(at + 1, None);
        }
        if let Some(old) = self.registrations[at].take() {
            match old.counted {
                Err(_) => {
                    self.failures.remove(&old.order);
                }
                Ok(Some((region, _, _))) => {
                    if let Some(wings) = self.wings.get_mut(&region) {
                        wings.remove(&old.order);
                        if wings.is_empty() {
                            self.wings.remove(&region);
                        }
                    }
                    self.mark_region(region);
                }
                Ok(None) => {}
            }
        }
        let columns = self.columns();
        let Some(registrations) = mirrors.get(TableId::AirRegionalMissions).filter(|mirror| mirror.live(slot)) else { return };
        let (Some(key), Some(ordinal)) = (registrations.key(slot), registrations.number(columns.ordinal, slot).value()) else { return };
        let wing = key as u64;
        let wings = mirrors.get(TableId::AirWings);
        let row = wings.and_then(|wings| {
            let at = wings.slot(key)?;
            Some((wings.number(columns.owner, at).value()?, wings.number(columns.mission_region, at).value(), wings.number(columns.planes, at).value()?, wings.number(columns.mission_mask, at).value()?))
        });
        let links = mirrors.get(TableId::CountryLinks);
        let slot_of = |raw: u32| links.and_then(|links| links.number(columns.link_slot, links.slot(u128::from(raw))?).value());
        let order = (ordinal, wing);
        let counted = registration(wing, row.map(|(owner, region, _, _)| (owner, region)), ours, slot_of).map(|region| {
            region.map(|region| {
                let (_, _, planes, mask) = row.expect("a counted registration has its wing");
                (region, planes, mask)
            })
        });
        match &counted {
            Err(reason) => {
                self.failures.insert(order, reason.clone());
            }
            Ok(Some((region, planes, mask))) => {
                self.wings.entry(*region).or_default().insert(order, (*planes, *mask));
                self.mark_region(*region);
            }
            Ok(None) => {}
        }
        self.registrations[at] = Some(Registration { order, counted });
    }

    fn settle_combat(&mut self, mirrors: &Mirrors, key: u128, ours: u32) {
        if let Some((region, _)) = self.records.remove(&key) {
            if let Some(keys) = self.holders.get_mut(&region) {
                keys.remove(&key);
            }
            self.mark_shown(region);
        }
        let Some(combat) = mirrors.get(TableId::AirRegionCombat) else { return };
        if combat.slot(key).is_none() || !self.is_ours(mirrors.get(TableId::CountryLinks), (key >> 32) as u32, ours) {
            return;
        }
        let region = key as u32;
        self.records.insert(key, (region, None));
        self.holders.entry(region).or_default().insert(key);
        self.mark_shown(region);
    }

    fn mark_shown(&mut self, region: u32) {
        if self.values.contains_key(&region) || self.wings.contains_key(&region) {
            self.mark_region(region);
        }
    }

    fn holder_records(&mut self, mirrors: &Mirrors, region: u32) -> Option<Records> {
        let columns = self.columns();
        let key = *self.holders.get(&region)?.first()?;
        let (_, held) = self.records.get_mut(&key)?;
        if held.is_none() {
            let combat = mirrors.get(TableId::AirRegionCombat)?;
            let slot = combat.slot(key)?;
            *held = Some((records_of(combat, columns.own, slot).unwrap_or_default(), records_of(combat, columns.foreign, slot).unwrap_or_default()));
            self.phases[1].0 += 1;
        }
        held.clone()
    }

    fn settle_globals(&mut self, mirrors: &Mirrors, ours: u32) {
        let columns = self.columns();
        if std::mem::take(&mut self.supply_dirty) {
            let (complete, unrecorded) = match complete(mirrors, TableId::AirGroundCrews) {
                Some(crews) => {
                    let unrecorded = crews.rows().any(|slot| matches!(supply_of(crews, columns, slot), Recorded::NotRecorded));
                    let complete = crews.slot(u128::from(ours) << 32 | u128::from(u32::MAX)).is_some_and(|slot| matches!(supply_of(crews, columns, slot), Recorded::Value(Some(_))));
                    (complete, unrecorded)
                }
                None => (false, false),
            };
            if (complete, unrecorded) != (self.supply_complete, self.supply_unrecorded) {
                (self.supply_complete, self.supply_unrecorded) = (complete, unrecorded);
                let regions: BTreeSet<u32> = self.values.keys().chain(self.wings.keys()).copied().collect();
                regions.into_iter().for_each(|region| self.mark_region(region));
                self.supply_regions(mirrors, ours);
            }
        }
        if std::mem::take(&mut self.power_dirty) {
            let power = complete(mirrors, TableId::Countries).and_then(|countries| countries.rows().find(|&slot| countries.number(columns.country_slot, slot).value() == Some(ours)).and_then(|slot| countries.number(columns.command_power, slot).value()));
            if power != self.power {
                self.power = power;
                let regions: Vec<u32> = self.values.keys().copied().collect();
                regions.into_iter().for_each(|region| self.mark_region(region));
            }
        }
    }

    fn supply_regions(&mut self, mirrors: &Mirrors, ours: u32) {
        let Some(crews) = mirrors.get(TableId::AirGroundCrews) else { return };
        let regions: Vec<u32> = crews.rows().filter_map(|slot| crews.key(slot)).filter(|key| (key >> 32) as u32 == ours && *key as u32 != u32::MAX).map(|key| key as u32).collect();
        regions.into_iter().for_each(|region| self.mark_region(region));
    }

    fn known_answer(&self, pass: Hour, ours: u32) -> Option<Result<bool, String>> {
        let (_, branch) = lock(&self.passes).clone()?;
        let asker = self.asker.as_ref()?;
        lock(&asker.shared.0).given.get_entry(&(branch, pass, ours)).cloned()
    }

    fn ask(&mut self, pass: Hour, ours: u32, region: u32) {
        let Some((passes, branch)) = lock(&self.passes).clone() else { return };
        let question = (branch, pass, ours);
        let asker = self.asker.get_or_insert_with(|| Asker::new(passes));
        let mut answers = lock(&asker.shared.0);
        if answers.given.contains_key(&question) {
            return;
        }
        if let Some(why) = &asker.failed {
            answers.given.insert(question, Err(why.clone()));
            drop(answers);
            self.waiting.entry(question).or_default().insert(region);
            return;
        }
        if answers.asked.insert(question) {
            let _ = asker.send.send(question);
        }
        drop(answers);
        self.waiting.entry(question).or_default().insert(region);
    }

    fn take_answers(&mut self, settling: Settling) -> io::Result<()> {
        let Some(asker) = &self.asker else { return Ok(()) };
        let shared = Arc::clone(&asker.shared);
        let (answers, signal) = &*shared;
        let mut held = lock(answers);
        let until = Instant::now() + settling.waiting(WHOLE);
        while self.waiting.keys().any(|question| held.asked.contains(question)) {
            self.retiring.check()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            held = signal.wait_timeout(held, left.min(RetirementFlag::POLL)).unwrap_or_else(|error| error.into_inner()).0;
        }
        let answered: Vec<(BranchId, Hour, u32)> = self.waiting.keys().filter(|question| held.given.contains_key(*question)).copied().collect();
        drop(held);
        for question in answered {
            for region in self.waiting.remove(&question).unwrap_or_default() {
                self.mark_region(region);
            }
        }
        Ok(())
    }

    fn settle_region(&mut self, mirrors: &Mirrors, region: u32, ours: u32, hour: Hour) {
        let columns = self.columns();
        self.waiting.values_mut().for_each(|regions| {
            regions.remove(&region);
        });
        self.waiting.retain(|_, regions| !regions.is_empty());
        let wings: Vec<((u32, u64), (i32, u32))> = self.wings.get(&region).map(|wings| wings.iter().map(|(&order, &values)| (order, values)).collect()).unwrap_or_default();
        let mut counts: Option<Counts> = None;
        for &(_, (planes, mask)) in &wings {
            count_of(counts.get_or_insert([-1; MISSIONS]), planes, mask);
        }
        let crews = complete(mirrors, TableId::AirGroundCrews);
        let crew_row = crews.and_then(|crews| Some((crews, crews.slot(u128::from(ours) << 32 | u128::from(region))?)));
        let mut supply = None;
        if self.supply_complete
            && let Some((crews, slot)) = crew_row
            && let Recorded::Value(Some(summary)) = supply_of(crews, columns, slot)
        {
            supply = Some(summary);
            supplied(&mut counts, &summary);
        }
        self.wake.remove(&region);
        let Some(counts) = counts else {
            self.values.remove(&region);
            self.hourly.remove(&region);
            return;
        };
        let combat_whole = mirrors.get(TableId::AirRegionCombat).is_some_and(Mirror::complete);
        let holder = if combat_whole { self.holder_records(mirrors, region) } else { None };
        let gates = complete(mirrors, TableId::AirMissionInputs);
        let gate = |wing: u64| -> Option<bool> {
            let gates = gates?;
            let slot = gates.slot(u128::from(wing))?;
            let recorded = match gates.number(columns.gate, slot) {
                Cell::Value(gate) => Recorded::Value(Some(gate)),
                Cell::Missing => Recorded::Value(None),
                Cell::Unrecorded => Recorded::NotRecorded,
            };
            interception_gate(&recorded)
        };
        let timeout = self.content.timeout.as_ref().map(|&timeout| timeout).map_err(|_| TIMEOUT_UNKNOWN);
        let entries: Vec<(u64, u32)> = wings.iter().map(|&((_, wing), (_, mask))| (wing, mask)).collect();
        let pass = last_pass(hour);
        let known = pass.and_then(|pass| self.known_answer(pass, ours));
        let needed = std::cell::Cell::new(false);
        let alerts = region_alerts(
            &counts,
            &entries,
            gate,
            |kind, classification| {
                if !combat_whole {
                    return Err(CENSUS);
                }
                let Some((own, foreign)) = &holder else { return Ok(false) };
                interception_received_of(
                    Retained { own, foreign },
                    hour,
                    Filter { missions: 1 << kind, window: WINDOW, classification },
                    |_| {
                        needed.set(known.is_none());
                        known.clone()
                    },
                    timeout,
                )
            },
            self.supply_complete,
            supply_refusal(self.supply_unrecorded),
        );
        if needed.get()
            && let Some(pass) = pass
        {
            self.ask(pass, ours, region);
        }
        let ground = counts.iter().any(|&count| count >= 0).then(|| {
            let active = || -> Result<u32, String> {
                let crews = mirrors.get(TableId::AirGroundCrews).filter(|mirror| mirror.complete()).ok_or_else(|| not_whole(mirrors.get(TableId::AirGroundCrews), TableId::AirGroundCrews, hour).unwrap_or_default())?;
                let Some(slot) = crews.slot(u128::from(ours) << 32 | u128::from(region)) else { return Ok(0) };
                crews.number(columns.enabled, slot).value().ok_or_else(|| ENABLED_UNKNOWN.to_owned())
            };
            ground_of(self.content.ground.as_ref().map(|&(cost, _)| cost).map_err(Clone::clone), active, self.power)
        });
        if ground.is_some() && self.content.ground.is_ok() && !mirrors.get(TableId::AirGroundCrews).is_some_and(Mirror::complete) {
            self.hourly.insert(region);
        } else {
            self.hourly.remove(&region);
        }
        if let Some((own, foreign)) = &holder {
            let start = hour.wrapping_sub(WINDOW);
            if let Some(oldest) = own.iter().chain(foreign.iter()).map(|record| record.date).filter(|&date| date >= start).min() {
                self.wake.insert(region, oldest.wrapping_add(WINDOW + 1));
            }
        }
        self.values.insert(region, RegionValue { counts, alerts: alerts.alerts, uncertain: alerts.uncertain, supply, ground });
    }
}

impl AirMissionCounters {
    fn settle_dirty(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.frame += 1;
        let core = if self.hidden { Err("not drawn at this view".to_owned()) } else { self.core(mirrors) };
        let ours = match core {
            Ok(ours) => ours,
            Err(why) => {
                if self.refused.as_ref() != Some(&why) {
                    self.release();
                    self.refused = Some(why);
                }
                self.all = false;
                return Ok(());
            }
        };
        let renew = self.vision_new.swap(false, Ordering::AcqRel);
        if self.refused.take().is_some() || self.all || renew {
            self.all = false;
            self.passes_new.store(false, Ordering::Release);
            self.recompose(mirrors);
        }
        if self.passes_new.swap(false, Ordering::AcqRel) {
            self.asker = None;
            self.waiting.clear();
            let regions: Vec<u32> = self.values.keys().copied().collect();
            regions.into_iter().for_each(|region| self.mark_region(region));
        }
        let hour = mirrors.get(TableId::AirRegionalMissions).and_then(Mirror::head).unwrap_or_default();
        let began = std::time::Instant::now();
        let mut dirty = std::mem::take(&mut self.registrations_dirty);
        self.phases[0].0 += dirty.len() as u64;
        let _ = dirty.settle(|slot| {
            self.settle_registration(mirrors, slot, ours);
            Ok(())
        });
        self.registrations_dirty = dirty;
        self.phases[0].1 += began.elapsed().as_nanos() as u64;
        let began = std::time::Instant::now();
        let changed = std::mem::take(&mut self.combat_changed);
        self.phases[1].0 += changed.len() as u64;
        changed.into_iter().for_each(|key| self.settle_combat(mirrors, key, ours));
        self.phases[1].1 += began.elapsed().as_nanos() as u64;
        self.settle_globals(mirrors, ours);
        self.withheld = [TableId::CountryLinks, TableId::AirWings, TableId::AirRegionalMissions]
            .into_iter()
            .find_map(|table| not_whole(mirrors.get(table), table, hour))
            .or_else(|| self.failures.values().next().cloned())
            .map(|reason| counts_withheld(&reason));
        if Some(hour) != self.hour {
            let began = std::time::Instant::now();
            let day = last_pass(hour);
            let mut regions: Vec<u32> = if day != self.day {
                self.holders.iter().filter(|(region, keys)| !keys.is_empty() && self.values.contains_key(region)).map(|(&region, _)| region).collect()
            } else {
                self.wake.iter().filter(|&(_, &wake)| wake <= hour).map(|(&region, _)| region).collect()
            };
            regions.extend(self.hourly.iter().copied());
            self.phases[3].0 += regions.len() as u64;
            regions.into_iter().for_each(|region| self.mark_region(region));
            (self.hour, self.day) = (Some(hour), day);
            self.phases[3].1 += began.elapsed().as_nanos() as u64;
        }
        self.take_answers(settling)?;
        let began = std::time::Instant::now();
        let mut dirty = std::mem::take(&mut self.regions);
        let taken = dirty.len() as u64;
        let _ = dirty.settle(|region| {
            self.settle_region(mirrors, region, ours, hour);
            Ok(())
        });
        self.phases[2].0 += taken - dirty.len() as u64;
        self.regions = dirty;
        self.phases[2].1 += began.elapsed().as_nanos() as u64;
        if !self.waiting.is_empty() {
            self.settle_dirty_again(mirrors, ours, hour, settling)?;
        }
        if self.waiting.is_empty() && self.marks() == 0 {
            self.product_hour = Some(hour);
        }
        Ok(())
    }

    fn marks(&self) -> usize {
        self.regions.len() + self.registrations_dirty.len() + self.combat_changed.len() + usize::from(self.all)
    }

    fn settle_dirty_again(&mut self, mirrors: &Mirrors, ours: u32, hour: Hour, settling: Settling) -> io::Result<()> {
        while !self.waiting.is_empty() {
            let before = self.waiting.len();
            self.take_answers(settling)?;
            if self.waiting.len() == before && settling.waiting(WHOLE).is_zero() {
                break;
            }
            let mut dirty = std::mem::take(&mut self.regions);
            if dirty.len() == 0 {
                self.regions = dirty;
                break;
            }
            let _ = dirty.settle(|region| {
                self.settle_region(mirrors, region, ours, hour);
                Ok(())
            });
            self.regions = dirty;
        }
        Ok(())
    }

    fn apply_table(&mut self, table: TableId, changes: &Changes, mirrors: &Mirrors, columns: Columns) {
        let Some(mirror) = mirrors.get(table) else { return };
        let written = |changes: &Changes| changes.columns().iter().flat_map(|written| written.slots()).copied().collect::<Vec<u32>>();
        let lives = |changes: &Changes| changes.deleted().keys().unwrap_or_default().iter().chain(changes.inserted().keys().unwrap_or_default()).copied().collect::<Vec<u128>>();
        let keys = |changes: &Changes| {
            let mut keys = lives(changes);
            keys.extend(written(changes).into_iter().filter_map(|slot| mirror.key(slot)));
            keys
        };
        let registration_of = |key: u128| mirrors.get(TableId::AirRegionalMissions).and_then(|registrations| registrations.slot(key));
        match table {
            TableId::AirRegionalMissions => {
                for &slot in changes.deleted().slots().iter().chain(changes.inserted().slots()).chain(written(changes).iter()) {
                    self.registrations_dirty.mark(slot, self.frame);
                }
            }
            TableId::AirWings => {
                for key in keys(changes) {
                    if let Some(slot) = registration_of(key) {
                        self.registrations_dirty.mark(slot, self.frame);
                    }
                }
            }
            TableId::AirMissionInputs => {
                for key in keys(changes) {
                    let region = registration_of(key).and_then(|slot| self.registrations.get(slot as usize)?.as_ref()?.counted.as_ref().ok()?.map(|(region, _, _)| region));
                    if let Some(region) = region {
                        self.mark_region(region);
                    }
                }
            }
            TableId::AirRegionCombat => {
                let ours = self.ours();
                let links = mirrors.get(TableId::CountryLinks);
                for key in keys(changes) {
                    if self.records.contains_key(&key) || self.is_ours(links, (key >> 32) as u32, ours) {
                        self.combat_changed.insert(key);
                    }
                }
            }
            TableId::AirGroundCrews => {
                let ours = self.ours();
                let supply_columns = [
                    columns.supply.column,
                    columns.supply_severity.column,
                    columns.supply_needed.column,
                    columns.supply_supplied.column,
                    columns.supply_deficit_extra.column,
                ];
                let supply_written = changes
                    .columns()
                    .iter()
                    .any(|written| supply_columns.contains(&written.column()) && !written.is_empty());
                if supply_written || !changes.deleted().is_empty() || !changes.inserted().is_empty() {
                    self.supply_dirty = true;
                }
                for key in keys(changes) {
                    if (key >> 32) as u32 == ours {
                        match key as u32 {
                            u32::MAX => self.mark_all_regions(),
                            region => self.mark_region(region),
                        }
                    }
                }
            }
            TableId::Countries => self.power_dirty = true,
            TableId::CountryLinks if !changes.deleted().is_empty() || !changes.inserted().is_empty() || changes.columns().iter().any(|written| written.column() == columns.link_slot.column && !written.is_empty()) => {
                self.all = true;
            }
            _ => {}
        }
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        let mut differing = 0;
        let mut first = None;
        let mut note = |what: String| {
            differing += 1;
            first.get_or_insert(what);
        };
        if (self.refused(), self.withheld(), self.supply()) != (other.refused(), other.withheld(), other.supply()) {
            note(format!("refused, withheld, supply: {:?} against {:?}", (self.refused(), self.withheld(), self.supply()), (other.refused(), other.withheld(), other.supply())));
        }
        for region in self.values.keys().chain(other.values.keys()).collect::<BTreeSet<_>>() {
            if self.values.get(region) != other.values.get(region) {
                note(format!("region {region}: {:?} against {:?}", self.values.get(region), other.values.get(region)));
            }
        }
        (differing, first)
    }
}

impl Layer for AirMissionCounters {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn held_viewer(&self) -> Option<u32> {
        self.perspective().map(|(_, vision)| vision.ours())
    }

    fn name(&self) -> &'static str {
        "air_mission_counters"
    }

    fn placed(&mut self, _view: &viewer_layer_core::layers::View) -> Option<u64> {
        Some(0)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if viewer_map_modes::map_mode::visibility::of(&view.map_mode).priorities != PriorityTable::StrategicAir {
            self.hidden = true;
            return Ok(());
        }
        for table in TABLES {
            inputs.table(table, true)?;
        }
        use TableId as T;
        self.columns = Some(Columns {
            ordinal: inputs.number(T::AirRegionalMissions, "ordinal")?,
            owner: inputs.number(T::AirWings, "owner")?,
            planes: inputs.number(T::AirWings, "planes")?,
            mission_mask: inputs.number(T::AirWings, "mission_mask")?,
            mission_region: inputs.number(T::AirWings, "mission_region")?,
            gate: inputs.number(T::AirMissionInputs, "interception_alert_gate")?,
            own: inputs.list(T::AirRegionCombat, "own")?,
            foreign: inputs.list(T::AirRegionCombat, "foreign")?,
            enabled: inputs.number(T::AirGroundCrews, "enabled")?,
            supply: inputs.list(T::AirGroundCrews, "supply")?,
            supply_severity: inputs.number(T::AirGroundCrews, "supply_severity")?,
            supply_needed: inputs.number(T::AirGroundCrews, "supply_needed")?,
            supply_supplied: inputs.number(T::AirGroundCrews, "supply_supplied")?,
            supply_deficit_extra: inputs.number(T::AirGroundCrews, "supply_deficit_extra")?,
            link_slot: inputs.number(T::CountryLinks, "canonical_slot")?,
            country_slot: inputs.number(T::Countries, "canonical_slot")?,
            command_power: inputs.number(T::Countries, "command_power_raw")?,
        });
        if std::mem::take(&mut self.hidden) {
            self.all = true;
        }
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        let (marked, phases) = (self.marked, self.phases);
        self.all = true;
        self.settle_dirty(mirrors, Settling::Whole)?;
        self.publish();
        (self.marked, self.phases) = (marked, phases);
        Ok(())
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        if step.gap() || self.columns.is_none() || self.hidden || self.refused.is_some() {
            return;
        }
        let columns = self.columns();
        for changes in step.tables() {
            if changes.status().is_some_and(|(status, _)| self.statuses.insert(changes.table(), status) != Some(status)) {
                self.all = true;
                continue;
            }
            self.apply_table(TableId::ALL[changes.table()], changes, mirrors, columns);
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.settle_dirty(mirrors, settling)?;
        self.publish();
        Ok(())
    }


    fn carried(&self) -> Carried {
        let entities = self.marks();
        Carried { entities }
    }

    fn published(&self) -> Published {
        if self.refused.is_some() || self.hidden || self.waiting.is_empty() {
            return Published::Current;
        }
        self.product_hour.map_or(Published::Nothing, Published::At)
    }

    fn held(&self) -> usize {
        let records = |records: &Arc<Vec<AirCombatRecord>>| records.capacity() * std::mem::size_of::<AirCombatRecord>() + records.iter().map(|record| (record.first_damage.capacity() + record.second_damage.capacity()) * 16).sum::<usize>();
        self.registrations.capacity() * std::mem::size_of::<Option<Registration>>()
            + self.failures.values().map(|reason| reason.capacity() + 40).sum::<usize>()
            + self.wings.values().map(|wings| wings.len() * 32 + 48).sum::<usize>()
            + self.holders.values().map(|keys| keys.len() * 24 + 48).sum::<usize>()
            + self.records.values().map(|(_, held)| 64 + held.as_ref().map_or(0, |(own, foreign)| records(own) + records(foreign))).sum::<usize>()
            + self.values.len() * (std::mem::size_of::<RegionValue>() + 48)
            + self.wake.len() * 24
            + self.hourly.len() * 24
            + self.ours_of.borrow().capacity() * 8
            + self.registrations_dirty.bytes()
            + self.regions.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests;
