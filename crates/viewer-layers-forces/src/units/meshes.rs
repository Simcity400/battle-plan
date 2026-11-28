
use super::models::Select;
use viewer_layer_core::{layers::{RetirementFlag, Settling}, rebuilt::{absent, refusal}, playhead_host::{Entry, timeline::{self, Mark}, views::{self, HeldKeys, Need}}};
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::{DecodedFrame, SharedView, TableView, discovering};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_unit_models::unit_models::{KeptChoices, Made, SelectionKey, Told};

pub const TABLES: &[TableId] = &[
    TableId::Clock,
    TableId::DivisionTemplateIcons,
    TableId::DivisionModelInputs,
    TableId::DivisionStatus,
    TableId::VariantDefinitions,
    TableId::VariantUpgrades,
    TableId::AvailableEquipmentVariants,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::CountryLocalSubunits,
    TableId::Politics,
    TableId::PoliticalParties,
    TableId::Factions,
    TableId::FactionMembers,
    TableId::LexicalTokens,
    TableId::ScriptVariables,
    TableId::ScriptVariableNames,
    TableId::SimulationFeatures,
    TableId::Provinces,
    TableId::States,
    TableId::WeatherProvinces,
    TableId::WeatherRegions,
];

pub const KEYED: [TableId; 2] = [TableId::DivisionEquipment, TableId::NewDivisionEquipment];

const SNOW: u64 = replay_data::rows::mask::<replay_data::rows::WeatherProvinceRow>(&["snow", "permanent_snow"]);

fn needs(tables: &[TableId], learned: &[TableId]) -> Vec<Need> {
    let named = |table: TableId| if table == TableId::WeatherProvinces { Need::columns(table, SNOW) } else { Need::every(table) };
    tables.iter().copied().map(named).chain(learned.iter().copied().map(Need::every)).collect()
}

pub const WHOLE: Duration = Duration::from_secs(10);

const LOOK: Duration = Duration::from_millis(2);

pub type Asked = (u32, SelectionKey);

pub type Mesh = Result<Option<usize>, String>;

pub struct Chosen {
    pub meshes: Vec<(u64, Mesh)>,
    pub made: Made,
}

pub enum Ask {
    Stack(u64, Asked),
    Gone(u64),
}

struct Product {
    clear: bool,
    meshes: Vec<(u64, Mesh)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub builds: u64,
    pub products: u64,
    pub chosen: u64,
    pub selections: u64,
    pub looked_up: u64,
    pub camouflages: u64,
    pub contexts: u64,
    pub busy_ns: u64,
    pub parts: Vec<(&'static str, u64)>,
    pub entering: u64,
    pub leaving: u64,
    pub waiting: u64,
    pub keys: u64,
    pub keyed_rows: u64,
}

#[derive(Default)]
struct Held {
    ours: Option<u32>,
    asks: Vec<Ask>,
    clear: bool,
    epoch: u64,
    made: viewer_runtime::landed::Published<Vec<Product>>,
    hour: viewer_runtime::landed::Published<Option<Hour>>,
    answered: viewer_runtime::landed::Published<Option<Hour>>,
    refused: viewer_runtime::landed::Published<Option<String>>,
    owed: bool,
    tables: Vec<TableId>,
    counts: Counts,
    waiting: u64,
    keys: u64,
    keyed_rows: u64,
    stop: bool,
}

struct Shared {
    held: Mutex<Held>,
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait(&self, until: Instant, reader: &views::Reader) -> bool {
        let held = self.lock();
        if reader.frame().is_none() {
            return false;
        }
        let _ = self.done.wait_timeout(held, until.saturating_duration_since(Instant::now()));
        true
    }

