
use crate::layers::{Published, Retirement};
use crate::playhead_host::{
    Entry,
    views::{Need, Recording},
    timeline::{self, Mark},
    views,
};
use engine_executable::executable_tables::Tables;
use replay_data::{
    Hour,
    rows::{TableId, TableRows},
};
use replay_frame::frame::{DecodedFrame, SharedView, TableView, discovering};
use std::{
    io,
    panic::AssertUnwindSafe,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_runtime::{
    landed::{Landed, Published as PublishedSlot, PublishedHour},
    presentation::Continuity,
};

const TICK: Duration = Duration::from_micros(16_667);

const WHOLE: Duration = Duration::from_secs(30);

pub type Seen = (Choice, Option<Arc<Vision>>);

pub trait Build: Send + Sync + 'static {
    type Product: Send + Sync + 'static;
    fn exact_columns(&self) -> &[TableId] { &[] }
    fn row_tables(&self, _view: &crate::layers::View) -> Vec<TableId> { Vec::new() }
    fn hold_picture(&self) -> bool { false }
    fn tables(&self) -> Option<Tables>;
    fn build(&self, frame: &DecodedFrame, seen: &Seen) -> io::Result<Self::Product>;
    fn anew(&self, frame: &DecodedFrame, seen: &Seen) -> io::Result<Self::Product> {
        self.build(frame, seen)
    }
    fn stands(&self, product: &Self::Product, frame: &DecodedFrame, seen: &Seen) -> bool {
        let _ = (product, frame, seen);
        false
    }
    fn hourly(&self) -> bool {
        false
    }
    fn viewed(&self, view: &crate::layers::View) -> bool {
        let _ = view;
        false
    }
    fn parts(&self, product: &Self::Product, each: &mut dyn FnMut(&'static str, u64)) {
        let _ = (product, each);
    }
    fn counts(&self, product: &Self::Product, each: &mut dyn FnMut(&'static str, u64)) {
        let _ = (product, each);
    }
    fn install(&self, gpu: &mut crate::layers::Gpu<'_>, product: Option<&Self::Product>, continuity: Continuity) -> io::Result<Option<Self::Product>> {
        let _ = (gpu, product, continuity);
        Ok(None)
    }
}

pub struct Built<P> {
    pub hour: Hour,
    pub generation: u64,
    pub product: Result<P, String>,
    pub ns: u64,
    pub frame: Option<Arc<DecodedFrame>>,
    pub seen: Option<Seen>,
    current: PublishedHour,
    stands: PublishedHour,
}

impl<P> Built<P> {
    fn new(hour: Hour, generation: u64, product: Result<P, String>, ns: u64, frame: Option<Arc<DecodedFrame>>, seen: Option<Seen>) -> Self {
        Self { hour, generation, product, ns, frame, seen, current: PublishedHour::new(hour), stands: PublishedHour::new(hour) }
    }

    pub fn current(&self) -> Hour {
        self.current.load()
    }

    pub fn stands(&self) -> Hour {
        self.stands.load().max(self.current())
    }

    fn of(&self, hour: Hour) -> bool {
        self.hour == hour || self.current() == hour
    }
}

#[derive(Clone, Debug, Default)]
pub struct Builds {
    pub count: u64,
    pub total_ns: u64,
    pub most_ns: u64,
    pub ns: Vec<u64>,
    pub skipped: u64,
    pub discovered: u64,
    pub confirmed: u64,
    pub confirm_ns: u64,
    pub parts: Vec<(&'static str, u64)>,
    pub counts: Vec<(&'static str, u64)>,
    pub behind: [u64; BEHIND.len()],
    pub behind_sum: u64,
    pub behind_most: u64,
    pub span: Option<(Instant, Instant)>,
}

pub const BEHIND: [u64; 7] = [0, 24, 96, 384, 1_536, 6_144, u64::MAX];

impl Builds {
    fn record(&mut self, began: Instant, ns: u64, behind: Option<u64>, timed: bool) {
        (self.count, self.total_ns, self.most_ns) = (self.count + 1, self.total_ns + ns, self.most_ns.max(ns));
        if timed {
            self.ns.push(ns);
        }
        if let Some(behind) = behind {
            self.behind[BEHIND.iter().position(|&bound| behind <= bound).unwrap_or(BEHIND.len() - 1)] += 1;
            (self.behind_sum, self.behind_most) = (self.behind_sum + behind, self.behind_most.max(behind));
        }
        let ended = Instant::now();
        self.span = Some((self.span.map_or(began, |(first, _)| first), ended));
    }

    fn part(&mut self, name: &'static str, ns: u64) {
        add(&mut self.parts, name, ns);
    }

    fn count(&mut self, name: &'static str, count: u64) {
        add(&mut self.counts, name, count);
    }
}

fn add(sums: &mut Vec<(&'static str, u64)>, name: &'static str, value: u64) {
    match sums.iter_mut().find(|(held, _)| *held == name) {
        Some((_, sum)) => *sum += value,
        None => sums.push((name, value)),
    }
}

struct State<P> {
    name: &'static str,
    source: Option<(Arc<Entry>, Recording)>,
    seen: Option<Seen>,
    generation: u64,
    open: bool,
    stop: bool,
    head: Option<Hour>,
    views: Option<views::Views>,
    reader: Option<Arc<views::Reader>>,
    built: PublishedSlot<Option<Arc<Built<P>>>>,
    tables: Vec<TableId>,
    builds: Builds,
    seen_hour: Option<Hour>,
    settled: Option<(Hour, u64)>,
    awaiting: Vec<TableId>,
    refused: PublishedSlot<Option<String>>,
    stopped: PublishedSlot<Option<String>>,
    retired: Vec<Box<dyn Send>>,
}

struct Shared<P> {
    state: Mutex<State<P>>,
    wake: Condvar,
    built: Condvar,
}

impl<P> Shared<P> {
    fn lock(&self) -> MutexGuard<'_, State<P>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.built.notify_all();
        self.wake.notify_all();
    }

    fn notify_product(&self, state: &State<P>) {
        self.built.notify_all();
        if let Some(views) = &state.views {
            views.notify_progress();
        }
    }
}

pub struct Rebuilt<P> {
    shared: Arc<Shared<P>>,
    worker: Option<JoinHandle<()>>,
    name: &'static str,
    maker: timeline::Maker,
    timed: bool,
}

impl<P: Send + Sync + 'static> Rebuilt<P> {
    pub fn new<B: Build<Product = P>>(name: &'static str, build: Arc<B>, tables: &[TableId], timed: bool) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                name,
                source: None,
                seen: None,
                generation: 0,
                open: false,
                stop: false,
                head: None,
                views: None,
                reader: None,
                built: PublishedSlot::default(),
                tables: {
                    let mut tables = tables.to_vec();
                    if build.hourly() && !tables.contains(&TableId::Clock) {
                        tables.push(TableId::Clock);
                    }
                    tables
                },
                builds: Builds::default(),
                seen_hour: None,
                settled: None,
                awaiting: Vec::new(),
                refused: PublishedSlot::default(),
                stopped: PublishedSlot::default(),
                retired: Vec::new(),
            }),
            wake: Condvar::new(),
            built: Condvar::new(),
        });
        let working = Arc::clone(&shared);
        let maker = timeline::maker(name);
        let worker = std::thread::Builder::new().name(name.into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            let worked = std::panic::catch_unwind(AssertUnwindSafe(|| work(&working, &*build, &Rules { name, timed, maker })));
            if let Err(panic) = worked {
                let mut state = working.lock();
                state.stopped.set_some(format!("The {name} worker stopped: {}", panicked(&*panic)));
                working.notify_product(&state);
            }
        })?;
        Ok(Self { shared, worker: Some(worker), name, maker, timed })
    }

    pub fn set_source(&self, entry: Arc<Entry>, recording: Recording, seen: Seen) -> io::Result<()> {
        self.attach(entry, recording)?;
        self.set_seen(seen);
        Ok(())
    }

    pub fn attach(&self, entry: Arc<Entry>, recording: Recording) -> io::Result<()> {
        let mut state = self.shared.lock();
        if state.source.as_ref().is_some_and(|(held, _)| Arc::ptr_eq(held, &entry)) {
            return Ok(());
        }
        let absent = absent(&entry, &state.tables);
        state.refused.set_if_changed((!absent.is_empty()).then(|| refusal(&format!("the {} layer", self.name), &absent)));
        state.source = Some((entry, recording));
        Ok(())
    }

    pub fn set_views(&self, store: &views::Store) {
        let mut state = self.shared.lock();
        let needs: Vec<Need> = state.tables.iter().copied().map(Need::every).collect();
        if state.refused.is_none() {
            state.reader = Some(Arc::new(store.reader(self.name, &needs)));
            state.awaiting = state.tables.clone();
        }
        state.views = Some(store.handle());
        drop(state);
        self.shared.wake.notify_all();
    }

    pub fn set_seen(&self, seen: Seen) -> u64 {
        let mut state = self.shared.lock();
        state.seen = Some(seen);
        state.generation += 1;
        let generation = state.generation;
        drop(state);
        self.shared.wake.notify_all();
        generation
    }

    pub fn renew(&self) -> u64 {
        let mut state = self.shared.lock();
        state.generation += 1;
        let generation = state.generation;
        drop(state);
        self.shared.wake.notify_all();
        generation
    }

    pub fn perspective(&self) -> Option<Choice> {
        self.shared.lock().seen.as_ref().map(|(choice, _)| choice.clone())
    }

    pub fn set_open(&self, open: bool) {
        let mut state = self.shared.lock();
        let declared = !open && state.reader.take().is_some();
        if state.open != open || declared {
            state.open = open;
            if !open {
                state.built.clear();
            }
            drop(state);
            self.shared.wake.notify_all();
        }
    }

    fn declare_rows(&self, tables: Vec<TableId>) {
        if tables.is_empty() {
            return;
        }
        let mut state = self.shared.lock();
        if state.refused.is_some() {
            return;
        }
        let additional: Vec<_> = tables.into_iter().filter(|table| {
            !state.tables.contains(table)
                && state.source.as_ref().is_some_and(|(entry, _)| crate::playhead_host::details::held_at_all(entry, *table))
        }).collect();
        state.tables.extend(additional);
        state.tables.sort_unstable_by_key(|table| table.index());
        state.tables.dedup();
        if let Some(reader) = row_reader(&mut state, &self.shared.built) {
            reader.declare_only(&state.tables);
        }
        self.shared.wake.notify_all();
    }

    pub fn applied(&self, hour: Hour) {
        let mut state = self.shared.lock();
        if state.head == Some(hour) {
            return;
        }
        state.head = Some(hour);
        timeline::mark(self.maker, Mark::Asked);
        drop(state);
        self.shared.wake.notify_all();
    }

    pub fn newer(&self, held: Option<&Arc<Built<P>>>) -> Option<Landed<Arc<Built<P>>>> {
        timeline::mark(self.maker, Mark::Joined);
        let state = self.shared.lock();
        let built = state.built.as_ref()?;
        let newer = held.is_none_or(|held| !Arc::ptr_eq(held, built));
        newer.then(|| state.built.landed_clone()).flatten()
    }

    pub fn retire(&self, gone: impl Send + 'static) {
        self.shared.lock().retired.push(Box::new(gone));
        self.shared.wake.notify_all();
    }

    pub fn wait(&self, hour: Hour, limit: Duration) -> io::Result<Landed<Arc<Built<P>>>> {
        self.wait_for(hour, limit, false)
    }

    fn wait_for(&self, hour: Hour, limit: Duration, drives_rows: bool) -> io::Result<Landed<Arc<Built<P>>>> {
        let until = Instant::now() + limit;
        let mut state = self.shared.lock();
        loop {
            if state.stop {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if let Some(built) = state.built.as_ref()
                && built.of(hour)
                && built.generation == state.generation
            {
                return Ok(state.built.landed_clone().expect("checked above"));
            }
            if let Some(why) = state.stopped.as_ref() {
                return Err(io::Error::other(why.clone()));
            }
            if let Some(why) = state.refused.landed_clone() {
                let generation = state.generation;
                return Ok(why.map(|why| Arc::new(Built::new(hour, generation, Err(why), 0, None, None))));
            }
            if drives_rows && state.views.is_some() && state.reader.as_ref().is_none_or(|reader| !reader.has_rows_at(hour)) {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, format!("The {} layer awaits {:?} at hour {hour}", self.name, state.tables)));
            }
            let now = Instant::now();
            if now >= until {
                let had = state.built.as_ref().map(|built| (built.hour, built.current(), built.generation));
                return Err(io::Error::other(format!(
                    "The worker built no product at hour {hour} with vision {} in {limit:?}; it holds {had:?}, its newest rows are at {:?}, it awaits the rows of {:?}",
                    state.generation,
                    state.seen_hour,
                    state.awaiting,
                )));
            }
            state = self.shared.built.wait_timeout(state, until - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn wait_settled(&self, hour: Hour, before_discovered: u64) -> io::Result<Option<Arc<Built<P>>>> {
        let mut state = self.shared.lock();
        loop {
            if let Some(why) = state.stopped.as_ref() {
                return Err(io::Error::other(why.clone()));
            }
            if let Some(why) = state.refused.as_ref() {
                return Ok(Some(Arc::new(Built::new(hour, state.generation, Err(why.clone()), 0, None, None))));
            }
            if state.builds.discovered > before_discovered {
                return Ok(state.built.as_ref().cloned());
            }
            if state.settled.is_some_and(|(settled_hour, settled_gen)| settled_hour >= hour && settled_gen == state.generation) {
                return Ok(state.built.as_ref().cloned());
            }
            state = self.shared.built.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn looked(&self, hour: Hour, until: Instant) -> bool {
        let mut state = self.shared.lock();
        loop {
            if state.seen_hour == Some(hour) {
                return true;
            }
            let now = Instant::now();
            if now >= until || state.stopped.is_some() {
                return false;
            }
            state = self.shared.built.wait_timeout(state, (until - now).min(TICK)).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    pub fn builds(&self) -> Builds {
        self.shared.lock().builds.clone()
    }

    pub fn tables(&self) -> Vec<TableId> {
        self.shared.lock().tables.clone()
    }

    pub fn awaiting(&self) -> Vec<TableId> {
        self.shared.lock().awaiting.clone()
    }

    pub fn stopped(&self) -> Option<Landed<String>> {
        self.shared.lock().stopped.landed_clone()
    }
}

impl<P> Drop for Rebuilt<P> {
    fn drop(&mut self) {
        self.shared.stop();
        if self.timed {
            report(self.name, &self.shared.lock());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn absent(entry: &Entry, tables: &[TableId]) -> Vec<TableId> {
    tables.iter().copied().filter(|&table| !crate::playhead_host::details::held_at_all(entry, table)).collect()
}

pub fn refusal(what: &str, absent: &[TableId]) -> String {
    let names: Vec<String> = absent.iter().map(|table| format!("{table:?}")).collect();
    format!("The recording holds no {}, which {what} reads", names.join(", "))
}

pub fn panicked(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic with no message".to_owned())
}

fn attempt<B: Build, T>(build: &B, run: impl FnOnce() -> io::Result<T>) -> Result<T, String> {
    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| engine_executable::executable_tables::scope(build.tables(), run)));
    match ran {
        Ok(product) => product.map_err(|error| error.to_string()),
        Err(panic) => Err(format!("The build panicked: {}", panicked(&*panic))),
    }
}

fn learning<B: Build>(build: &B, frame: &DecodedFrame, seen: &Seen) -> (Result<B::Product, String>, Vec<TableId>) {
    let (product, mut missed) = discovering(|| attempt(build, || build.build(frame, seen)));
    let mut trial: Option<DecodedFrame> = None;
    let mut given = 0;
    while given < missed.len() {
        let trial = trial.get_or_insert_with(|| frame.clone());
        for &table in &missed[given..] {
            trial.tables[table.index()] = SharedView::from(TableView::Complete(TableRows::empty(table)));
        }
        given = missed.len();
        let (_, more) = discovering(|| attempt(build, || build.build(trial, seen)));
        let more: Vec<TableId> = more.into_iter().filter(|table| !missed.contains(table)).collect();
        missed.extend(more);
    }
    (product, missed)
}

fn stands<B: Build>(build: &B, built: &Built<B::Product>, frame: &DecodedFrame, tables: &[TableId], seen: &Seen) -> bool {
    let (Some(then), Ok(product)) = (&built.frame, &built.product) else { return false };
    let same = then.branch == frame.branch && tables.iter().filter(|&&table| !(build.hourly() && table == TableId::Clock)).all(|table| SharedView::ptr_eq(&then.tables[table.index()], &frame.tables[table.index()]));
    if !same {
        return false;
    }
    let (stands, missed) = discovering(|| attempt(build, || Ok(build.stands(product, frame, seen))));
    missed.is_empty() && stands == Ok(true)
}

pub fn holds(frame: &DecodedFrame, tables: &[TableId]) -> bool {
    tables.iter().all(|table| !frame.tables[table.index()].is_not_declared())
}

fn report<P>(name: &str, state: &State<P>) {
    let builds = &state.builds;
    let mut ms: Vec<f64> = builds.ns.iter().map(|&ns| ns as f64 / 1e6).collect();
    ms.sort_unstable_by(f64::total_cmp);
    let at = |q: f64| ms.get(((ms.len() as f64 - 1.0) * q).round() as usize).copied();
    let line = serde_json::json!({
        "worker": name, "builds": builds.count, "build_ms": {"p50": at(0.5), "p99": at(0.99), "most": builds.most_ns as f64 / 1e6, "mean": builds.total_ns as f64 / 1e6 / builds.count.max(1) as f64},
        "passed_over": builds.skipped, "discovered": builds.discovered, "confirmed": builds.confirmed, "tables": state.tables.iter().map(|table| format!("{table:?}")).collect::<Vec<_>>(),
        "awaiting": state.awaiting.iter().map(|table| format!("{table:?}")).collect::<Vec<_>>(), "refused": state.refused.as_ref(), "stopped": state.stopped.as_ref(),
    });
    eprintln!("{line}");
}

struct Rules {
    name: &'static str,
    timed: bool,
    maker: timeline::Maker,
}

fn refuse<P>(shared: &Shared<P>, state: &mut State<P>, why: String) {
    let Some(hour) = state.head else { return };
    if state.built.as_ref().is_some_and(|built| built.hour == hour && built.generation == state.generation) {
        return;
    }
    state.built.set_some(Arc::new(Built::new(hour, state.generation, Err(why), 0, None, None)));
    shared.notify_product(state);
}

fn row_reader<P>(state: &mut State<P>, built: &Condvar) -> Option<Arc<views::Reader>> {
    if state.reader.is_none() {
        let store = state.views.as_ref()?;
        let needs: Vec<Need> = state.tables.iter().copied().map(Need::every).collect();
        state.reader = Some(Arc::new(store.reader(state.name, &needs)));
        state.awaiting = state.tables.clone();
        built.notify_all();
    }
    state.reader.clone()
}

fn work<B: Build>(shared: &Shared<B::Product>, build: &B, rules: &Rules) {
    let mut last: Option<(Arc<DecodedFrame>, u64, u64)> = None;
    loop {
        let mut state = shared.lock();
        if state.stop {
            return;
        }
        if !state.retired.is_empty() {
            let retired = std::mem::take(&mut state.retired);
            drop(state);
            drop(retired);
            continue;
        }
        if !state.open || state.seen.is_none() {
            last = None;
            state.awaiting.clear();
            let _ = shared.wake.wait_timeout(state, TICK);
            continue;
        }
        if let Some(why) = state.refused.as_ref().cloned() {
            state.reader = None;
            state.awaiting.clear();
            refuse(shared, &mut state, why);
            let _ = shared.wake.wait_timeout(state, TICK);
            continue;
        }
        let generation = state.generation;
        let Some(reader) = row_reader(&mut state, &shared.built) else {
            let _ = shared.wake.wait_timeout(state, TICK);
            continue;
        };
        let before = reader.published();
        let Some((frame, published)) = reader.newest().filter(|(frame, _)| holds(frame, &state.tables)) else {
            drop(state);
            reader.wait_newer(before, TICK);
            continue;
        };
        state.awaiting.clear();
        if last.as_ref().is_some_and(|(held, _, taken)| Arc::ptr_eq(held, &frame) && *taken == generation) || state.generation != generation {
            if let Some(built) = state.built.as_ref().filter(|built| built.generation == generation && built.current() >= frame.hour)
                && let Some(through) = reader.through(Arc::as_ptr(&frame))
                && through > built.stands()
            {
                built.stands.advance(through);
                shared.notify_product(&state);
            }
            if state.generation != generation {
                continue;
            }
            state.settled = Some((frame.hour, generation));
            shared.built.notify_all();
            drop(state);
            reader.wait_newer(published, TICK);
            continue;
        }
        if let Some((_, before, _)) = &last {
            state.builds.skipped += published.saturating_sub(before + 1);
        }
        last = Some((Arc::clone(&frame), published, generation));
        timeline::mark(rules.maker, Mark::Rows);
        let seen = state.seen.clone().expect("checked above");
        state.seen_hour = Some(frame.hour);
        let held = state.built.as_ref().cloned().filter(|built| built.generation == generation);
        let tables = state.tables.clone();
        drop(state);
        if let Some(held) = held {
            let checked = Instant::now();
            let stood = stands(build, &held, &frame, &tables, &seen);
            let mut state = shared.lock();
            state.builds.confirm_ns += checked.elapsed().as_nanos() as u64;
            if stood {
                held.current.set(frame.hour);
                state.builds.confirmed += 1;
                shared.notify_product(&state);
                continue;
            }
        }
        let behind = shared.lock().head.map(|head| u64::try_from(head - frame.hour).unwrap_or(0));
        timeline::mark(rules.maker, Mark::BuildBegan);
        let started = Instant::now();
        let (product, missed) = learning(build, &frame, &seen);
        let ns = started.elapsed().as_nanos() as u64;
        timeline::mark(rules.maker, Mark::BuildEnded);
        let mut state = shared.lock();
        if !missed.is_empty() {
            let absent = state.source.as_ref().map_or_else(Vec::new, |(entry, _)| absent(entry, &missed));
            if !absent.is_empty() {
                state.refused.set_if_changed(Some(refusal(&format!("the {} layer", rules.name), &absent)));
                shared.notify_product(&state);
                continue;
            }
            state.tables.extend(missed.iter().copied());
            state.tables.sort_unstable_by_key(|table| table.index());
            state.tables.dedup();
            state.awaiting = missed;
            state.builds.discovered += 1;
            state.generation += 1;
            reader.declare_only(&state.tables);
            drop(state);
            shared.built.notify_all();
            continue;
        }
        let builds = &mut state.builds;
        builds.record(started, ns, behind, rules.timed);
        if let Ok(product) = &product {
            build.parts(product, &mut |name, ns| builds.part(name, ns));
            build.counts(product, &mut |name, count| builds.count(name, count));
        }
        if state.open {
            state.built.set_some(Arc::new(Built::new(frame.hour, generation, product, ns, Some(frame), Some(seen))));
            shared.notify_product(&state);
        }
    }
}

pub struct Whole<B: Build> {
    name: &'static str,
    build: Arc<B>,
    drawn: fn(&B, &crate::layers::View) -> bool,
    bytes: fn(&B::Product) -> usize,
    rebuilt: Rebuilt<B::Product>,
    held: Option<Landed<Arc<Built<B::Product>>>>,
    open: bool,
    since: u64,
    marked: [usize; 2],
    window: Option<Window>,
    exact: bool,
    applied: Option<Hour>,
    behind: bool,
}

fn installing(owed: usize) -> Continuity {
    if owed > 0 { Continuity::Reset } else { Continuity::Continue }
}

#[derive(Default)]
struct Window {
    perspective: Option<Choice>,
    install: bool,
    owed: usize,
    empty: bool,
}

impl<B: Build> Whole<B> {
    pub fn new(name: &'static str, build: B, drawn: fn(&B, &crate::layers::View) -> bool, bytes: fn(&B::Product) -> usize, tables: &[TableId], timed: bool) -> io::Result<Self> {
        let build = Arc::new(build);
        let rebuilt = Rebuilt::new(name, Arc::clone(&build), tables, timed)?;
        Ok(Self { name, build, drawn, bytes, rebuilt, held: None, open: false, since: 0, marked: [0; 2], window: None, exact: false, applied: None, behind: false })
    }

    pub fn exact(mut self) -> Self {
        self.exact = true;
        self
    }

    pub fn in_window(mut self, entry: Arc<Entry>, recording: Recording) -> io::Result<Self> {
        self.rebuilt.attach(entry, recording)?;
        self.window = Some(Window::default());
        Ok(self)
    }

    pub fn shows(&self) -> bool {
        !self.open || self.held.is_some()
    }

    pub fn refusal(&self) -> Option<&str> {
        self.held.as_ref().filter(|_| self.open).and_then(|built| built.product.as_ref().err()).map(String::as_str)
    }

    pub fn rebuilt(&self) -> &Rebuilt<B::Product> {
        &self.rebuilt
    }

    pub fn build(&self) -> &B {
        &self.build
    }

    pub fn product(&self) -> Option<&Landed<Arc<Built<B::Product>>>> {
        self.held.as_ref()
    }

    pub fn drawn(&self) -> bool {
        self.open
    }

    pub fn marked(&self) -> [usize; 2] {
        self.marked
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn installing(&self) -> Option<Continuity> {
        let window = self.window.as_ref().filter(|window| window.install)?;
        Some(installing(window.owed))
    }

    pub fn again(&self, built: &Built<B::Product>) -> Option<io::Result<B::Product>> {
        let (frame, seen) = (built.frame.as_ref()?, built.seen.as_ref()?);
        Some(discovering(|| attempt(&*self.build, || self.build.anew(frame, seen))).0.map_err(io::Error::other))
    }
}

impl<B: Build> crate::layers::Layer for Whole<B> {
    fn retirement(&self) -> Option<Retirement> {
        let shared = Arc::clone(&self.rebuilt.shared);
        Some(Box::new(move || shared.stop()))
    }

    fn views(&mut self, store: &views::Store) -> io::Result<()> {
        for &table in self.build.exact_columns() {
            store.use_columns(table);
        }
        self.rebuilt.set_views(store);
        Ok(())
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn placed(&mut self, view: &crate::layers::View) -> Option<u64> {
        Some(u64::from((self.drawn)(&self.build, view)))
    }

    fn inputs(&mut self, view: &crate::layers::View, inputs: &mut crate::layers::Inputs<'_>) -> io::Result<()> {
        self.open = (self.drawn)(&self.build, view);
        if self.open {
            self.rebuilt.declare_rows(self.build.row_tables(view));
        }
        let mut renewed = None;
        if let Some(window) = &mut self.window
            && window.perspective.as_ref() != Some(&view.perspective)
        {
            let changed = window.perspective.replace(view.perspective.clone()).is_some();
            let generation = self.rebuilt.set_seen((view.perspective.clone(), None));
            renewed = changed.then_some(generation);
        }
        if self.build.viewed(view) {
            renewed = Some(self.rebuilt.renew());
        }
        if let Some(generation) = renewed {
            self.since = generation;
            if let Some(window) = self.window.as_mut().filter(|_| self.open) {
                window.owed += 1;
                window.install = !self.build.hold_picture();
            }
            if !self.build.hold_picture() && let Some(old) = self.held.take() {
                self.rebuilt.retire(old);
            }
        }
        self.rebuilt.set_open(self.open);
        if !self.open {
            if let Some(old) = self.held.take() {
                self.rebuilt.retire(old);
            }
            if let Some(window) = self.window.as_mut().filter(|window| !window.empty) {
                (window.install, window.owed) = (true, window.owed + 1);
            }
            return Ok(());
        }
        inputs.table(TableId::Clock, false)
    }

    fn compose(&mut self, _mirrors: &crate::playhead_host::mirror::Mirrors) -> io::Result<()> {
        self.since = self.rebuilt.renew();
        self.behind = false;
        let dropped = !self.build.hold_picture() && self.held.take().map(|old| self.rebuilt.retire(old)).is_some();
        if let Some(window) = &mut self.window {
            window.owed += 1;
            window.install |= dropped;
        }
        Ok(())
    }

    fn apply(&mut self, step: &playhead::Step, _mirrors: &crate::playhead_host::mirror::Mirrors) {
        self.rebuilt.applied(step.last());
        if self.open {
            self.marked[0] += 1;
        }
    }

    fn ask(&mut self, mirrors: &crate::playhead_host::mirror::Mirrors, _upstream: &crate::layers::Upstream<'_>) -> io::Result<bool> {
        if !self.open {
            return Ok(false);
        }
        if let Some(hour) = mirrors.get(TableId::Clock).and_then(|clock| clock.head()) {
            self.rebuilt.applied(hour);
        }
        Ok(true)
    }

    fn join(&mut self, mirrors: &crate::playhead_host::mirror::Mirrors, _upstream: &crate::layers::Upstream<'_>, settling: crate::layers::Settling) -> io::Result<()> {
        if !self.open {
            return Ok(());
        }
        let hour = mirrors.get(TableId::Clock).and_then(|clock| clock.head());
        self.applied = hour;
        if let Some(hour) = hour
            && self.window.is_some()
        {
            let waiting = match () {
                () if self.behind => Duration::ZERO,
                () if self.exact && self.held.as_ref().is_some_and(|built| built.generation >= self.since) => WHOLE,
                () => settling.waiting(WHOLE),
            };
            if !waiting.is_zero()
                && let Err(error) = self.rebuilt.wait_for(hour, waiting, true)
            {
                if error.kind() == io::ErrorKind::Interrupted {
                    return Err(error);
                }
                if self.exact && error.kind() != io::ErrorKind::WouldBlock && self.rebuilt.stopped().is_none() {
                    self.behind = true;
                }
            }
        }
        if let Some(why) = self.rebuilt.stopped() {
            let refused = self.held.as_ref().is_some_and(|built| built.product.as_ref().err() == Some(&*why));
            if let Some(hour) = hour
                && !(refused && self.held.as_ref().is_some_and(|built| built.hour == hour))
            {
                let since = self.since;
                if let Some(old) = self.held.replace(why.map(|why| Arc::new(Built::new(hour, since, Err(why), 0, None, None)))) {
                    self.rebuilt.retire(old);
                }
                if let Some(window) = self.window.as_mut().filter(|_| !refused) {
                    window.install = true;
                }
            }
            return Ok(());
        }
        if let Some(newer) = self.rebuilt.newer(self.held.as_deref()).filter(|built| built.generation >= self.since) {
            if let Some(old) = self.held.replace(newer) {
                self.rebuilt.retire(old);
            }
            self.marked[1] += 1;
            if let Some(window) = &mut self.window {
                window.install = true;
            }
        }
        if self.held.as_ref().zip(hour).is_some_and(|(built, hour)| built.of(hour)) {
            self.behind = false;
        }
        Ok(())
    }

    fn settle(&mut self, mirrors: &crate::playhead_host::mirror::Mirrors, settling: crate::layers::Settling) -> io::Result<()> {
        let upstream = crate::layers::Upstream::new(&[]);
        self.ask(mirrors, &upstream)?;
        self.join(mirrors, &upstream, settling)
    }

    fn upload(&mut self, gpu: &mut crate::layers::Gpu<'_>) -> io::Result<()> {
        let Some(window) = &mut self.window else { return Ok(()) };
        if !window.install {
            return Ok(());
        }
        let continuity = installing(window.owed);
        window.owed = window.owed.saturating_sub(1);
        let product = self.held.as_ref().filter(|_| self.open).and_then(|built| built.product.as_ref().ok());
        if let Some(replaced) = self.build.install(gpu, product, continuity)? {
            self.rebuilt.retire(replaced);
        }
        (window.install, window.empty) = (false, product.is_none());
        Ok(())
    }

    fn uploaded(&self) -> bool {
        self.window.as_ref().is_none_or(|window| !window.install)
    }

    fn mode_ready(&self) -> bool {
        !self.build.hold_picture() || !self.open || self.held.as_ref().is_some_and(|built| built.generation >= self.since)
    }

    fn carried(&self) -> crate::layers::Carried {
        crate::layers::Carried::default()
    }

    fn published(&self) -> Published {
        if !self.open {
            return Published::Current;
        }
        match &self.held {
            None => Published::Nothing,
            Some(built) if self.exact => match self.applied {
                Some(applied) if built.stands() < applied => Published::At(built.stands()),
                _ => Published::Current,
            },
            Some(built) => Published::Cadence(built.stands()),
        }
    }

    fn made_at(&self, hour: Hour) -> bool {
        self.window.is_none() || !self.open || self.held.as_ref().is_some_and(|built| {
            let state = self.rebuilt.shared.lock();
            built.of(hour) && (built.generation == state.generation || state.stopped.is_some())
        })
    }

    fn held(&self) -> usize {
        self.held.as_ref().and_then(|built| built.product.as_ref().ok()).map_or(0, self.bytes)
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    #[cfg(test)]
    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
pub mod tests;