    fn stood(&self, stands: Hour) {
        let mut held = self.lock();
        if held.hour.is_some() {
            held.hour.advance(stands);
        }
        held.answered.advance(stands);
        drop(held);
        self.done.notify_all();
    }
}

pub struct Meshes {
    shared: Arc<Shared>,
    reader: std::sync::Weak<views::Reader>,
    worker: Option<JoinHandle<()>>,
    maker: timeline::Maker,
    pub meshes: HashMap<u64, usize>,
    pub withheld: HashMap<u64, String>,
    pub hour: Option<Hour>,
    pub refused: Option<String>,
}

impl Meshes {
    pub fn start(select: Arc<dyn Select>, store: &views::Views, learned: &[TableId]) -> Self {
        let entry = Arc::clone(store.entry());
        let mut tables: Vec<TableId> = TABLES.iter().chain(learned).copied().filter(|table| !KEYED.contains(table)).collect();
        tables.sort_unstable_by_key(|table| table.index());
        tables.dedup();
        let every: Vec<TableId> = tables.iter().chain(&KEYED).copied().collect();
        let absent = absent(&entry, &every);
        tables.retain(|table| !absent.contains(table));
        let keyed: Vec<TableId> = KEYED.into_iter().filter(|table| !absent.contains(table)).collect();
        let learned: Vec<TableId> = learned.iter().copied().filter(|table| tables.contains(table)).collect();
        let reader = Arc::new(store.telling("unit_meshes", &needs(&tables, &learned)));
        let declared = Arc::new(BTreeSet::new());
        for &table in &keyed {
            reader.declare_keyed(table, Arc::clone(&declared));
        }
        let maker = timeline::maker("unit_meshes");
        let shared = Arc::new(Shared { held: Mutex::new(Held { tables: tables.clone(), clear: true, ..Held::default() }), done: Condvar::new() });
        let run = Run {
            select,
            entry,
            absent: Vec::new(),
            held: HashMap::new(),
            asked: HashSet::new(),
            kept: KeptChoices::default(),
            last: None,
            kept_at: None,
            since: BTreeMap::new(),
            tables,
            learned,
            keyed,
            divisions: BTreeSet::new(),
            declared,
            waiting: Vec::new(),
            read: None,
            clear: false,
            again: false,
        };
        let rows = Arc::downgrade(&reader);
        let spawned = std::thread::Builder::new().name("unit meshes".into()).spawn({
            let shared = Arc::clone(&shared);
            move || {
                viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
                run.started(&absent).work(&shared, &reader, maker);
            }
        });
        let (worker, refused) = match spawned {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(format!("The unit meshes' worker cannot start: {error}"))),
        };
        Self { shared, reader: rows, worker, maker, meshes: HashMap::new(), withheld: HashMap::new(), hour: None, refused }
    }

    pub fn maker(&self) -> timeline::Maker {
        self.maker
    }

    pub fn clear(&mut self) {
        let mut held = self.shared.lock();
        held.asks.clear();
        (held.clear, held.epoch) = (true, held.epoch + 1);
        held.answered.clear();
    }

    pub fn ask(&mut self, ours: u32, asks: Vec<Ask>) {
        let mut held = self.shared.lock();
        held.ours = Some(ours);
        held.asks.extend(asks);
        drop(held);
        timeline::mark(self.maker, Mark::Asked);
    }

    pub fn join(&mut self, applied: Hour, settling: Settling, retiring: &RetirementFlag) -> bool {
        let until = Instant::now() + settling.waiting(WHOLE);
        let strict = settling.strict();
        let mut changed = false;
        loop {
            changed |= self.take();
            if self.shared.lock().answered.as_ref().is_some_and(|&at| at >= applied) && (!strict || self.stand()) {
                return changed | self.take();
            }
            let now = Instant::now();
            if retiring.requested() || now >= until {
                return changed;
            }
            if !self.reader.upgrade().is_some_and(|reader| self.shared.wait(until.min(now + RetirementFlag::POLL), &reader)) {
                return changed;
            }
        }
    }

    fn take(&mut self) -> bool {
        let (made, hour, refused) = {
            let mut held = self.shared.lock();
            (held.made.take_all().map(viewer_runtime::landed::Landed::into_inner).unwrap_or_default(), held.hour.as_ref().copied(), held.refused.as_ref().cloned())
        };
        let mut changed = false;
        for product in made {
            if product.clear {
                changed |= !self.meshes.is_empty() || !self.withheld.is_empty();
                self.meshes.clear();
                self.withheld.clear();
            }
            for (oldest, mesh) in product.meshes {
                changed |= match mesh {
                    Ok(Some(mesh)) => {
                        self.withheld.remove(&oldest);
                        self.meshes.insert(oldest, mesh) != Some(mesh)
                    }
                    Ok(None) => {
                        self.withheld.remove(&oldest);
                        self.meshes.remove(&oldest).is_some()
                    }
                    Err(why) => {
                        self.withheld.insert(oldest, why);
                        self.meshes.remove(&oldest).is_some()
                    }
                };
            }
        }
        (self.hour, self.refused) = (hour, refused);
        changed
    }

    pub fn tables(&self) -> Vec<TableId> {
        self.shared.lock().tables.clone()
    }

    pub fn waiting(&self) -> bool {
        self.hour.is_none() && self.refused.is_none()
    }

    pub fn stand(&self) -> bool {
        let held = self.shared.lock();
        held.refused.as_ref().is_some() || (!held.owed && !held.clear && held.asks.iter().all(|ask| matches!(ask, Ask::Gone(_))))
    }

    pub fn take_counts(&self) -> Counts {
        let mut held = self.shared.lock();
        let (waiting, keys, keyed_rows) = (held.waiting, held.keys, held.keyed_rows);
        let mut counts = std::mem::take(&mut held.counts);
        (counts.waiting, counts.keys, counts.keyed_rows) = (waiting, keys, keyed_rows);
        counts
    }

    pub fn record_view(&self) {
        use viewer_runtime::view_record::set;
        let held = self.shared.lock();
        set("units", "choices_waiting", held.waiting);
        set("units", "keys_declared", held.keys);
        set("units", "rows_by_key", held.keyed_rows);
        drop(held);
        set("units", "meshes_held", self.meshes.len() as u64);
        set("units", "meshes_withheld", self.withheld.len() as u64);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state(&self) -> String {
        let held = self.shared.lock();
        format!(
            "keys declared {}; asks queued {}, waiting for their rows {}, owed {}, clear {}, hour {:?}, answered {:?}, refused {:?}",
            held.keys,
            held.asks.len(),
            held.waiting,
            held.owed,
            held.clear,
            held.hour.as_ref(),
            held.answered.as_ref(),
            held.refused.as_ref()
        )
    }

    pub fn bytes(&self) -> usize {
        self.meshes.capacity() * (8 + 8 + 8) + self.withheld.capacity() * (8 + 24 + 8) + self.withheld.values().map(String::capacity).sum::<usize>()
    }
}

impl Drop for Meshes {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.done.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Run {
    select: Arc<dyn Select>,
    entry: Arc<Entry>,
    absent: Vec<(TableId, SharedView)>,
    held: HashMap<u64, Asked>,
    asked: HashSet<u64>,
    kept: KeptChoices,
    last: Option<Arc<DecodedFrame>>,
    kept_at: Option<DecodedFrame>,
    since: BTreeMap<TableId, (SharedView, Option<BTreeMap<u128, u64>>)>,
    tables: Vec<TableId>,
    learned: Vec<TableId>,
    keyed: Vec<TableId>,
    divisions: BTreeSet<u64>,
    declared: Arc<BTreeSet<u64>>,
    waiting: Vec<Ask>,
    read: Option<(Arc<DecodedFrame>, u32)>,
    clear: bool,
    again: bool,
}

impl Run {
    fn started(mut self, absent: &[TableId]) -> Self {
        self.refuse(absent);
        self
    }

    fn refuse(&mut self, tables: &[TableId]) {
        let mut every: Vec<TableId> = self.absent.iter().map(|(table, _)| *table).chain(tables.iter().copied()).collect();
        every.sort_unstable_by_key(|table| table.index());
        every.dedup();
        if every.is_empty() {
            return;
        }
        let reason = refusal("the land unit models", &every);
        self.absent = every.into_iter().map(|table| (table, SharedView::from(TableView::Unavailable { reason: reason.clone() }))).collect();
    }

    fn note(&mut self, shared: &Shared, asks: &[Ask]) {
        let mut counts = (0, 0);
        for ask in asks {
            match ask {
                Ask::Stack(oldest, _) if self.divisions.insert(*oldest) => counts.0 += 1,
                Ask::Gone(oldest) if self.divisions.remove(oldest) => counts.1 += 1,
                _ => {}
            }
        }
        viewer_runtime::view_record::add("units", "entering", counts.0);
        viewer_runtime::view_record::add("units", "leaving", counts.1);
        let mut held = shared.lock();
        (held.counts.entering, held.counts.leaving) = (held.counts.entering + counts.0, held.counts.leaving + counts.1);
    }

    fn declare_keys(&mut self, reader: &views::Reader) {
        if *self.declared == self.divisions {
            return;
        }
        self.declared = Arc::new(self.divisions.clone());
        for &table in &self.keyed {
            reader.declare_keyed(table, Arc::clone(&self.declared));
        }
    }

    fn holds(&self, keys: &HeldKeys, ask: Option<&Ask>) -> bool {
        self.keyed.iter().all(|&table| {
            let Some(held) = keys.of(table) else { return false };
            match ask {
                Some(Ask::Gone(_)) => true,
                Some(Ask::Stack(oldest, _)) => held.contains(oldest),
                None => self.divisions.is_subset(held),
            }
        }) || ask.is_some_and(|ask| matches!(ask, Ask::Gone(_)))
    }

    fn owed(&self) -> bool {
        self.clear || self.again || self.waiting.iter().any(|ask| matches!(ask, Ask::Stack(..)))
    }

    fn publish(&self, shared: &Shared, reader: &views::Reader, running: bool) {
        let waiting = self.waiting.iter().filter(|ask| matches!(ask, Ask::Stack(..))).count() as u64;
        let keyed_rows = viewer_runtime::view_record::on().then(|| self.keyed_rows(reader));
        let mut held = shared.lock();
        held.owed = running || self.owed();
        (held.waiting, held.keys) = (waiting, self.declared.len() as u64);
        if let Some(rows) = keyed_rows {
            held.keyed_rows = rows;
        }
    }

    fn keyed_rows(&self, reader: &views::Reader) -> u64 {
        reader.frame().map_or(0, |frame| {
            self.keyed
                .iter()
                .map(|&table| match frame.tables[table.index()].held() {
                    TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.len() as u64,
                    _ => 0,
                })
                .sum()
        })
    }

    fn work(mut self, shared: &Shared, reader: &views::Reader, maker: timeline::Maker) {
        loop {
            let (ours, asks, clear, epoch) = {
                let mut held = shared.lock();
                if held.stop {
                    return;
                }
                let asks = std::mem::take(&mut held.asks);
                let clear = std::mem::replace(&mut held.clear, false);
                if clear || asks.iter().any(|ask| matches!(ask, Ask::Stack(..))) {
                    held.owed = true;
                }
                (held.ours, asks, clear, held.epoch)
            };
            if clear {
                self.forget();
            }
            if !asks.is_empty() {
                self.note(shared, &asks);
                for ask in asks {
                    let oldest = match &ask {
                        Ask::Stack(oldest, _) | Ask::Gone(oldest) => *oldest,
                    };
                    if matches!(ask, Ask::Gone(_)) {
                        self.waiting.retain(|held| !matches!(held, Ask::Stack(stack, _) if *stack == oldest));
                    }
                    self.waiting.push(ask);
                }
                self.declare_keys(reader);
            }
            let (Some(ours), Some((newest, published))) = (ours, reader.newest()) else {
                self.publish(shared, reader, false);
                reader.wait_newer(0, LOOK);
                continue;
            };
            let Some((frame, keys)) = reader.frame_keyed() else { continue };
            if !Arc::ptr_eq(&newest, &frame) {
                continue;
            }
            if !self.again
                && let Some((read, _)) = &self.read
                && let Some(stands) = reader.through(Arc::as_ptr(read))
            {
                shared.stood(stands);
            }
            let made_anew = self.read.as_ref().is_none_or(|(read, _)| {
                self.tables.iter().copied().chain(self.keyed.iter().copied()).any(|table| !SharedView::ptr_eq(&read.tables[table.index()], &frame.tables[table.index()]))
            });
            let sendable = self.waiting.iter().any(|ask| self.holds(&keys, Some(ask)));
            let asked = self.clear || self.again || sendable || self.read.as_ref().is_none_or(|&(_, viewer)| viewer != ours);
            if !made_anew && !asked {
                shared.stood(reader.through(Arc::as_ptr(&frame)).unwrap_or(frame.hour).max(frame.hour));
                self.publish(shared, reader, false);
                reader.wait_newer(published, LOOK);
                continue;
            }
            let Some((frame, told, keys)) = reader.take_keyed() else { continue };
            timeline::mark(maker, Mark::Rows);
            let (chosen, waiting): (Vec<Ask>, Vec<Ask>) = std::mem::take(&mut self.waiting).into_iter().partition(|ask| self.holds(&keys, Some(ask)));
            self.waiting = waiting;
            let clear = std::mem::take(&mut self.clear);
            self.again = false;
            {
                let mut held = shared.lock();
                (held.counts.builds, held.owed) = (held.counts.builds + 1, true);
            }
            timeline::mark(maker, Mark::BuildBegan);
            let started = Instant::now();
            let answer = self.answer(&frame, told, &keys, ours, clear, chosen);
            timeline::mark(maker, Mark::BuildEnded);
            let busy_ns = started.elapsed().as_nanos() as u64;
            if self.again {
                reader.declare_only_needs(&needs(&self.tables, &self.learned));
                shared.lock().tables.clone_from(&self.tables);
            }
            if shared.lock().epoch != epoch {
                continue;
            }
            self.publish_answer(shared, answer, busy_ns, &frame);
            self.publish(shared, reader, false);
            shared.done.notify_all();
            reader.notify_progress();
        }
    }

    fn forget(&mut self) {
        self.held.clear();
        self.asked.clear();
        self.divisions.clear();
        self.waiting.clear();
        self.read = None;
        self.again = false;
        self.clear = true;
    }

    fn publish_answer(&self, shared: &Shared, answer: Answer, busy_ns: u64, frame: &Arc<DecodedFrame>) {
        let Answer { meshes, made, chosen, refused, clear } = answer;
        let mut held = shared.lock();
        let counts = &mut held.counts;
        counts.products += 1;
        counts.chosen += chosen as u64;
        counts.selections += made.selections as u64;
        let looked_up = (chosen as u64).saturating_sub(made.selections as u64);
        counts.looked_up += looked_up;
        counts.camouflages += made.camouflages as u64;
        counts.contexts += made.contexts as u64;
        counts.busy_ns += busy_ns;
        for &(name, ns) in &made.parts {
            match counts.parts.iter_mut().find(|(held, _)| *held == name) {
                Some((_, sum)) => *sum += ns,
                None => counts.parts.push((name, ns)),
            }
        }
        viewer_runtime::view_record::add("units", "choices_looked_up", looked_up);
        viewer_runtime::view_record::add("units", "choices_made_new", made.selections as u64);
        held.refused.set_if_changed(refused);
        if !self.again && held.refused.is_none() {
            held.hour.set_if_changed(Some(frame.hour));
        }
        held.answered.advance(frame.hour);
        held.made.push(Product { clear, meshes });
    }

    fn answer(&mut self, frame: &Arc<DecodedFrame>, told: views::Told, keys: &HeldKeys, ours: u32, clear: bool, chosen: Vec<Ask>) -> Answer {
        let mut answer = Answer { meshes: Vec::new(), made: Session::default(), chosen: 0, refused: None, clear };
        let mut since = Instant::now();
        let mut lap = |parts: &mut Vec<(&'static str, u64)>, name: &'static str| {
            let now = Instant::now();
            parts.push((name, now.duration_since(since).as_nanos() as u64));
            since = now;
        };
        for ask in chosen {
            match ask {
                Ask::Stack(oldest, asked) => {
                    self.held.insert(oldest, asked);
                    self.asked.insert(oldest);
                }
                Ask::Gone(oldest) => {
                    self.held.remove(&oldest);
                    self.asked.remove(&oldest);
                    answer.meshes.push((oldest, Ok(None)));
                }
            }
        }
        let tables: Vec<TableId> = self.tables.iter().copied().chain(self.keyed.iter().copied()).collect();
        self.read = Some((Arc::clone(frame), ours));
        self.note_rows(frame, &tables, told);
        lap(&mut answer.made.parts, "asks_and_told");
        let mut rows = DecodedFrame::clone(frame);
        for (table, view) in &self.absent {
            rows.tables[table.index()] = view.clone();
        }
        let told = Told::new(self.since.iter().filter_map(|(&table, (from, rows))| Some((table, from.clone(), rows.as_ref()?.iter().map(|(&key, &fields)| (key, fields)).collect()))).collect());
        lap(&mut answer.made.parts, "rows_and_told");
        let mut session = Vec::new();
        let keyed = keys.of(TableId::DivisionEquipment).or_else(|| keys.of(TableId::NewDivisionEquipment)).map(|keys| &**keys);
        let (made, missed) = discovering(|| self.select.choose_meshes(&rows, ours, (&self.kept, keyed), &self.held, &self.asked, &told, &mut |name, ns| session.push((name, ns))));
        answer.made.parts.extend(session);
        match made {
            Ok(Chosen { meshes, made }) => {
                (self.kept_at, self.since) = (Some(rows), BTreeMap::new());
                (answer.made.selections, answer.made.camouflages, answer.made.contexts) = (made.selections, made.camouflages, made.contexts);
                let missed: Vec<TableId> = missed.into_iter().filter(|table| !tables.contains(table) && !self.absent.iter().any(|(held, _)| held == table)).collect();
                if !missed.is_empty() {
                    let gone = absent(&self.entry, &missed);
                    self.refuse(&gone);
                    let learned: Vec<TableId> = missed.into_iter().filter(|table| !gone.contains(table)).collect();
                    if !learned.is_empty() {
                        self.learn(&learned);
                    }
                    self.again = true;
                    return answer;
                }
                self.asked.clear();
                answer.chosen = meshes.len();
                answer.meshes.extend(meshes);
            }
            Err(error) => answer.refused = Some(format!("The land unit models cannot be chosen at the head: {error}")),
        }
        answer
    }

    fn learn(&mut self, learned: &[TableId]) {
        self.learned.extend(learned.iter().copied());
        self.tables.extend(learned.iter().copied());
        for held in [&mut self.tables, &mut self.learned] {
            held.sort_unstable_by_key(|table| table.index());
            held.dedup();
        }
    }

    fn note_rows(&mut self, frame: &Arc<DecodedFrame>, tables: &[TableId], mut told: views::Told) {
        let last = self.last.replace(Arc::clone(frame));
        let Some(kept_at) = &self.kept_at else { return };
        for &table in tables {
            let index = table.index();
            let rows = match told.remove(&table) {
                Some(rows) => rows,
                None if last.as_ref().is_some_and(|last| SharedView::ptr_eq(&last.tables[index], &frame.tables[index])) => continue,
                None => None,
            };
            let (_, held) = self.since.entry(table).or_insert_with(|| (kept_at.tables[index].clone(), Some(BTreeMap::new())));
            match (held, rows) {
                (Some(held), Some(rows)) => {
                    for (key, fields) in rows {
                        *held.entry(key).or_default() |= fields;
                    }
                }
                (held, _) => *held = None,
            }
        }
    }
}

#[derive(Default)]
struct Session {
    selections: usize,
    camouflages: usize,
    contexts: usize,
    parts: Vec<(&'static str, u64)>,
}

struct Answer {
    meshes: Vec<(u64, Mesh)>,
    made: Session,
    chosen: usize,
    refused: Option<String>,
    clear: bool,
}
