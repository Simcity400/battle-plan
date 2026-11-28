
use crate::{
    changes::{Changes, Texts},
    clock::Clock,
    invalid,
    lane::{
        Fetch, Fetched, Held, Lane, Sought,
        log::{self, Rows, Wanted},
    },
    pages::Pages,
    ring::Ring,
    state::{PendingStates, Flight, Rebuild, State, PendingState},
    step::{Built, Fetching, Rebuilt, Seeking, Step},
};
use replay::campaign::timelines;
use replay_columnar::unit::BLOCK;
use replay_data::{BranchId, Hour};
use std::{
    cmp::Ordering as Order,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    io,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(test)]
mod tests;

const SPLIT_NS: f64 = 500_000.0;

const FIRST_COST: f64 = 1_000.0;

const REPORT_EVERY: Duration = Duration::from_millis(100);

const PREPARE_SECONDS: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub step_threads: usize,
    pub fetch_threads: usize,
    pub ahead: u32,
    pub ring: usize,
    pub speed: f64,
    pub rate: f64,
    pub entering: Option<fn(Thread)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thread {
    Stepper,
    Step,
    Fetch,
    Seeking,
}

impl Default for Settings {
    fn default() -> Self {
        Self { step_threads: 1, fetch_threads: 1, ahead: 4, ring: 3, speed: 1440.0, rate: 60.0, entering: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub run: usize,
    pub first: Hour,
    pub last: Hour,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pieces: Vec<Piece>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Piece(usize),
    Gap { next: usize },
}

impl Timeline {
    pub fn new(pages: &dyn Pages, pieces: Vec<Piece>) -> io::Result<Self> {
        if pieces.is_empty() {
            return Err(invalid("A timeline holds no piece"));
        }
        for (at, piece) in pieces.iter().enumerate() {
            let run = pages.runs().get(piece.run).ok_or_else(|| invalid(format!("Piece {at} names run {}, which the recording does not hold", piece.run)))?;
            if piece.first > piece.last || piece.first < run.first_hour || piece.last > run.last_hour() {
                return Err(invalid(format!("Piece {at} reads hours {} to {} of a run of hours {} to {}", piece.first, piece.last, run.first_hour, run.last_hour())));
            }
            if at > 0 && piece.first <= pieces[at - 1].last {
                return Err(invalid(format!("Piece {at} starts at hour {}, not after the piece before it", piece.first)));
            }
        }
        Ok(Self { pieces })
    }

    pub fn whole(pages: &dyn Pages) -> io::Result<Self> {
        let pieces = pages.runs().iter().enumerate().map(|(run, held)| Piece { run, first: held.first_hour, last: held.last_hour() }).collect();
        Self::new(pages, pieces)
    }

    pub fn branch(pages: &dyn Pages, branches: &[BranchId], branch: BranchId, pieces: &[timelines::Piece]) -> io::Result<Self> {
        let mut cut = Vec::new();
        for piece in pieces {
            let from = cut.len();
            for (run, (held, &segment)) in pages.runs().iter().zip(branches).enumerate() {
                let (first, last) = (piece.first_hour.max(held.first_hour), piece.last_hour.min(held.last_hour()));
                if segment == piece.segment && first <= last {
                    cut.push(Piece { run, first, last });
                }
            }
            cut[from..].sort_unstable_by_key(|piece| piece.first);
        }
        if cut.is_empty() {
            return Err(io::Error::other(format!("Branch {branch} holds no readable hour of the recording")));
        }
        Self::new(pages, cut).map_err(|error| io::Error::new(error.kind(), format!("Branch {branch} of the recording: {error}")))
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub fn first(&self) -> Hour {
        self.pieces[0].first
    }

    pub fn last(&self) -> Hour {
        self.pieces[self.pieces.len() - 1].last
    }

    pub fn clamp(&self, hour: Hour) -> Hour {
        hour.clamp(self.first(), self.last())
    }

    fn region(&self, hour: Hour) -> Region {
        let at = self.pieces.partition_point(|piece| piece.last < hour).min(self.pieces.len() - 1);
        if self.pieces[at].first <= hour { Region::Piece(at) } else { Region::Gap { next: at } }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    pub table: usize,
    pub columns: Vec<usize>,
    pub keyed: bool,
    pub passing: bool,
    pub hourly: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reader(u64);

#[derive(Clone)]
struct Declaration {
    tables: Vec<Declared>,
    projections: BTreeMap<usize, Vec<usize>>,
    since: Vec<u64>,
    rebuild: Option<Rebuild>,
    states: PendingStates,
}

#[derive(Clone)]
struct Reading {
    reader: Reader,
    columns: Vec<usize>,
    keyed: bool,
    passing: bool,
    since: u64,
    rebuild: Option<Rebuild>,
}

impl Reading {
    fn same(&self, other: &Self) -> bool {
        (self.reader, &self.columns, self.keyed, self.passing, self.since) == (other.reader, &other.columns, other.keyed, other.passing, other.since)
    }
}

#[derive(Clone)]
struct Selected {
    columns: Vec<usize>,
    keyed: bool,
    passing: bool,
    hourly: bool,
    readings: Vec<Reading>,
}

impl Selected {
    fn rebuilds(&self, only: Option<&[Reader]>) -> Vec<(Reader, Vec<usize>, Rebuild)> {
        self.readings
            .iter()
            .filter(|reading| only.is_none_or(|only| only.contains(&reading.reader)))
            .filter_map(|reading| reading.rebuild.as_ref().map(|rebuild| (reading.reader, reading.columns.clone(), Arc::clone(rebuild))))
            .collect()
    }
}

pub struct Lanes<'a> {
    slots: &'a [Slot],
}

impl<'a> Lanes<'a> {
    pub fn get(&self, table: usize) -> Option<&'a Lane> {
        self.slots.binary_search_by_key(&table, |slot| slot.table).ok().map(|at| &*self.slots[at].lane)
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a Lane> + 'a {
        self.slots.iter().map(|slot| &*slot.lane)
    }
}

pub type Hook = Box<dyn FnMut(&Step, &Lanes<'_>) + Send>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeadHeld {
    pub steps: usize,
    pub past: usize,
    pub bytes: usize,
    pub capped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lead {
    pub seconds: f64,
    pub bytes: usize,
}

impl Lead {
    pub const NONE: Self = Self { seconds: 0.0, bytes: usize::MAX };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Report {
    pub lanes: usize,
    pub held: Held,
    pub ring_steps: usize,
    pub ring_bytes: usize,
    pub fetches_queued: usize,
    pub jobs_running: usize,
    pub freed_ns: u64,
    pub preparing: usize,
    pub preparing_held: Held,
    pub states: usize,
    pub state_bytes: usize,
    pub total: usize,
}

const IDLE_RING_FULL: usize = 0;
const IDLE_ROOM_BEHIND: usize = 1;
const IDLE_FETCH: usize = 2;
const IDLE_END: usize = 3;
const IDLE_OTHER: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub steps: u64,
    pub stalls: u64,
    pub stalled_steps: u64,
    pub piece_waits: u64,
    pub seeks: u64,
    pub joins: u64,
    pub piece_prepares: u64,
    pub joins_at_head: u64,
    pub join_holds: u64,
    pub catch_ups: u64,
    pub split_steps: u64,
    pub room_held_ns: u64,
    pub idle_ns: [u64; 5],
    pub join_held_ns: u64,
    pub landings: u64,
    pub landing_rebuild_ns: u64,
    pub landed_behind: u64,
    pub caught_up: u64,
    pub catch_up_ns: u64,
    pub catch_up_most_ns: u64,
    pub catch_waits: [u64; 4],
    pub standing_reads: u64,
    pub standing_dropped: u64,
    pub resteps: u64,
    pub join_waits: [u64; 6],
    pub join_answers_dropped: u64,
    pub joins_open: u64,
    pub ripe_checks: u64,
    pub pieces_crossed: u64,
    pub crossed_tables: u64,
    pub crossed_resets: u64,
    pub crossed_rows: u64,
    pub crossed_cells: u64,
    pub cross_ns: u64,
    pub cross_most_ns: u64,
    pub declarations_applied: u64,
}

struct Slot {
    table: usize,
    generation: u64,
    lane: Box<Lane>,
    fetching: bool,
    cost: f64,
    ends: VecDeque<(Hour, Option<Hour>)>,
}

impl Slot {
    fn new(generation: u64, lane: Box<Lane>) -> Self {
        Self { table: lane.table(), generation, lane, fetching: false, cost: FIRST_COST, ends: VecDeque::new() }
    }

    fn ended(&mut self, last: Hour, keep: usize) {
        if self.ends.len() >= keep {
            self.ends.pop_front();
        }
        self.ends.push_back((last, self.lane.head()));
    }

    fn head_at(&self, hour: Hour) -> Option<Hour> {
        self.ends.iter().rev().find(|(last, _)| *last <= hour).filter(|(last, _)| *last == hour).and_then(|&(_, head)| head)
    }

    fn whole_at(&self, hour: Hour) -> bool {
        self.ends.iter().rev().find(|(last, _)| *last <= hour).is_some_and(|&(last, head)| last == hour && head == Some(hour))
    }
}

#[derive(Clone, Copy)]
enum Fed {
    Slot(usize),
    Join,
    Restep,
    CatchUp(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Seek,
    Join,
    Piece(usize),
    Restep,
    CatchUp,
}

struct Prepare {
    epoch: u64,
    purpose: Purpose,
    table: usize,
    generation: u64,
    run: usize,
    columns: Vec<usize>,
    keyed: bool,
    passing: bool,
    hour: Hour,
    through: Hour,
    frames: Option<(Hour, Hour)>,
    rebuilds: Vec<(Reader, Vec<usize>, Rebuild)>,
}

struct Prepared {
    generation: u64,
    lane: Box<Lane>,
    states: Vec<(Reader, PendingState)>,
    sought: Sought,
    rebuild_ns: u64,
    ns: u64,
    ahead_ns: u64,
    thread: std::thread::ThreadId,
}

type Retired = Box<dyn Send>;

enum Job {
    Prepare(Prepare),
    Fetch { epoch: u64, table: usize, generation: u64, fetch: Fetch },
    Drop(Retired),
    Rows(Wanted, Reply),
}

#[derive(Default)]
struct Answer {
    rows: Mutex<Option<io::Result<Rows>>>,
    ready: Condvar,
}

struct Reply {
    answer: Option<Arc<Answer>>,
    on_done: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Reply {
    fn answer(answer: &Arc<Answer>, rows: io::Result<Rows>) {
        *lock(&answer.rows) = Some(rows);
        answer.ready.notify_all();
    }

    fn give(mut self, rows: io::Result<Rows>) {
        let on_done = self.on_done.take();
        if let Some(answer) = self.answer.take() {
            Self::answer(&answer, rows);
        }
        if let Some(on_done) = on_done {
            on_done();
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        let on_done = self.on_done.take();
        if let Some(answer) = self.answer.take() {
            Self::answer(&answer, Err(io::Error::other("The playhead stopped before it read the rows asked")));
        }
        if let Some(on_done) = on_done {
            on_done();
        }
    }
}

#[derive(Clone)]
pub struct LogReads {
    shared: Arc<Shared>,
}

impl LogReads {
    pub fn is_log(&self, table: usize) -> io::Result<bool> {
        log::is_log(&*self.shared.pages, table)
    }

    pub fn rows(&self, wanted: Wanted) -> Asked {
        self.rows_with(wanted, None)
    }

    pub fn rows_notifying(&self, wanted: Wanted, on_done: Arc<dyn Fn() + Send + Sync>) -> Asked {
        self.rows_with(wanted, Some(on_done))
    }

    fn rows_with(&self, wanted: Wanted, on_done: Option<Arc<dyn Fn() + Send + Sync>>) -> Asked {
        let answer = Arc::new(Answer::default());
        let reply = Reply { answer: Some(Arc::clone(&answer)), on_done };
        let stopped = lock(&self.shared.jobs).stop;
        if stopped {
            drop(reply);
        } else {
            self.shared.queue([((3, 0), Job::Rows(wanted, reply))]);
        }
        self.shared.listing.fetch_add(1, Ordering::AcqRel);
        Asked { answer, shared: Arc::downgrade(&self.shared) }
    }
}

pub struct Asked {
    answer: Arc<Answer>,
    shared: std::sync::Weak<Shared>,
}

impl Drop for Asked {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.listing.fetch_sub(1, Ordering::AcqRel);
            shared.nudge(|_| {});
        }
    }
}

impl Asked {
    pub fn take(&self) -> Option<io::Result<Rows>> {
        lock(&self.answer.rows).take()
    }

    pub fn wait(self) -> io::Result<Rows> {
        let mut rows = lock(&self.answer.rows);
        loop {
            if let Some(answer) = rows.take() {
                return answer;
            }
            rows = self.answer.ready.wait(rows).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

enum Done {
    Prepared { epoch: u64, purpose: Purpose, table: usize, generation: u64, result: io::Result<Box<Prepared>> },
    Fetched { epoch: u64, table: usize, generation: u64, result: io::Result<Fetched> },
}

struct Queued {
    rank: (u8, i64),
    seq: u64,
    job: Job,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.rank, self.seq) == (other.rank, other.seq)
    }
}

impl Eq for Queued {}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Order> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Order {
        (other.rank, other.seq).cmp(&(self.rank, self.seq))
    }
}

#[derive(Default)]
struct Jobs {
    heap: BinaryHeap<Queued>,
    joins: VecDeque<Job>,
    seq: u64,
    running: usize,
    lent_busy: usize,
    stop: bool,
    lent_stop: bool,
}

impl Jobs {
    fn push(&mut self, rank: (u8, i64), job: Job) {
        self.seq += 1;
        self.heap.push(Queued { rank, seq: self.seq, job });
    }
}

struct Control {
    version: u64,
    head: Hour,
    asks: BTreeMap<Option<usize>, Hour>,
    pace: f64,
    rate: f64,
    seek: Option<(Hour, u64)>,
    asked: Option<Instant>,
    readers: BTreeMap<Reader, Declaration>,
    next_reader: u64,
    selection: u64,
    hook_requested: bool,
    seed_paused: bool,
    lands: Vec<usize>,
    done: Vec<Done>,
    newest: Option<Hour>,
    published_steps: u64,
    caught_up_at: Option<Hour>,
    stood: u64,
    stop: bool,
    error: Option<String>,
    holding_reads: bool,
}

struct Shared {
    pages: Arc<dyn Pages>,
    timeline: Timeline,
    settings: Settings,
    epoch: AtomicU64,
    freed_ns: AtomicU64,
    activity: AtomicU64,
    control_seq: AtomicU64,
    next_watcher: AtomicU64,
    watchers: Mutex<Vec<(u64, bool, Arc<dyn Fn() + Send + Sync>)>>,
    control: Mutex<Control>,
    wake: Condvar,
    jobs: Mutex<Jobs>,
    work: Condvar,
    lent_work: Condvar,
    #[cfg(test)]
    lent_live: AtomicUsize,
    ring: Mutex<Ring>,
    clock: Mutex<Clock>,
    flight: Arc<Flight>,
    report: Mutex<(Report, Report, Counters)>,
    lead: Mutex<Lead>,
    listing: AtomicUsize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

std::thread_local! {
    static HELD_CLOCK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct ClockGuard<'a> {
    prev: usize,
    guard: MutexGuard<'a, Clock>,
}

impl std::ops::Deref for ClockGuard<'_> {
    type Target = Clock;

    fn deref(&self) -> &Clock {
        &self.guard
    }
}

impl std::ops::DerefMut for ClockGuard<'_> {
    fn deref_mut(&mut self) -> &mut Clock {
        &mut self.guard
    }
}

impl Drop for ClockGuard<'_> {
    fn drop(&mut self) {
        HELD_CLOCK.set(self.prev);
    }
}

fn lock_clock(clock: &Mutex<Clock>) -> ClockGuard<'_> {
    let guard = lock(clock);
    let prev = HELD_CLOCK.replace(std::ptr::from_ref(clock) as usize);
    ClockGuard { prev, guard }
}

fn try_lock<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(held) => Some(held),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

impl Control {
    fn ask(&mut self, who: Option<usize>, head: Hour) {
        self.asks.insert(who, head);
        let furthest = self.asks.values().copied().max().unwrap_or(head);
        if furthest != self.head {
            self.head = furthest;
        } else if head == furthest {
            self.stood += 1;
        }
        self.version += 1;
    }
}

impl Shared {
    fn try_nudge(&self) {
        if let Some(mut control) = try_lock(&self.control) {
            control.version += 1;
            drop(control);
            self.wake.notify_all();
        }
    }

    fn notify_control(&self) {
        self.control_seq.fetch_add(1, Ordering::Release);
        self.activity.fetch_add(1, Ordering::Release);
        let watchers: Vec<Arc<dyn Fn() + Send + Sync>> = lock(&self.watchers).iter().map(|(_, _, wake)| Arc::clone(wake)).collect();
        for wake in watchers {
            wake();
        }
    }

    fn notify_published(&self) {
        self.activity.fetch_add(1, Ordering::Release);
        let watchers: Vec<Arc<dyn Fn() + Send + Sync>> = lock(&self.watchers)
            .iter()
            .filter_map(|(_, on_publish, wake)| on_publish.then(|| Arc::clone(wake)))
            .collect();
        for wake in watchers {
            wake();
        }
    }

    fn nudge(&self, change: impl FnOnce(&mut Control)) {
        let mut control = lock(&self.control);
        change(&mut control);
        control.version += 1;
        drop(control);
        self.wake.notify_all();
    }

    fn ask(&self, who: Option<usize>, head: Hour) -> bool {
        let head = self.timeline.clamp(head);
        let Some(mut control) = try_lock(&self.control) else {
            return false;
        };
        control.ask(who, head);
        drop(control);
        self.wake.notify_all();
        true
    }

    fn seek(&self, hour: Hour) {
        {
            let mut clock = lock_clock(&self.clock);
            self.seek_locked(&mut clock, hour);
        }
        self.notify_control();
    }

    fn seek_locked(&self, clock: &mut Clock, hour: Hour) {
        let hour = self.timeline.clamp(hour);
        let epoch = lock(&self.ring).abandon();
        self.epoch.store(epoch, Ordering::Release);
        clock.seek(hour, epoch);
        self.nudge(|control| {
            control.seek = Some((hour, epoch));
            control.asked = Some(Instant::now());
            control.head = hour;
            control.asks.clear();
            control.newest = None;
            control.caught_up_at = None;
            control.seed_paused = false;
            if !control.stop {
                control.error = None;
            }
        });
    }

    fn resize(&self) {
        let (speed, rate) = { let control = lock(&self.control); (control.pace, control.rate) };
        let Lead { seconds, bytes, .. } = *lock(&self.lead);
        let quantum = ((speed / rate).round() as Hour).max(1);
        let steps = ((seconds * speed / f64::from(quantum)).ceil() as usize).max(self.settings.ring);
        lock(&self.ring).resize(steps, bytes);
        self.nudge(|_| {});
    }

    fn leads(&self) -> bool {
        lock(&self.lead).seconds > 0.0
    }

    fn queue(&self, jobs: impl IntoIterator<Item = ((u8, i64), Job)>) -> (usize, usize, usize) {
        let mut held = lock(&self.jobs);
        let before = held.heap.len();
        let mut urgent = false;
        for (rank, job) in jobs {
            urgent |= rank.0 <= 1;
            held.push(rank, job);
        }
        let added = held.heap.len() - before;
        let load = if urgent {
            (held.running, held.lent_busy, held.heap.iter().filter(|queued| queued.rank.0 <= 1 && matches!(queued.job, Job::Prepare(_))).count())
        } else {
            (held.running, held.lent_busy, 0)
        };
        drop(held);
        match added {
            0 => {}
            1 => self.work.notify_one(),
            _ => self.work.notify_all(),
        }
        if urgent {
            self.lent_work.notify_all();
        }
        load
    }

    fn queue_joins(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        lock(&self.jobs).joins.extend(jobs);
        self.work.notify_all();
        self.lent_work.notify_all();
    }

    fn free(&self, retired: impl Send + 'static) {
        self.queue([((0, 0), Job::Drop(Box::new(retired)))]);
    }

    fn hold(&self, reader: Reader, state: State) -> Option<PendingState> {
        let control = lock(&self.control);
        Some(state.hold(&self.flight, &control.readers.get(&reader)?.states))
    }

    fn free_after_prepares(&self, retired: impl Send + 'static) {
        self.queue([((1, i64::MAX), Job::Drop(Box::new(retired)))]);
    }

    fn pop_urgent(&self) -> Option<Job> {
        let mut jobs = lock(&self.jobs);
        if jobs.stop || jobs.heap.peek().is_none_or(|queued| queued.rank.0 > 1) {
            return None;
        }
        jobs.running += 1;
        Some(jobs.heap.pop().expect("peeked").job)
    }

    fn run(&self, job: Job) -> Option<Done> {
        struct Running<'a>(&'a Shared);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                lock(&self.0.jobs).running -= 1;
            }
        }
        struct OnPanic<'a> {
            shared: &'a Shared,
            done: Option<Done>,
        }
        impl Drop for OnPanic<'_> {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    if let Some(done) = self.done.take() {
                        self.shared.nudge(|control| control.done.push(done));
                    }
                    self.shared.work.notify_all();
                    self.shared.lent_work.notify_all();
                }
            }
        }
        let mut guard = OnPanic { shared: self, done: None };
        let _running = Running(self);
        match job {
            Job::Prepare(prepare) if prepare.epoch != self.epoch.load(Ordering::Acquire) => None,
            Job::Prepare(prepare) => {
                let (epoch, purpose, table, generation) = (prepare.epoch, prepare.purpose, prepare.table, prepare.generation);
                guard.done = Some(Done::Prepared {
                    epoch,
                    purpose,
                    table,
                    generation,
                    result: Err(io::Error::other("playhead worker panicked")),
                });
                let result = self.prepare(&prepare);
                guard.done = None;
                Some(Done::Prepared { epoch, purpose, table, generation, result })
            }
            Job::Fetch { epoch, fetch, .. } if epoch != self.epoch.load(Ordering::Acquire) => {
                drop(fetch);
                None
            }
            Job::Fetch { epoch, table, generation, fetch } => {
                guard.done = Some(Done::Fetched {
                    epoch,
                    table,
                    generation,
                    result: Err(io::Error::other("playhead worker panicked")),
                });
                let result = fetch.run();
                guard.done = None;
                Some(Done::Fetched { epoch, table, generation, result })
            }
            Job::Rows(wanted, reply) => {
                reply.give(log::rows(&*self.pages, &wanted));
                None
            }
            Job::Drop(retired) => {
                let started = Instant::now();
                drop(retired);
                self.freed_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                None
            }
        }
    }
}

impl Shared {
    fn prepare(&self, prepare: &Prepare) -> io::Result<Box<Prepared>> {
        let started = Instant::now();
        let (hour, through) = match prepare.frames {
            Some((latest, span)) => {
                let hour = lock(&self.ring).frames().map_or(prepare.hour, |(_, hour)| hour.clamp(prepare.hour, latest));
                (hour, hour + span)
            }
            None => (prepare.hour, prepare.through),
        };
        let mut lane = Box::new(Lane::new(Arc::clone(&self.pages), prepare.run, prepare.table, &prepare.columns, prepare.keyed)?);
        lane.set_passing(prepare.passing);
        let mut sought = lane.seek(hour)?;
        let (ahead_ns, _) = lane.fetch_through(through)?;
        sought.fetch_ns += ahead_ns;
        let mut states = Vec::with_capacity(prepare.rebuilds.len());
        let mut rebuild_ns = 0;
        for (reader, columns, rebuild) in &prepare.rebuilds {
            if self.epoch.load(Ordering::Acquire) != prepare.epoch {
                break;
            }
            let began = Instant::now();
            let state = rebuild(&lane, columns)?;
            rebuild_ns += began.elapsed().as_nanos() as u64;
            if let Some(state) = self.hold(*reader, state) {
                states.push((*reader, state));
            }
        }
        Ok(Box::new(Prepared { generation: prepare.generation, lane, states, sought, rebuild_ns, ns: started.elapsed().as_nanos() as u64, ahead_ns, thread: std::thread::current().id() }))
    }
}

fn seek_cost(pages: &dyn Pages, run: usize, table: usize, hour: Hour) -> i64 {
    let held = &pages.runs()[run];
    let target = (hour - held.first_hour).max(0) as u32;
    let block = ((target / BLOCK) as usize).min(held.blocks() - 1);
    let mut restored = None;
    let mut bytes = 0u64;
    for at in (0..=block).rev() {
        let Ok(entry) = pages.table_entry(run, at, table) else { return 0 };
        bytes += entry.parts.iter().flat_map(|part| &part.frames).map(|frame| u64::from(frame.stored)).sum::<u64>();
        if let Some(restore) = entry.restore.as_ref().or(entry.first.as_ref()) {
            bytes += restore.streams.iter().flat_map(|(_, chunks)| chunks).map(|chunk| u64::from(chunk.frame.stored) + chunk.prefix.as_ref().map_or(0, |frame| u64::from(frame.stored))).sum::<u64>();
            restored = Some(at);
            break;
        }
    }
    if restored.is_none() {
        return 0;
    }
    bytes as i64
}

struct Batch {
    to: Hour,
    queue: Mutex<Vec<Item>>,
    done: Mutex<Vec<Item>>,
    finished: Condvar,
    left: AtomicUsize,
    threads: AtomicUsize,
}

struct Item {
    index: usize,
    predicted: f64,
    slot: Slot,
    changes: Changes,
    hours: Option<Vec<Changes>>,
    result: io::Result<()>,
}

impl Batch {
    fn work(&self) {
        let mut took = false;
        loop {
            let Some(mut item) = lock(&self.queue).pop() else { break };
            took = true;
            item.result = step_slot(&mut item.slot, self.to, &mut item.changes, item.hours.as_mut());
            let mut done = lock(&self.done);
            done.push(item);
            let last = self.left.fetch_sub(1, Ordering::AcqRel) == 1;
            drop(done);
            if last {
                self.finished.notify_all();
            }
        }
        if took {
            self.threads.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn wait(&self) {
        let mut done = lock(&self.done);
        while self.left.load(Ordering::Acquire) > 0 {
            done = self.finished.wait(done).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

fn step_slot(slot: &mut Slot, to: Hour, changes: &mut Changes, hours: Option<&mut Vec<Changes>>) -> io::Result<()> {
    let from = slot.lane.head().unwrap_or(to);
    let started = Instant::now();
    slot.lane.step_hours(to, changes, hours)?;
    let ns = started.elapsed().as_nanos() as f64;
    let hours = (to - from).max(1) as f64;
    slot.cost = slot.cost * 0.75 + ns / hours * 0.25;
    Ok(())
}

enum Help {
    Batch(Arc<Batch>),
    Prepares,
}

struct StepperExit {
    shared: Arc<Shared>,
}

impl Drop for StepperExit {
    fn drop(&mut self) {
        let panicked = std::thread::panicking();
        self.shared.nudge(|control| {
            control.stop = true;
            if panicked && control.error.is_none() {
                control.error = Some("playhead stepper panicked".into());
            }
        });
        self.shared.notify_control();
    }
}

pub struct Playhead {
    shared: Arc<Shared>,
    stepper: Option<JoinHandle<Vec<Lane>>>,
    threads: Vec<JoinHandle<()>>,
}

pub struct Consumer {
    shared: Arc<Shared>,
    id: usize,
    reader: Reader,
    asked: std::sync::atomic::AtomicBool,
    blocking: bool,
}

impl Consumer {
    pub fn set_blocking(&mut self, blocking: bool) -> bool {
        std::mem::replace(&mut self.blocking, blocking)
    }

    fn lock_ring(&self) -> Option<MutexGuard<'_, Ring>> {
        if self.blocking {
            return Some(lock(&self.shared.ring));
        }
        try_lock(&self.shared.ring)
    }

    fn take_from(&self, mut ring: MutexGuard<'_, Ring>, head: Hour, out: &mut Vec<Arc<Step>>) -> usize {
        let from = out.len();
        let taken = ring.take(self.id, head, out);
        drop(ring);
        if taken > 0 {
            let released = if let Some(reset) = out[from..].iter().find(|step| step.reset) {
                let now = Instant::now();
                let mut clock = lock_clock(&self.shared.clock);
                let was_held = clock.held().is_some();
                clock.shown(reset.epoch, now);
                was_held && clock.held().is_none()
            } else {
                false
            };
            self.shared.try_nudge();
            if released {
                self.shared.notify_control();
            }
        }
        taken
    }

    fn ask_blocking(&self, head: Hour) {
        let head = self.shared.timeline.clamp(head);
        let mut control = lock(&self.shared.control);
        control.ask(Some(self.id), head);
        drop(control);
        self.shared.wake.notify_all();
        if !self.asked.load(Ordering::Relaxed) {
            lock(&self.shared.ring).asks(self.id);
            self.asked.store(true, Ordering::Relaxed);
        }
    }

    pub fn try_take(&mut self, head: Hour, out: &mut Vec<Arc<Step>>) -> Option<usize> {
        let ring = self.lock_ring()?;
        Some(self.take_from(ring, head, out))
    }

    pub fn take(&mut self, head: Hour, out: &mut Vec<Arc<Step>>) -> usize {
        self.try_take(head, out).unwrap_or(0)
    }

    pub fn ahead(&self, out: &mut Vec<Arc<Step>>) {
        if let Some(ring) = self.lock_ring() {
            ring.ahead(self.id, out);
        }
    }

    pub fn try_advance(&self, head: Hour) -> bool {
        if self.blocking {
            self.ask_blocking(head);
            return true;
        }
        if !self.shared.ask(Some(self.id), head) {
            return false;
        }
        if !self.asked.load(Ordering::Relaxed) {
            let Some(mut ring) = try_lock(&self.shared.ring) else {
                return false;
            };
            ring.asks(self.id);
            self.asked.store(true, Ordering::Relaxed);
        }
        true
    }

    pub fn advance(&self, head: Hour) {
        let _ = self.try_advance(head);
    }

    pub fn lands_itself(&self) {
        lock(&self.shared.ring).lands_itself(self.id);
    }

    pub fn reaches(&self, head: Hour) -> Option<bool> {
        self.lock_ring().map(|ring| ring.reaches(self.id, head))
    }

    pub fn next_hour(&self) -> Option<Hour> {
        lock(&self.shared.ring).next_hour(self.id)
    }

    pub fn land(&mut self) {
        let id = self.id;
        self.shared.nudge(|control| {
            control.lands.push(id);
            control.caught_up_at = None;
        });
        self.shared.notify_control();
    }

    pub fn state(&self, step: &Step, table: usize) -> Option<State> {
        let mut states = lock(&step.states);
        let at = states.iter().position(|held| held.reader == self.reader && held.table == table)?;
        let mut state = states.swap_remove(at).state.take()?;
        drop(states);
        state.release();
        Some(state)
    }

    pub fn reader(&self) -> Reader {
        self.reader
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Consumer {
    pub fn take_blocking(&mut self, head: Hour, out: &mut Vec<Arc<Step>>) -> usize {
        let ring = lock(&self.shared.ring);
        self.take_from(ring, head, out)
    }

    pub fn advance_blocking(&self, head: Hour) {
        self.ask_blocking(head);
    }

    pub fn wait_step_after(&self, before: (Option<Hour>, u64), cap: Hour) -> Result<bool, String> {
        loop {
            let seen = lock(&self.shared.control).version;
            if lock(&self.shared.ring).has_ready(self.id, cap) {
                return Ok(true);
            }
            let mut control = lock(&self.shared.control);
            if let Some(error) = &control.error {
                return Err(error.clone());
            }
            if control.stop {
                return Err("the playhead stopped".into());
            }
            if control.published_steps > before.1 {
                return Ok(true);
            }
            if control.version != seen {
                continue;
            }
            if control.newest.is_some_and(|newest| newest >= cap)
                && control.caught_up_at.is_some_and(|at| at >= cap)
            {
                return Ok(false);
            }
            while control.version == seen {
                if let Some(error) = &control.error {
                    return Err(error.clone());
                }
                if control.stop {
                    return Err("the playhead stopped".into());
                }
                control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        lock(&self.shared.ring).leave(self.id);
        let id = self.id;
        self.shared.nudge(|control| {
            control.asks.remove(&Some(id));
        });
    }
}

pub struct Watcher {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        lock(&self.shared.watchers).retain(|(id, _, _)| *id != self.id);
    }
}

impl Playhead {
    pub fn new(pages: Arc<dyn Pages>, timeline: Timeline, settings: Settings, hook: Option<Hook>) -> io::Result<Self> {
        if settings.step_threads == 0 || settings.fetch_threads == 0 || settings.ring == 0 {
            return Err(invalid("A playhead needs a thread of each kind and a ring of one step"));
        }
        if !(settings.speed > 0.0 && settings.rate > 0.0) {
            return Err(invalid("A playhead's speed and publication rate are positive"));
        }
        let (first, last) = (timeline.first(), timeline.last());
        let shared = Arc::new(Shared {
            pages,
            timeline,
            settings,
            epoch: AtomicU64::new(0),
            freed_ns: AtomicU64::new(0),
            activity: AtomicU64::new(0),
            control_seq: AtomicU64::new(0),
            next_watcher: AtomicU64::new(0),
            watchers: Mutex::default(),
            control: Mutex::new(Control {
                version: 0,
                head: first,
                asks: BTreeMap::new(),
                pace: settings.speed, rate: settings.rate,
                seek: None,
                asked: None,
                readers: BTreeMap::new(),
                next_reader: 0,
                selection: 0,
                hook_requested: false,
                seed_paused: false,
                lands: Vec::new(),
                done: Vec::new(),
                newest: None,
                published_steps: 0,
                caught_up_at: None,
                stood: 0,
                stop: false,
                error: None,
                holding_reads: false,
            }),
            wake: Condvar::new(),
            jobs: Mutex::new(Jobs::default()),
            work: Condvar::new(),
            lent_work: Condvar::new(),
            #[cfg(test)]
            lent_live: AtomicUsize::new(0),
            ring: Mutex::new(Ring::new(settings.ring)),
            clock: Mutex::new(Clock::new(first, last, settings.speed)),
            flight: Arc::new(Flight::default()),
            report: Mutex::new(Default::default()),
            lead: Mutex::new(Lead::NONE),
            listing: AtomicUsize::new(0),
        });
        let mut threads = Vec::new();
        let entering = settings.entering;
        let entered = move |thread: Thread| {
            if let Some(entering) = entering {
                entering(thread);
            }
        };
        let spawn = |name: String, work: Box<dyn FnOnce() + Send>| std::thread::Builder::new().name(name).spawn(work);
        for at in 0..settings.fetch_threads {
            let held = Arc::clone(&shared);
            threads.push(spawn(format!("playhead fetch {at}"), Box::new(move || {
                entered(Thread::Fetch);
                fetcher(&held);
            }))?);
        }
        let mut helpers = Vec::new();
        for at in 1..settings.step_threads {
            let (send, receive) = mpsc::channel();
            helpers.push(send);
            let held = Arc::clone(&shared);
            threads.push(spawn(format!("playhead step {at}"), Box::new(move || {
                entered(Thread::Step);
                helper(&held, &receive);
            }))?);
        }
        let held = Arc::clone(&shared);
        let stepper = std::thread::Builder::new().name("playhead stepper".into()).spawn(move || {
            let _exit = StepperExit { shared: Arc::clone(&held) };
            entered(Thread::Stepper);
            Stepper::new(held, hook, helpers).run()
        })?;
        Ok(Self { shared, stepper: Some(stepper), threads })
    }

    pub fn timeline(&self) -> &Timeline {
        &self.shared.timeline
    }

    pub fn declare(&self, tables: Vec<Declared>, rebuild: Option<Rebuild>) -> io::Result<Reader> {
        self.declare_projected(tables, rebuild, BTreeMap::new())
    }

    pub fn declare_projected(&self, tables: Vec<Declared>, rebuild: Option<Rebuild>, projections: BTreeMap<usize, Vec<usize>>) -> io::Result<Reader> {
        self.check(&tables)?;
        let mut id = 0;
        self.shared.nudge(|control| {
            id = control.next_reader;
            control.next_reader += 1;
            control.selection += 1;
            control.caught_up_at = None;
            let since = vec![control.selection; tables.len()];
            control.readers.insert(Reader(id), Declaration { tables, projections, since, rebuild, states: PendingStates::default() });
        });
        self.shared.notify_control();
        Ok(Reader(id))
    }

    pub fn redeclare(&self, reader: Reader, tables: Vec<Declared>) -> io::Result<()> {
        self.redeclare_seeding(reader, tables, false)
    }

    pub fn redeclare_seeding(&self, reader: Reader, tables: Vec<Declared>, seed_paused: bool) -> io::Result<()> {
        self.redeclare_projected(reader, tables, BTreeMap::new(), seed_paused)
    }

    pub fn redeclare_projected(&self, reader: Reader, tables: Vec<Declared>, projections: BTreeMap<usize, Vec<usize>>, seed_paused: bool) -> io::Result<()> {
        if self.shared.leads() {
            let clock = seed_paused.then(|| lock_clock(&self.shared.clock));
            let mut control = lock(&self.shared.control);
            if control.readers.get(&reader).is_some_and(|held| held.tables == tables && held.projections == projections) {
                control.hook_requested = true;
                if clock.as_ref().is_some_and(|clock| !clock.playing()) {
                    control.seed_paused = true;
                }
                control.version += 1;
                drop(control);
                drop(clock);
                self.shared.wake.notify_all();
                self.shared.notify_control();
                return Ok(());
            }
        }
        self.check(&tables)?;
        let mut found = false;
        let clock = seed_paused.then(|| lock_clock(&self.shared.clock));
        self.shared.nudge(|control| {
            let stamp = control.selection + 1;
            if let Some(held) = control.readers.get_mut(&reader) {
                held.since = tables.iter().map(|declared| {
                    if held.projections.get(&declared.table) != projections.get(&declared.table) {
                        return stamp;
                    }
                    held.tables.iter().position(|before| before == declared).map_or(stamp, |at| held.since[at])
                }).collect();
                (held.tables, held.projections) = (tables, projections);
                control.selection = stamp;
                control.caught_up_at = None;
                if clock.as_ref().is_some_and(|clock| !clock.playing()) {
                    control.seed_paused = true;
                }
                found = true;
            }
        });
        drop(clock);
        if found {
            self.shared.notify_control();
            Ok(())
        } else {
            Err(invalid(format!("Reader {} is retired", reader.0)))
        }
    }

    fn check(&self, tables: &[Declared]) -> io::Result<()> {
        for (at, declared) in tables.iter().enumerate() {
            let kinds = self.shared.pages.kinds(declared.table).ok_or_else(|| invalid(format!("No table {} in the schema", declared.table)))?;
            if let Some(column) = declared.columns.iter().find(|&&column| column >= kinds.len()) {
                return Err(invalid(format!("Table {} holds no column {column}", declared.table)));
            }
            if tables[..at].iter().any(|before| before.table == declared.table) {
                return Err(invalid(format!("Table {} is declared twice by one reader", declared.table)));
            }
            if declared.passing && log::is_log(&*self.shared.pages, declared.table)? {
                return Err(invalid(format!("Table {} is a log: every row it appends passes once, in its steps' appended rows; it is not declared passing", declared.table)));
            }
        }
        Ok(())
    }

    pub fn rows(&self, wanted: Wanted) -> Asked {
        self.log_reads().rows(wanted)
    }

    pub fn log_reads(&self) -> LogReads {
        LogReads { shared: Arc::clone(&self.shared) }
    }

    pub fn retire(&self, reader: Reader) {
        lock(&self.shared.ring).forget(reader);
        let mut states = BTreeMap::new();
        self.shared.nudge(|control| {
            if let Some(held) = control.readers.remove(&reader) {
                states = held.states.release();
                control.selection += 1;
                control.caught_up_at = None;
            }
        });
        if !states.is_empty() {
            self.shared.free(states);
        }
        self.shared.notify_control();
    }

    pub fn consumer(&self, reader: Reader) -> Consumer {
        let id = lock(&self.shared.ring).join(reader);
        Consumer {
            shared: Arc::clone(&self.shared),
            id,
            reader,
            asked: std::sync::atomic::AtomicBool::new(false),
            blocking: false,
        }
    }

    pub fn head(&self, now: Instant) -> Hour {
        lock_clock(&self.shared.clock).frame(now)
    }

    pub fn clock_state_at(&self, now: Instant) -> (u64, Hour, Option<f64>) {
        let mut clock = lock_clock(&self.shared.clock);
        let speed = (clock.playing() && clock.held().is_none()).then(|| clock.speed());
        (self.epoch(), clock.frame(now), speed)
    }

    pub fn head_from(&self, at: Hour, by: Duration) -> Option<Hour> {
        let last = self.shared.timeline.last();
        let clock = lock_clock(&self.shared.clock);
        if !clock.playing() || clock.held().is_some() || at >= last {
            return None;
        }
        Some(at.saturating_add((clock.speed() * by.as_secs_f64()) as Hour).min(last))
    }

    pub fn anchor(&self, position: f64, playing: bool, speed: f64, at: Instant, sought: bool) {
        let mut clock = lock_clock(&self.shared.clock);
        if sought {
            self.shared.seek_locked(&mut clock, position.floor() as Hour);
        }
        clock.anchor(position, playing, speed, at);
        drop(clock);
        self.shared.nudge(|control| {
            if playing {
                control.seed_paused = false;
            }
        });
        self.shared.notify_control();
    }

    pub fn frame_rate(&self, rate: f64) {
        if !(rate.is_finite() && rate > 0.0) { return; }
        let mut control = lock(&self.shared.control);
        if control.rate == rate { return; }
        control.rate = rate;
        control.version += 1;
        drop(control);
        self.shared.resize();
        self.shared.notify_control();
    }

    pub fn step_hours(&self) -> Hour {
        let control = lock(&self.shared.control);
        ((control.pace / control.rate).round() as Hour).max(1)
    }

    pub fn frame_period(&self) -> Duration {
        let rate = lock(&self.shared.control).rate;
        Duration::from_secs_f64((1.0 / rate).max(0.001))
    }

    pub fn playing(&self) -> bool {
        let clock = lock_clock(&self.shared.clock);
        clock.playing() && clock.held().is_none()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn clock_held_on_current_thread(&self) -> bool {
        HELD_CLOCK.get() == std::ptr::from_ref(&self.shared.clock) as usize
    }

    pub fn activity(&self) -> u64 {
        self.shared.activity.load(Ordering::Acquire)
    }

    pub fn control_seq(&self) -> u64 {
        self.shared.control_seq.load(Ordering::Acquire)
    }

    pub fn watch(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Watcher {
        let id = self.shared.next_watcher.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.watchers).push((id, true, wake));
        Watcher { shared: Arc::clone(&self.shared), id }
    }

    pub fn watch_control(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Watcher {
        let id = self.shared.next_watcher.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.watchers).push((id, false, wake));
        Watcher { shared: Arc::clone(&self.shared), id }
    }

    pub fn play(&self, speed: f64) {
        if speed > 0.0 {
            lock_clock(&self.shared.clock).play(speed, Instant::now());
            self.shared.nudge(|control| {
                control.pace = speed;
                control.seed_paused = false;
            });
            if self.shared.leads() { self.shared.resize(); }
            self.shared.notify_control();
        }
    }

    pub fn pace(&self, speed: f64) {
        if speed > 0.0 {
            self.shared.nudge(|control| control.pace = speed);
            if self.shared.leads() { self.shared.resize(); }
            self.shared.notify_control();
        }
    }

    pub fn lead(&self, lead: Lead) {
        *lock(&self.shared.lead) = Lead { seconds: lead.seconds.max(0.0), ..lead };
        self.shared.resize();
    }

    pub fn share_budget(&self, budget: Arc<crate::ByteBudget>) -> io::Result<()> {
        lock(&self.shared.ring).share_budget(budget)
    }

    pub fn lead_held(&self) -> LeadHeld {
        let head = lock(&self.shared.control).head;
        let ring = lock(&self.shared.ring);
        let (past, bytes, capped) = ring.lead(head);
        LeadHeld { steps: ring.size(), past, bytes, capped }
    }

    pub fn pause(&self) {
        lock_clock(&self.shared.clock).pause(Instant::now());
        self.shared.nudge(|_| {});
        self.shared.notify_control();
    }

    pub fn seed_paused_query(&self) {
        let clock = lock_clock(&self.shared.clock);
        if !clock.playing() {
            self.shared.nudge(|control| control.seed_paused = true);
        }
    }

    pub fn paused_seed_pending(&self) -> bool {
        lock(&self.shared.control).seed_paused
    }

    pub fn holding(&self) -> Option<u64> {
        lock_clock(&self.shared.clock).held()
    }

    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::Acquire)
    }

    pub fn freed_ns(&self) -> u64 {
        self.shared.freed_ns.load(Ordering::Relaxed)
    }

    pub fn advance(&self, head: Hour) {
        let _asked = self.shared.ask(None, head);
    }

    pub fn stand(&self) {
        let mut control = lock(&self.shared.control);
        control.stood += 1;
        control.version += 1;
        drop(control);
        self.shared.wake.notify_all();
    }

    pub fn wake_waiters(&self) {
        let mut control = lock(&self.shared.control);
        control.version += 1;
        drop(control);
        self.shared.wake.notify_all();
    }

    pub fn place(&self, hour: Hour) {
        let hour = self.shared.timeline.clamp(hour);
        let mut clock = lock_clock(&self.shared.clock);
        if clock.held().is_some() {
            drop(clock);
            return self.shared.seek(hour);
        }
        clock.place(hour, Instant::now());
        drop(clock);
        self.shared.nudge(|_| {});
        self.shared.notify_control();
    }

    pub fn seek(&self, hour: Hour) {
        self.shared.seek(hour);
    }

    pub fn newest(&self) -> Option<Hour> {
        lock(&self.shared.control).newest
    }

    pub fn published(&self) -> Option<(Hour, u64)> {
        lock(&self.shared.ring).published_now()
    }

    #[cfg(feature = "test-support")]
    pub fn wait_newest(&self, hour: Hour) -> Result<(), String> {
        let mut control = lock(&self.shared.control);
        loop {
            if control.newest.is_some_and(|newest| newest >= hour) {
                return Ok(());
            }
            if let Some(error) = &control.error {
                return Err(error.clone());
            }
            if control.stop {
                return Err("the playhead stopped".into());
            }
            control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn holding_reads(&self) -> bool {
        lock(&self.shared.control).holding_reads
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn step_state(&self) -> (Option<Hour>, u64) {
        let control = lock(&self.shared.control);
        (control.newest, control.published_steps)
    }

    #[cfg(feature = "test-support")]
    pub fn wait_declarations(&self) -> Result<(), String> {
        loop {
            let version = lock(&self.shared.control).version;
            let (declared, counters) = (self.declarations(), self.held().2);
            if counters.declarations_applied == declared && counters.joins_open == 0 {
                return Ok(());
            }
            let mut control = lock(&self.shared.control);
            while control.version == version {
                if let Some(error) = &control.error {
                    return Err(error.clone());
                }
                if control.stop {
                    return Err("the playhead stopped".into());
                }
                control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    #[cfg(feature = "test-support")]
    pub fn wait_for(&self, mut ready: impl FnMut() -> bool) -> Result<(), String> {
        loop {
            let version = {
                let control = lock(&self.shared.control);
                if let Some(error) = &control.error {
                    return Err(error.clone());
                }
                if control.stop {
                    return Err("the playhead stopped".into());
                }
                control.version
            };
            if ready() {
                return Ok(());
            }
            let mut control = lock(&self.shared.control);
            if let Some(error) = &control.error {
                return Err(error.clone());
            }
            if control.stop {
                return Err("the playhead stopped".into());
            }
            while control.version == version {
                control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(error) = &control.error {
                    return Err(error.clone());
                }
                if control.stop {
                    return Err("the playhead stopped".into());
                }
            }
        }
    }

    #[cfg(feature = "test-support")]
    pub fn drive_until(
        &self,
        consumer: &mut Consumer,
        head: Hour,
        mut ready: impl FnMut() -> std::ops::ControlFlow<Result<(), String>>,
    ) -> Result<(), String> {
        let clamped = self.shared.timeline.clamp(head);
        let mut out = Vec::new();
        loop {
            self.advance(head);
            out.clear();
            let taken = consumer.take(head, &mut out);
            let version = {
                let mut control = lock(&self.shared.control);
                if control.asks.get(&None) != Some(&clamped) {
                    if let Some(error) = &control.error {
                        return Err(error.clone());
                    }
                    if control.stop {
                        return Err("playhead stopped".into());
                    }
                    drop(control);
                    std::thread::yield_now();
                    continue;
                }
                if taken > 0 {
                    control.version += 1;
                    self.shared.wake.notify_all();
                }
                control.version
            };
            if consumer.reaches(head) != Some(false) {
                std::thread::yield_now();
                continue;
            }
            if let std::ops::ControlFlow::Break(result) = ready() {
                return result;
            }
            let mut control = lock(&self.shared.control);
            loop {
                if let Some(error) = &control.error {
                    return Err(error.clone());
                }
                if control.stop {
                    return Err("playhead stopped".into());
                }
                if control.version != version {
                    break;
                }
                control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    pub fn declarations(&self) -> u64 {
        lock(&self.shared.control).selection
    }

    pub fn held(&self) -> (Report, Report, Counters) {
        *lock(&self.shared.report)
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.shared.control).error.clone()
    }

    pub fn finish(mut self) -> Vec<Lane> {
        self.stop()
    }

    fn stop(&mut self) -> Vec<Lane> {
        self.shared.nudge(|control| control.stop = true);
        self.shared.notify_control();
        lock(&self.shared.jobs).stop = true;
        self.shared.work.notify_all();
        self.shared.lent_work.notify_all();
        let lanes = self.stepper.take().map(|stepper| stepper.join().unwrap_or_default()).unwrap_or_default();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        lanes
    }
}

#[cfg(test)]
impl Playhead {
    #[track_caller]
    fn wait_until(&self, mut ready: impl FnMut() -> bool) {
        let caller = std::panic::Location::caller();
        loop {
            let version = lock(&self.shared.control).version;
            if ready() {
                return;
            }
            let mut control = lock(&self.shared.control);
            while control.version == version {
                if let Some(error) = &control.error {
                    panic!("the playhead stopped during the wait at {caller}: {error}");
                }
                control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    fn ring_len(&self) -> usize {
        let head = lock(&self.shared.control).head;
        lock(&self.shared.ring).past(head)
    }

    fn kept(&self) -> usize {
        lock(&self.shared.ring).steps_kept()
    }

    fn kept_bytes(&self) -> (usize, usize) {
        let ring = lock(&self.shared.ring);
        (ring.trailing_bytes(), ring.largest_kept())
    }
}

impl Drop for Playhead {
    fn drop(&mut self) {
        if self.stepper.is_some() {
            self.stop();
        }
    }
}

fn fetcher(shared: &Shared) {
    loop {
        let job = {
            let mut jobs = lock(&shared.jobs);
            loop {
                if jobs.stop {
                    return;
                }
                if let Some(job) = jobs.heap.pop().map(|queued| queued.job).or_else(|| jobs.joins.pop_front()) {
                    jobs.running += 1;
                    break job;
                }
                jobs = shared.work.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if let Some(done) = shared.run(job) {
            shared.nudge(|control| control.done.push(done));
        }
    }
}

fn pop_lent(jobs: &mut Jobs) -> Option<Job> {
    if jobs.heap.peek().is_some_and(|queued| queued.rank.0 <= 1) {
        return Some(jobs.heap.pop().expect("peeked").job);
    }
    jobs.joins.pop_front()
}

struct WorkerExit {
    shared: Arc<Shared>,
    exited: Arc<AtomicBool>,
}

impl Drop for WorkerExit {
    fn drop(&mut self) {
        #[cfg(test)]
        self.shared.lent_live.fetch_sub(1, Ordering::Release);
        self.exited.store(true, Ordering::Release);
        if std::thread::panicking() {
            self.shared.work.notify_all();
            self.shared.lent_work.notify_all();
            self.shared.nudge(|_| {});
        }
    }
}

fn lent_worker(shared: Arc<Shared>, exited: Arc<AtomicBool>) {
    struct LentBusy<'a>(&'a Shared);
    impl Drop for LentBusy<'_> {
        fn drop(&mut self) {
            lock(&self.0.jobs).lent_busy -= 1;
        }
    }
    let _exit = WorkerExit { shared: Arc::clone(&shared), exited };
    loop {
        let job = {
            let mut jobs = lock(&shared.jobs);
            loop {
                if jobs.stop || jobs.lent_stop {
                    return;
                }
                if let Some(job) = pop_lent(&mut jobs) {
                    jobs.running += 1;
                    jobs.lent_busy += 1;
                    break job;
                }
                jobs = shared.lent_work.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let _busy = LentBusy(&shared);
        if let Some(done) = shared.run(job) {
            shared.nudge(|control| control.done.push(done));
        }
    }
}

struct LentWorkers {
    shared: Arc<Shared>,
    handles: Vec<(JoinHandle<()>, Arc<AtomicBool>)>,
}

impl LentWorkers {
    fn new(shared: Arc<Shared>) -> Self {
        Self { shared, handles: Vec::new() }
    }

    fn reap(&mut self) -> usize {
        let mut reaped = 0;
        let mut at = 0;
        while at < self.handles.len() {
            if self.handles[at].1.load(Ordering::Acquire) || self.handles[at].0.is_finished() {
                let (handle, _) = self.handles.swap_remove(at);
                let _ = handle.join();
                reaped += 1;
            } else {
                at += 1;
            }
        }
        reaped
    }

    fn ensure(&mut self, want: usize) {
        self.reap();
        while self.handles.len() < want {
            let shared = Arc::clone(&self.shared);
            let exited = Arc::new(AtomicBool::new(false));
            #[cfg(test)]
            self.shared.lent_live.fetch_add(1, Ordering::AcqRel);
            let worker_exited = Arc::clone(&exited);
            match std::thread::Builder::new().name("playhead lent".into()).spawn(move || lent_worker(shared, worker_exited)) {
                Ok(handle) => self.handles.push((handle, exited)),
                Err(_) => {
                    #[cfg(test)]
                    self.shared.lent_live.fetch_sub(1, Ordering::Release);
                    break;
                }
            }
        }
    }
}

impl Drop for LentWorkers {
    fn drop(&mut self) {
        if self.handles.is_empty() {
            return;
        }
        lock(&self.shared.jobs).lent_stop = true;
        self.shared.lent_work.notify_all();
        for (handle, _) in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn helper(shared: &Shared, receive: &Receiver<Help>) {
    while let Ok(help) = receive.recv() {
        match help {
            Help::Batch(batch) => batch.work(),
            Help::Prepares => {
                while let Some(job) = shared.pop_urgent() {
                    if let Some(done) = shared.run(job) {
                        shared.nudge(|control| control.done.push(done));
                    }
                }
            }
        }
    }
}

struct Seek {
    hour: Hour,
    gap: bool,
    piece: usize,
    asked: Instant,
    started: Instant,
    pending: BTreeMap<usize, u64>,
    prepared: Vec<Prepared>,
}

struct Restep {
    quantum: Hour,
    pending: BTreeMap<usize, u64>,
    ready: BTreeMap<usize, (Box<Prepared>, bool)>,
}

struct NextPiece {
    piece: usize,
    pending: BTreeMap<usize, u64>,
    prepared: Vec<Prepared>,
}

struct Join {
    generation: u64,
    columns: Vec<usize>,
    keyed: bool,
    passing: bool,
    readers: Vec<Reader>,
    prepared: Option<Box<Prepared>>,
    fetching: bool,
}

struct CatchUp {
    id: usize,
    reader: Reader,
    piece: Option<usize>,
    from: Hour,
    at: Hour,
    lanes: BTreeMap<usize, (u64, Option<Slot>)>,
    steps: Vec<Arc<Step>>,
    started: Instant,
    standing: Option<Standing>,
}

struct Standing {
    tables: Vec<usize>,
    states: Vec<Rebuilt>,
    revoke: Arc<Step>,
}

impl CatchUp {
    fn lanes(self) -> (Vec<Box<Lane>>, Vec<Arc<Step>>, Option<Vec<Rebuilt>>) {
        (self.lanes.into_values().filter_map(|(_, slot)| slot.map(|slot| slot.lane)).collect(), self.steps, self.standing.map(|standing| standing.states))
    }
}

struct Given {
    reader: Reader,
    tables: Vec<usize>,
    step: Arc<Step>,
    hour: Hour,
    stood: u64,
    read: bool,
}

struct Stepper {
    shared: Arc<Shared>,
    hook: Option<Hook>,
    helpers: Vec<Sender<Help>>,
    lent: LentWorkers,
    epoch: u64,
    head: Hour,
    pace: f64,
    rate: f64,
    seek: Option<Seek>,
    slots: Vec<Slot>,
    piece: usize,
    newest: Option<Hour>,
    selection: BTreeMap<usize, Selected>,
    made: BTreeMap<usize, Selected>,
    selection_seen: u64,
    selection_applied: bool,
    hook_requested: bool,
    seed_paused: bool,
    generation: u64,
    joining: BTreeMap<usize, Join>,
    owed: BTreeMap<Reader, BTreeSet<usize>>,
    lands: Vec<usize>,
    restep: u64,
    paced: Hour,
    coarse: bool,
    restepping: Option<Restep>,
    hidden: Option<Hour>,
    next_piece: Option<NextPiece>,
    waiting_piece: bool,
    fetching: Fetching,
    counters: Counters,
    joins_published: (u64, u64, u64, u64),
    stood: u64,
    given: Vec<Given>,
    report_at: Instant,
    peak: Report,
    failed: bool,
    holding: Option<Instant>,
    holding_reads: bool,
    why: usize,
    catching: Vec<CatchUp>,
}

impl Stepper {
    fn new(shared: Arc<Shared>, hook: Option<Hook>, helpers: Vec<Sender<Help>>) -> Self {
        let settings = shared.settings;
        let lent = LentWorkers::new(Arc::clone(&shared));
        Self {
            head: shared.timeline.first(),
            shared,
            hook,
            helpers,
            lent,
            epoch: 0,
            pace: settings.speed, rate: settings.rate,
            seek: None,
            slots: Vec::new(),
            piece: 0,
            newest: None,
            selection: BTreeMap::new(),
            made: BTreeMap::new(),
            selection_seen: 0,
            selection_applied: true,
            hook_requested: false,
            seed_paused: false,
            generation: 0,
            joining: BTreeMap::new(),
            owed: BTreeMap::new(),
            lands: Vec::new(),
            restep: 0,
            paced: 0,
            coarse: false,
            restepping: None,
            hidden: None,
            next_piece: None,
            waiting_piece: false,
            fetching: Fetching::default(),
            counters: Counters::default(),
            joins_published: (0, 0, 0, 0),
            stood: 0,
            given: Vec::new(),
            report_at: Instant::now(),
            peak: Report::default(),
            failed: false,
            holding: None,
            holding_reads: false,
            why: IDLE_OTHER,
            catching: Vec::new(),
        }
    }

    fn hold(&mut self) {
        if self.newest.is_some_and(|newest| newest < self.head) && self.holding.is_none() {
            self.holding = Some(Instant::now());
        }
    }

    fn release(&mut self) {
        if let Some(since) = self.holding.take() {
            self.counters.room_held_ns += since.elapsed().as_nanos() as u64;
        }
    }

    fn quantum(&self) -> Hour {
        ((self.pace / self.rate).round() as Hour).max(1)
    }

    fn ring_size(&self) -> usize {
        lock(&self.shared.ring).size()
    }

    fn run(mut self) -> Vec<Lane> {
        let mut seen = u64::MAX;
        let mut idle = false;
        loop {
            let (seek, done, readers, lands) = {
                let mut control = lock(&self.shared.control);
                if idle {
                    let started = Instant::now();
                    while control.version == seen && !control.stop {
                        control = self.shared.wake.wait(control).unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    self.counters.idle_ns[self.why] += started.elapsed().as_nanos() as u64;
                }
                if control.stop {
                    break;
                }
                seen = control.version;
                self.head = control.head;
                self.stood = control.stood;
                self.pace = control.pace;
                self.rate = control.rate;
                self.hook_requested |= std::mem::take(&mut control.hook_requested);
                self.seed_paused |= std::mem::take(&mut control.seed_paused);
                let readers = (control.selection != self.selection_seen).then(|| {
                    self.selection_seen = control.selection;
                    control.readers.iter().map(|(&reader, declaration)| (reader, declaration.clone())).collect::<Vec<_>>()
                });
                let seek = control.seek.take().map(|(hour, epoch)| (hour, epoch, control.asked.take().unwrap_or_else(Instant::now)));
                (seek, std::mem::take(&mut control.done), readers, std::mem::take(&mut control.lands))
            };
            if self.failed {
                if seek.is_none() {
                    if let Some(readers) = readers {
                        self.selection = union(readers);
                        self.selection_applied = false;
                    }
                    done.into_iter().for_each(|done| self.discard(done));
                    idle = true;
                    continue;
                }
                self.failed = false;
                self.shared.nudge(|control| control.error = None);
            }
            if self.lent.reap() > 0 {
                let (busy, pending) = {
                    let jobs = lock(&self.shared.jobs);
                    let urgent = jobs.heap.iter().filter(|queued| queued.rank.0 <= 1).count();
                    (jobs.lent_busy, jobs.joins.len() + urgent)
                };
                if pending > 0 {
                    let cap = self.lent_cap(true);
                    self.lent.ensure((busy + pending).min(cap));
                }
            }
            let mut moved = !lands.is_empty();
            self.lands.extend(lands);
            if let Some(readers) = readers {
                self.selection = union(readers);
                self.selection_applied = false;
                self.retire();
                moved = true;
            }
            self.why = IDLE_OTHER;
            let outcome = (|| {
                if let Some((hour, epoch, asked)) = seek {
                    self.begin_seek(hour, epoch, asked);
                    moved = true;
                }
                for done in done {
                    self.receive(done)?;
                    moved = true;
                }
                if self.seek.is_some() {
                    moved |= self.help_seek()?;
                    if self.seek.as_ref().is_some_and(|seek| seek.pending.is_empty()) {
                        self.finish_seek()?;
                        moved = true;
                    }

                } else if self.newest.is_some() {
                    if self.reset_paused() {
                        moved = true;
                    } else {
                        if !self.selection_applied {
                            if !self.apply_selection() {
                                self.hook_declared();
                            }
                            moved = true;
                        } else if self.hook_requested
                            && (self.quantum() == 1 || self.newest == Some(self.shared.timeline.last()) || !lock_clock(&self.shared.clock).playing())
                        {
                            self.hook_declared();
                            moved = true;
                        }
                        if self.reset_paused() {
                            moved = true;
                        } else {
                            moved |= self.read_for_standing();
                            moved |= self.land_joins()?;
                            moved |= self.restep_ahead()?;
                            moved |= self.adopt_parked();
                            moved |= self.land_owed()?;
                            moved |= self.land_catch_ups()?;
                            self.schedule();
                            let declared = {
                                let control = lock(&self.shared.control);
                                control.selection != self.selection_seen || control.seed_paused
                            };
                            if declared {
                                moved = true;
                            } else {
                                moved |= self.build()?;
                            }
                            self.schedule();
                            self.prepare_next_piece();
                        }
                    }
                }
                Ok::<(), io::Error>(())
            })();
            if let Err(error) = outcome
                && self.current()
            {
                self.failed = true;
                self.shared.nudge(|control| {
                    if self.current() {
                        control.error = Some(error.to_string());
                    }
                });
                self.shared.notify_control();
            }
            self.publish_joins();
            if !moved && !lock_clock(&self.shared.clock).playing() {
                let retired = lock(&self.shared.ring).settle();
                if !retired.is_empty() {
                    self.shared.free(retired);
                }
            }
            self.report(false);
            idle = !moved;
        }
        self.slots.into_iter().map(|slot| *slot.lane).collect()
    }


    fn discard(&self, done: Done) {
        match done {
            Done::Prepared { result: Ok(prepared), .. } => self.shared.free(vec![prepared]),
            Done::Fetched { result: Ok(fetched), .. } => self.shared.free(fetched),
            _ => {}
        }
    }

    fn abandon_prepared(&mut self) -> Vec<Box<Prepared>> {
        let mut abandoned: Vec<Box<Prepared>> = std::mem::take(&mut self.joining).into_values().filter_map(|join| join.prepared).collect();
        self.counters.joins_open = 0;
        if let Some(next) = self.next_piece.take() {
            abandoned.extend(next.prepared.into_iter().map(Box::new));
        }
        if let Some(seek) = self.seek.take() {
            abandoned.extend(seek.prepared.into_iter().map(Box::new));
        }
        if let Some(restep) = self.restepping.take() {
            abandoned.extend(restep.ready.into_values().map(|(prepared, _)| prepared));
        }
        abandoned
    }

    fn begin_seek(&mut self, hour: Hour, epoch: u64, asked: Instant) {
        self.epoch = epoch;
        self.seed_paused = false;
        let catching: Vec<_> = std::mem::take(&mut self.catching).into_iter().map(CatchUp::lanes).collect();
        if !catching.is_empty() {
            self.shared.free(catching);
        }
        self.given.clear();
        self.counters.declarations_applied = self.selection_seen;
        self.counters.seeks += 1;
        self.release();
        let lanes: Vec<Box<Lane>> = std::mem::take(&mut self.slots).into_iter().map(|slot| slot.lane).collect();
        let abandoned = self.abandon_prepared();
        if !lanes.is_empty() || !abandoned.is_empty() {
            self.shared.free_after_prepares((lanes, abandoned));
        }
        self.newest = None;
        (self.hidden, self.coarse) = (None, false);
        self.selection_applied = true;
        self.made = self.selection.clone();
        self.owed.clear();
        self.lands.clear();
        let timeline = &self.shared.timeline;
        let (piece, lanes_at, gap) = match timeline.region(hour) {
            Region::Piece(piece) => (piece, hour, false),
            Region::Gap { next } => (next - 1, timeline.pieces[next - 1].last, true),
        };
        self.piece = piece;
        let run = timeline.pieces[piece].run;
        let through = lanes_at + self.shared.settings.ahead as Hour * self.quantum();
        let mut pending = BTreeMap::new();
        let mut jobs = Vec::new();
        for (&table, selected) in &self.selection {
            self.generation += 1;
            pending.insert(table, self.generation);
            let cost = seek_cost(&*self.shared.pages, run, table, lanes_at);
            let prepare = Prepare {
                epoch,
                purpose: Purpose::Seek,
                table,
                generation: self.generation,
                run,
                columns: selected.columns.clone(),
                keyed: selected.keyed,
                passing: selected.passing,
                hour: lanes_at,
                through,
                frames: None,
                rebuilds: selected.rebuilds(None),
            };
            jobs.push(((1, -cost), Job::Prepare(prepare)));
        }
        let (running, lent_busy, urgent) = self.shared.queue(jobs);
        for helper in &self.helpers {
            let _ = helper.send(Help::Prepares);
        }
        self.lend_seek_threads(running, lent_busy, urgent);
        self.seek = Some(Seek { hour, gap, piece, asked, started: Instant::now(), pending, prepared: Vec::new() });
    }

    fn lent_cap(&self, at_least_one: bool) -> usize {
        let own = 1 + self.helpers.len() + self.shared.settings.fetch_threads;
        let width = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let extra = width.saturating_sub(own);
        if at_least_one { extra.max(1) } else { extra }
    }

    fn lend_seek_threads(&mut self, running: usize, lent_busy: usize, urgent: usize) {
        let own = 1 + self.helpers.len() + self.shared.settings.fetch_threads;
        let own_idle = own.saturating_sub(running.saturating_sub(lent_busy));
        let want = (lent_busy + urgent.saturating_sub(own_idle)).min(self.lent_cap(false));
        self.lent.ensure(want);
    }

    fn help_seek(&mut self) -> io::Result<bool> {
        let mut moved = false;
        let entering = self.shared.settings.entering;
        while self.seek.as_ref().is_some_and(|seek| !seek.pending.is_empty()) {
            if lock(&self.shared.control).seek.is_some() {
                break;
            }
            let Some(job) = self.shared.pop_urgent() else { break };
            if !moved && let Some(entering) = entering {
                entering(Thread::Seeking);
            }
            moved = true;
            let done = self.shared.run(job);
            if let Some(done) = done {
                self.receive(done)?;
            }
        }
        if moved && let Some(entering) = entering {
            entering(Thread::Stepper);
        }
        Ok(moved)
    }

    fn finish_seek(&mut self) -> io::Result<()> {
        let seek = self.seek.take().expect("a seek under way");
        let mut sought = Seeking { tables: seek.prepared.len() as u32, ..Seeking::default() };
        let mut prepared = seek.prepared;
        prepared.sort_by_key(|held| held.lane.table());
        let mut states = Vec::new();
        let mut threads = Vec::new();
        for held in prepared {
            let Prepared { generation, lane, states: rebuilt, sought: lane_sought, rebuild_ns, ns, ahead_ns, thread } = held;
            if !threads.contains(&thread) {
                threads.push(thread);
            }
            sought.ahead_ns += ahead_ns;
            sought.restore_ns += lane_sought.restore_ns;
            sought.restore_most_ns = sought.restore_most_ns.max(lane_sought.restore_ns);
            sought.replay_ns += lane_sought.replay_ns;
            sought.replay_most_ns = sought.replay_most_ns.max(lane_sought.replay_ns);
            sought.fetch_ns += lane_sought.fetch_ns;
            sought.rebuild_ns += rebuild_ns;
            sought.rebuild_most_ns = sought.rebuild_most_ns.max(rebuild_ns);
            sought.table_most_ns = sought.table_most_ns.max(ns);
            sought.states += rebuilt.len() as u32;
            sought.state_bytes += rebuilt.iter().map(|(_, state)| state.bytes() as u64).sum::<u64>();
            let table = lane.table();
            states.extend(rebuilt.into_iter().map(|(reader, state)| Rebuilt { reader, table, state }));
            self.slots.push(Slot::new(generation, lane));
        }
        sought.ns = seek.started.elapsed().as_nanos() as u64;
        sought.waited_ns = seek.started.saturating_duration_since(seek.asked).as_nanos() as u64;
        sought.threads = threads.len() as u32;
        self.piece = seek.piece;
        let piece = (!seek.gap).then_some(seek.piece);
        let built = Built { sought: Some(sought), ..Built::default() };
        self.publish_reset(seek.hour, piece, states, built)?;
        self.report(true);
        Ok(())
    }

    fn publish_reset(&mut self, hour: Hour, piece: Option<usize>, states: Vec<Rebuilt>, mut built: Built) -> io::Result<()> {
        let mut step = self.fresh_step();
        let held = Arc::get_mut(&mut step).expect("a fresh step is held once");
        let mut buffers = std::mem::take(&mut held.tables).into_iter();
        let mut tables = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            let mut changes = buffers.next().unwrap_or_default();
            slot.lane.reset_into(&mut changes)?;
            tables.push(changes);
        }
        self.retire_changes(buffers.collect());
        held.tables = tables;
        *lock(&held.states) = states;
        built.fetched = std::mem::take(&mut self.fetching);
        let keep = self.ends_kept();
        let ended = hour.min(self.shared.timeline.pieces[self.piece].last);
        self.slots.iter_mut().for_each(|slot| slot.ended(ended, keep));
        (held.first, held.last, held.piece, held.reset, held.built) = (hour, hour, piece, true, built);
        self.publish(step)?;
        self.slots.iter_mut().for_each(|slot| slot.lane.clear_texts());
        Ok(())
    }

    fn receive(&mut self, done: Done) -> io::Result<()> {
        match done {
            Done::Fetched { epoch, table, generation, result } => {
                let slot = self.slots.iter().position(|slot| slot.table == table && slot.generation == generation);
                let wanted = if self.obsolete(epoch) {
                    None
                } else if let Some(at) = slot {
                    Some(Fed::Slot(at))
                } else if self.joining.get(&table).is_some_and(|join| join.generation == generation) {
                    Some(Fed::Join)
                } else if self.restepping.as_ref().and_then(|restep| restep.ready.get(&table)).is_some_and(|(prepared, _)| prepared.generation == generation) {
                    Some(Fed::Restep)
                } else {
                    self.catching.iter().position(|catch| catch.lanes.get(&table).is_some_and(|(held, slot)| *held == generation && slot.is_some())).map(Fed::CatchUp)
                };
                let Some(wanted) = wanted else {
                    if let Ok(fetched) = result {
                        self.shared.free(fetched);
                    }
                    return Ok(());
                };
                let fetched = match result {
                    Ok(fetched) => fetched,
                    Err(_) if !self.declared_now(table) => return Ok(()),
                    Err(error) => return Err(error),
                };
                let read = Fetching { jobs: 1, parts: fetched.parts() as u64, tracks: fetched.tracks as u64, stored: fetched.stored as u64, decompressed: fetched.decompressed as u64, ns: fetched.ns };
                self.fetching.add(&read);
                match wanted {
                    Fed::Slot(at) => {
                        let slot = &mut self.slots[at];
                        slot.fetching = false;
                        slot.lane.give(fetched)?;
                    }
                    Fed::Join => {
                        let join = self.joining.get_mut(&table).expect("a join waits");
                        join.fetching = false;
                        join.prepared.as_mut().expect("a join reads once prepared").lane.give(fetched)?;
                    }
                    Fed::Restep => {
                        let (prepared, fetching) = self.restepping.as_mut().and_then(|restep| restep.ready.get_mut(&table)).expect("a restep waits");
                        *fetching = false;
                        prepared.lane.give(fetched)?;
                    }
                    Fed::CatchUp(at) => {
                        let slot = self.catching[at].lanes.get_mut(&table).and_then(|(_, slot)| slot.as_mut()).expect("a catch-up waits");
                        slot.fetching = false;
                        slot.lane.give(fetched)?;
                    }
                }
            }
            Done::Prepared { epoch, purpose, table, generation, result } => {
                let wanted = !self.obsolete(epoch)
                    && match purpose {
                        Purpose::Seek => self.seek.as_ref().is_some_and(|seek| seek.pending.get(&table) == Some(&generation)),
                        Purpose::Piece(piece) => self.next_piece.as_ref().is_some_and(|next| next.piece == piece && next.pending.get(&table) == Some(&generation)),
                        Purpose::Restep => self.restepping.as_ref().is_some_and(|restep| restep.pending.get(&table) == Some(&generation)),
                        Purpose::CatchUp => self.catching.iter().any(|catch| catch.lanes.get(&table).is_some_and(|(held, slot)| *held == generation && slot.is_none())),
                        Purpose::Join => self.joining.get(&table).is_some_and(|join| join.generation == generation),
                    };
                if !wanted {
                    if matches!(purpose, Purpose::Join) && !self.obsolete(epoch) {
                        self.counters.join_answers_dropped += 1;
                    }
                    if let Ok(prepared) = result {
                        self.shared.free(vec![prepared]);
                    }
                    return Ok(());
                }
                let prepared = match result {
                    Ok(prepared) => prepared,
                    Err(_) if !self.declared_now(table) => return Ok(()),
                    Err(error) => return Err(error),
                };
                match purpose {
                    Purpose::Seek => {
                        let seek = self.seek.as_mut().expect("a seek waits");
                        seek.pending.remove(&table);
                        seek.prepared.push(*prepared);
                    }
                    Purpose::Piece(_) => {
                        let next = self.next_piece.as_mut().expect("a piece waits");
                        next.pending.remove(&table);
                        next.prepared.push(*prepared);
                    }
                    Purpose::Restep => {
                        let restep = self.restepping.as_mut().expect("a restep waits");
                        restep.pending.remove(&table);
                        restep.ready.insert(table, (prepared, false));
                    }
                    Purpose::CatchUp => {
                        let catch = self.catching.iter_mut().find(|catch| catch.lanes.get(&table).is_some_and(|(held, slot)| *held == generation && slot.is_none())).expect("a catch-up waits");
                        let Prepared { lane, states, .. } = *prepared;
                        if let Some(standing) = &mut catch.standing {
                            standing.states.extend(states.into_iter().map(|(reader, state)| Rebuilt { reader, table, state }));
                        }
                        catch.lanes.insert(table, (generation, Some(Slot::new(generation, lane))));
                    }
                    Purpose::Join => self.joining.get_mut(&table).expect("a join waits").prepared = Some(prepared),
                }
            }
        }
        Ok(())
    }

    fn obsolete(&self, epoch: u64) -> bool {
        epoch != self.epoch || epoch != self.shared.epoch.load(Ordering::Acquire)
    }

    fn apply_selection(&mut self) -> bool {
        self.selection_applied = true;
        if self.newest.is_none() {
            return false;
        }
        let selection = self.selection.clone();
        let mut abandoned: Vec<Box<Prepared>> = Vec::new();
        let reads = |reader: &Reader, table: &usize| selection.get(table).is_some_and(|selected| selected.readings.iter().any(|reading| reading.reader == *reader));
        self.owed.retain(|reader, tables| {
            tables.retain(|table| reads(reader, table));
            !tables.is_empty()
        });
        let mut jobs = Vec::new();
        for (&table, selected) in &selection {
            let made = self.made.get(&table);
            let fresh = selected.readings.iter().filter(|reading| made.is_none_or(|made| !made.readings.iter().any(|held| held.same(reading))));
            let union = (selected.columns.as_slice(), selected.keyed, selected.passing);
            let held = match (self.joining.get(&table), self.slot_of(table)) {
                (Some(join), _) => Some((join.columns.as_slice(), join.keyed, join.passing)),
                (None, Some(at)) => Some(self.slots[at].lane.declared()),
                (None, None) => None,
            };
            if self.made.get(&table).map(|made| (made.columns.as_slice(), made.keyed, made.passing)) != Some(union)
                && let Some(next) = &mut self.next_piece
            {
                next.pending.remove(&table);
                if let Some(at) = next.prepared.iter().position(|prepared| prepared.lane.table() == table) {
                    abandoned.push(Box::new(next.prepared.swap_remove(at)));
                }
            }
            if held == Some(union) {
                for reading in fresh.filter(|reading| reading.rebuild.is_some()) {
                    self.owed.entry(reading.reader).or_default().insert(table);
                }
                continue;
            }
            let mut readers: Vec<Reader> = if held.is_none() { selected.readings.iter().map(|reading| reading.reader).collect() } else { fresh.map(|reading| reading.reader).collect() };
            if let Some(join) = self.joining.remove(&table) {
                abandoned.extend(join.prepared);
                readers.extend(join.readers);
            }
            for (reader, tables) in &mut self.owed {
                if tables.remove(&table) {
                    readers.push(*reader);
                }
            }
            readers.sort_unstable();
            readers.dedup();
            readers.retain(|reader| selected.readings.iter().any(|reading| reading.reader == *reader));
            jobs.push(self.join(table, selected, readers));
        }
        self.owed.retain(|_, tables| !tables.is_empty());
        self.made = selection;
        self.counters.joins_open = self.joining.len() as u64;
        self.counters.declarations_applied = self.selection_seen;
        let count = jobs.len();
        let joins = count > 0;
        self.shared.queue_joins(jobs);
        self.lend_join_threads(count);
        if !abandoned.is_empty() {
            self.shared.free(abandoned);
        }
        if !self.catching.is_empty() {
            self.restart_catch_ups();
        }
        joins
    }

    fn retire(&mut self) {
        let selection = &self.selection;
        let kept = |table: &usize| selection.contains_key(table);
        let (slots, gone): (Vec<Slot>, Vec<Slot>) = std::mem::take(&mut self.slots).into_iter().partition(|slot| kept(&slot.table));
        self.slots = slots;
        let lanes: Vec<Box<Lane>> = gone.into_iter().map(|slot| slot.lane).collect();
        let mut abandoned: Vec<Box<Prepared>> = Vec::new();
        let joins: Vec<usize> = self.joining.keys().copied().filter(|table| !kept(table)).collect();
        for table in joins {
            abandoned.extend(self.joining.remove(&table).and_then(|join| join.prepared));
        }
        self.counters.joins_open = self.joining.len() as u64;
        let mut unprepared = |pending: &mut BTreeMap<usize, u64>, prepared: &mut Vec<Prepared>| {
            pending.retain(|table, _| kept(table));
            let (held, gone): (Vec<Prepared>, Vec<Prepared>) = std::mem::take(prepared).into_iter().partition(|held| kept(&held.lane.table()));
            *prepared = held;
            abandoned.extend(gone.into_iter().map(Box::new));
        };
        if let Some(seek) = &mut self.seek {
            unprepared(&mut seek.pending, &mut seek.prepared);
        }
        if let Some(next) = &mut self.next_piece {
            unprepared(&mut next.pending, &mut next.prepared);
        }
        if let Some(restep) = &mut self.restepping {
            restep.pending.retain(|table, _| kept(table));
            let gone: Vec<usize> = restep.ready.keys().copied().filter(|table| !kept(table)).collect();
            abandoned.extend(gone.into_iter().filter_map(|table| restep.ready.remove(&table)).map(|(prepared, _)| prepared));
        }
        if !lanes.is_empty() {
            self.shared.free(lanes);
        }
        if !abandoned.is_empty() {
            self.shared.free(abandoned);
        }
    }

    fn declared_now(&self, table: usize) -> bool {
        lock(&self.shared.control).readers.values().any(|declaration| declaration.tables.iter().any(|declared| declared.table == table))
    }

    fn reset_paused(&mut self) -> bool {
        let Some(newest) = self.newest else { return false };
        let sought = {
            let mut clock = lock_clock(&self.shared.clock);
            if clock.playing() {
                self.seed_paused = false;
                return false;
            }
            let head = clock.head(Instant::now());
            let need_seed = self.seed_paused && newest > head;
            let piece = self.shared.timeline.pieces[self.piece];
            let unlandable_join = !self.joining.is_empty()
                && (head < piece.first
                    || (newest > piece.last
                        && self.joining.keys().any(|&table| self.slot_of(table).is_some_and(|old| !self.slots[old].whole_at(piece.last)))));
            if need_seed || unlandable_join {
                if self.current() {
                    self.shared.seek_locked(&mut clock, head);
                    true
                } else {
                    false
                }
            } else {
                if self.selection_applied && !self.hook_requested && self.joining.is_empty() {
                    self.seed_paused = false;
                }
                false
            }
        };
        if sought {
            self.seed_paused = false;
            self.shared.notify_control();
        }
        sought
    }

    fn hold_for_paused_reads(&mut self, newest: Hour) -> bool {
        let holding = self.shared.listing.load(Ordering::Acquire) > 0 && newest >= self.head && !lock_clock(&self.shared.clock).playing();
        if holding != self.holding_reads {
            self.holding_reads = holding;
            self.shared.nudge(|control| control.holding_reads = holding);
        }
        holding
    }

    fn hold_for_paused_seed(&mut self) -> bool {
        if !self.seed_paused {
            return false;
        }
        let clock = lock_clock(&self.shared.clock);
        if clock.playing() {
            self.seed_paused = false;
            return false;
        }
        let head = clock.head(Instant::now());
        if self.joining.is_empty() && self.selection_applied && !self.hook_requested && self.newest.is_some_and(|newest| newest <= head) {
            self.seed_paused = false;
            return false;
        }
        !self.joining.is_empty() && self.newest.is_some_and(|newest| newest >= head)
    }

    fn piece_at(&self, hour: Hour) -> Option<usize> {
        match self.shared.timeline.region(hour) {
            Region::Piece(piece) => Some(piece),
            Region::Gap { .. } => None,
        }
    }

    fn hook_declared(&mut self) {
        self.hook_requested = false;
        let Some(newest) = self.newest else { return };
        let piece = self.piece_at(newest);
        let Some(hook) = &mut self.hook else { return };
        let step = Step { epoch: self.epoch, first: newest + 1, last: newest, piece, restep: self.restep, ..Step::default() };
        hook(&step, &Lanes { slots: &self.slots });
    }

    fn slot_of(&self, table: usize) -> Option<usize> {
        self.slots.binary_search_by_key(&table, |slot| slot.table).ok()
    }

    fn join(&mut self, table: usize, selected: &Selected, readers: Vec<Reader>) -> Job {
        self.generation += 1;
        self.counters.joins += 1;
        let piece = self.shared.timeline.pieces[self.piece];
        let span = (self.ring_size() as Hour + self.shared.settings.ahead as Hour) * self.quantum();
        let prepare = Prepare {
            epoch: self.epoch,
            purpose: Purpose::Join,
            table,
            generation: self.generation,
            run: piece.run,
            columns: selected.columns.clone(),
            keyed: selected.keyed,
            passing: selected.passing,
            hour: piece.first,
            through: piece.first + span,
            frames: Some((piece.last, span)),
            rebuilds: selected.rebuilds(Some(&readers)),
        };
        self.joining.insert(table, Join { generation: self.generation, columns: selected.columns.clone(), keyed: selected.keyed, passing: selected.passing, readers, prepared: None, fetching: false });
        self.counters.joins_open = self.joining.len() as u64;
        Job::Prepare(prepare)
    }

    fn lend_join_threads(&mut self, joins: usize) {
        if joins == 0 {
            return;
        }
        let (busy, queued) = {
            let jobs = lock(&self.shared.jobs);
            (jobs.lent_busy, jobs.joins.len())
        };
        let want = (busy + queued).min(self.lent_cap(true));
        self.lent.ensure(want);
    }

    fn publish_joins(&mut self) {
        let now = (self.joining.len() as u64, self.counters.ripe_checks, self.counters.declarations_applied, self.counters.join_answers_dropped);
        if now != self.joins_published {
            self.joins_published = now;
            self.counters.joins_open = now.0;
            lock(&self.shared.report).2 = self.counters;
            let caught_up = self.caught_up_at();
            self.shared.nudge(|control| self.publish_caught_up(control, caught_up));
        }
    }

    fn current(&self) -> bool {
        self.shared.epoch.load(Ordering::Acquire) == self.epoch
    }

    fn publish_newest(&self, control: &mut Control, newest: Hour) {
        if self.current() {
            control.newest = Some(newest);
        }
    }

    fn publish_caught_up(&self, control: &mut Control, caught_up: Option<Hour>) {
        let current = control.selection == self.selection_seen
            && control.seek.is_none()
            && control.lands.is_empty()
            && self.current();
        control.caught_up_at = if current { caught_up } else { None };
    }

    fn caught_up_at(&self) -> Option<Hour> {
        let newest = self.newest?;
        if !self.selection_applied
            || !self.joining.is_empty()
            || !self.catching.is_empty()
            || !self.owed.is_empty()
            || !self.lands.is_empty()
        {
            return None;
        }
        self.slots
            .iter()
            .filter_map(|slot| slot.lane.head())
            .min()
            .map_or(Some(newest), |head| Some(head.min(newest)))
    }

    fn ends_kept(&self) -> usize {
        2 * self.ring_size() + 4
    }

    fn land_joins(&mut self) -> io::Result<bool> {
        let Some(newest) = self.newest else { return Ok(false) };
        if self.shared.epoch.load(Ordering::Acquire) != self.epoch || self.joining.is_empty() {
            return Ok(false);
        }
        let Some((_, hour, spans)) = lock(&self.shared.ring).plan(self.epoch) else { return Ok(false) };
        let piece = self.piece;
        let at = hour.min(self.shared.timeline.pieces[piece].last);
        let past: Vec<usize> = self.joining.iter().filter(|(_, join)| join.prepared.as_ref().is_some_and(|prepared| prepared.lane.head().is_some_and(|head| head > at))).map(|(&table, _)| table).collect();
        if !past.is_empty() {
            let mut jobs = Vec::with_capacity(past.len());
            for table in past {
                let join = self.joining.remove(&table).expect("joining");
                self.shared.free(join.prepared);
                if let Some(selected) = self.selection.get(&table).cloned() {
                    jobs.push(self.join(table, &selected, join.readers));
                }
            }
            let count = jobs.len();
            self.shared.queue_joins(jobs);
            self.lend_join_threads(count);
        }
        let waits = |table: usize, join: &Join| -> Option<usize> {
            let Some(prepared) = join.prepared.as_ref() else { return Some(0) };
            if join.fetching {
                return Some(1);
            }
            if !prepared.lane.holds_through(newest) {
                return Some(2);
            }
            if prepared.lane.head().is_none_or(|head| head > at) {
                return Some(3);
            }
            let old = self.slot_of(table)?;
            if !self.slots[old].whole_at(at) {
                return Some(4);
            }
            (!spans.iter().filter(|span| !span.gap).all(|span| self.slots[old].whole_at(span.last))).then_some(5)
        };
        let mut tally = [0u64; 6];
        let ripe = self.joining.values().all(|join| join.prepared.is_some() && !join.fetching);
        let able = |table: usize, join: &Join| waits(table, join).is_none();
        for (&table, join) in &self.joining {
            if let Some(why) = waits(table, join) {
                tally[why] += 1;
            }
        }
        let mut held: BTreeSet<usize> = self.joining.iter().filter(|&(&table, join)| !able(table, join)).map(|(&table, _)| table).collect();
        loop {
            let waiting: BTreeSet<Reader> = held.iter().flat_map(|table| self.joining[table].readers.iter().copied()).collect();
            let more: Vec<usize> = self.joining.iter().filter(|(table, join)| !held.contains(*table) && join.readers.iter().any(|reader| waiting.contains(reader))).map(|(&table, _)| table).collect();
            if more.is_empty() {
                break;
            }
            held.extend(more);
        }
        let ready: Vec<usize> = self.joining.keys().copied().filter(|table| !held.contains(table)).collect();
        for (counted, waited) in self.counters.join_waits.iter_mut().zip(tally) {
            *counted += waited;
        }
        self.counters.ripe_checks += u64::from(ripe);
        if ready.is_empty() {
            lock(&self.shared.report).2 = self.counters;
            return Ok(false);
        }
        let keep = self.ends_kept();
        let mut landed = Vec::with_capacity(ready.len());
        let mut resets = Vec::with_capacity(ready.len());
        let mut fresh = Vec::with_capacity(ready.len());
        for &table in &ready {
            let join = self.joining.remove(&table).expect("ready");
            let Prepared { lane, states, .. } = *join.prepared.expect("ready");
            let mut reset = Changes::default();
            lane.reset_into(&mut reset)?;
            resets.push(reset);
            let mut slot = Slot::new(join.generation, lane);
            if let Some(head) = slot.lane.head() {
                slot.ended(head, keep);
            }
            match self.slot_of(table) {
                Some(old) => {
                    let old = std::mem::replace(&mut self.slots[old], slot);
                    self.shared.free(vec![old.lane]);
                    fresh.push(false);
                }
                None => {
                    let place = self.slots.partition_point(|held| held.table < table);
                    self.slots.insert(place, slot);
                    fresh.push(true);
                }
            }
            landed.push((table, join.readers, states));
        }
        let places: Vec<usize> = ready.iter().map(|&table| self.slot_of(table).expect("landed")).collect();
        let mut view = Step { epoch: self.epoch, first: hour + 1, last: hour, piece: self.piece_at(hour), tables: resets, ..Step::default() };
        hook_fresh(&mut self.hook, &self.slots, &mut view, &fresh);
        places.iter().for_each(|&place| self.slots[place].lane.clear_texts());
        let mut rounds: Vec<(Option<u64>, Vec<Option<Changes>>)> = vec![(None, std::mem::take(&mut view.tables).into_iter().map(Some).collect())];
        loop {
            let behind: Vec<bool> = places.iter().map(|&place| self.slots[place].lane.head().is_some_and(|head| head < at)).collect();
            if !behind.contains(&true) {
                break;
            }
            let mut round = Vec::with_capacity(places.len());
            let mut moved = false;
            for (&place, &behind) in places.iter().zip(&behind) {
                if !behind {
                    round.push(None);
                    continue;
                }
                let before = self.slots[place].lane.head();
                let mut changes = Changes::default();
                step_slot(&mut self.slots[place], at, &mut changes, None)?;
                moved |= self.slots[place].lane.head() != before;
                round.push(Some(changes));
            }
            self.view_round(hour + 1, hour, &places, &fresh, &mut round);
            rounds.push((None, round));
            if !moved {
                break;
            }
        }
        for &place in &places {
            self.slots[place].ended(at, keep);
        }
        let mut end = hour;
        for span in &spans {
            let mut round = Vec::with_capacity(places.len());
            for &place in &places {
                if span.gap {
                    round.push(None);
                    continue;
                }
                let mut changes = Changes::default();
                step_slot(&mut self.slots[place], span.last, &mut changes, None)?;
                self.counters.stalls += u64::from(changes.stalled());
                self.slots[place].ended(span.last, keep);
                round.push(Some(changes));
            }
            self.view_round(span.first, span.last, &places, &fresh, &mut round);
            rounds.push((Some(span.seq), round));
            end = span.last;
        }
        if fresh.contains(&false) {
            let view_piece = self.piece_at(end);
            if let Some(hook) = &mut self.hook {
                let mut view = Step { epoch: self.epoch, first: end + 1, last: end, piece: view_piece, ..Step::default() };
                for (&place, _) in places.iter().zip(&fresh).filter(|(_, fresh)| !**fresh) {
                    let mut changes = Changes::default();
                    self.slots[place].lane.reset_into(&mut changes)?;
                    view.tables.push(changes);
                }
                hook(&view, &Lanes { slots: &self.slots });
            }
        }
        let mut cursors = Vec::new();
        places.iter().for_each(|&place| self.slots[place].lane.retire_into(&mut cursors));
        if !cursors.is_empty() {
            self.shared.free(cursors);
        }
        let mut ring = lock(&self.shared.ring);
        let Some((position, now)) = ring.frames().filter(|_| ring.epoch == self.epoch) else {
            drop(ring);
            self.shared.free((rounds, landed));
            self.counters.joins_open = self.joining.len() as u64;
            return Ok(true);
        };
        let mut shared = Vec::new();
        let mut own: Vec<Vec<Option<Changes>>> = Vec::new();
        for (seq, round) in rounds {
            match seq {
                Some(seq) if seq >= position => shared.push((seq, round.into_iter().flatten().collect())),
                _ => own.push(round),
            }
        }
        let mut readers: BTreeMap<Reader, Vec<usize>> = BTreeMap::new();
        for (index, (_, held, ..)) in landed.iter().enumerate() {
            for &reader in held {
                readers.entry(reader).or_default().push(index);
            }
        }
        let lasts: Vec<Option<Reader>> = landed.iter().map(|(_, held, ..)| held.iter().max().copied()).collect();
        let mut states: BTreeMap<Reader, Vec<Rebuilt>> = BTreeMap::new();
        for (table, _, rebuilt) in landed {
            for (reader, state) in rebuilt {
                states.entry(reader).or_default().push(Rebuilt { reader, table, state });
            }
        }
        for (reader, indexes) in readers {
            let mut steps = Vec::with_capacity(own.len());
            for (round, sets) in own.iter_mut().enumerate() {
                let mut tables = Vec::with_capacity(indexes.len());
                for &index in &indexes {
                    let held = &mut sets[index];
                    tables.extend(if lasts[index] == Some(reader) { held.take() } else { held.clone() });
                }
                if tables.is_empty() && round > 0 {
                    continue;
                }
                let step = Step { epoch: self.epoch, seq: position, first: now + 1, last: now, piece: self.piece_at(now), tables, ..Step::default() };
                if round == 0 {
                    *lock(&step.states) = states.remove(&reader).unwrap_or_default();
                }
                steps.push(Arc::new(step));
            }
            ring.give(reader, position, &steps);
        }
        let replaced = ring.complete(shared);
        drop(ring);
        self.retire_changes(replaced);
        self.counters.joins_at_head += ready.len() as u64;
        self.counters.joins_open = self.joining.len() as u64;
        lock(&self.shared.report).2 = self.counters;
        let caught_up = self.caught_up_at();
        self.shared.nudge(|control| {
            control.published_steps += 1;
            self.publish_caught_up(control, caught_up);
        });
        self.shared.notify_published();
        Ok(true)
    }

    fn read_for_standing(&mut self) -> bool {
        if self.given.is_empty() {
            return false;
        }
        let stood = self.stood;
        let ring = lock(&self.shared.ring);
        self.given.retain(|given| ring.holds_own(&given.step));
        let mut reads = Vec::new();
        for (at, given) in self.given.iter().enumerate() {
            if given.read || stood <= given.stood || given.tables.iter().any(|table| self.joining.contains_key(table) || self.slot_of(*table).is_none() || !self.selection.contains_key(table)) {
                continue;
            }
            for id in ring.holding(&given.step) {
                if self.catching.iter().any(|catch| catch.id == id) {
                    continue;
                }
                if let Some(hour) = ring.stands_at(id).filter(|&hour| hour < given.hour) {
                    reads.push((at, id, given.reader, hour, given.tables.clone(), Arc::clone(&given.step)));
                }
            }
        }
        drop(ring);
        let mut jobs = Vec::new();
        for (at, id, reader, hour, tables, revoke) in reads {
            let Region::Piece(piece) = self.shared.timeline.region(hour) else { continue };
            self.given[at].read = true;
            let mut catch = CatchUp { id, reader, piece: Some(piece), from: hour, at: hour, lanes: BTreeMap::new(), steps: Vec::new(), started: Instant::now(), standing: Some(Standing { tables, states: Vec::new(), revoke }) };
            jobs.extend(self.catch_reads(&mut catch));
            self.catching.push(catch);
            self.counters.standing_reads += 1;
        }
        if jobs.is_empty() {
            return false;
        }
        let count = jobs.len();
        self.shared.queue_joins(jobs);
        self.lend_join_threads(count);
        lock(&self.shared.report).2 = self.counters;
        self.shared.nudge(|control| control.caught_up_at = None);
        true
    }

    fn join_owed(&mut self) -> bool {
        let mut wanted: BTreeMap<usize, Vec<Reader>> = BTreeMap::new();
        for (reader, tables) in std::mem::take(&mut self.owed) {
            for table in tables {
                if self.joining.contains_key(&table) {
                    self.owed.entry(reader).or_default().insert(table);
                } else if self.slot_of(table).is_some() {
                    wanted.entry(table).or_default().push(reader);
                }
            }
        }
        let asked: Vec<(usize, Option<Reader>)> = {
            let ring = lock(&self.shared.ring);
            std::mem::take(&mut self.lands).into_iter().map(|id| (id, ring.reader_of(id))).collect()
        };
        let had_lands = !asked.is_empty();
        for (id, reader) in asked {
            let Some(reader) = reader else { continue };
            let tables: Vec<usize> = self.selection.iter().filter(|(_, selected)| selected.readings.iter().any(|reading| reading.reader == reader)).map(|(&table, _)| table).collect();
            if tables.iter().any(|table| self.joining.contains_key(table)) {
                self.lands.push(id);
                continue;
            }
            for table in tables.into_iter().filter(|&table| self.slot_of(table).is_some()) {
                wanted.entry(table).or_default().push(reader);
            }
        }
        let mut jobs = Vec::with_capacity(wanted.len());
        for (table, mut readers) in wanted {
            let Some(selected) = self.selection.get(&table).cloned() else { continue };
            readers.sort_unstable();
            readers.dedup();
            jobs.push(self.join(table, &selected, readers));
        }
        let count = jobs.len();
        if count == 0 && had_lands && self.lands.is_empty() {
            let caught_up = self.caught_up_at();
            self.shared.nudge(|control| self.publish_caught_up(control, caught_up));
        }
        self.shared.queue_joins(jobs);
        self.lend_join_threads(count);
        count > 0
    }

    fn restep_ahead(&mut self) -> io::Result<bool> {
        if self.newest.is_none() || self.shared.epoch.load(Ordering::Acquire) != self.epoch {
            return Ok(false);
        }
        let quantum = self.quantum();
        if let Some(restep) = self.restepping.take_if(|restep| restep.quantum != quantum) {
            self.shared.free(restep.ready.into_values().map(|(prepared, _)| prepared).collect::<Vec<_>>());
        }
        self.coarse |= quantum < self.paced;
        let Some(restep) = &self.restepping else {
            if !self.coarse {
                return Ok(false);
            }
            let Some((_, _, spans)) = lock(&self.shared.ring).plan(self.epoch) else { return Ok(false) };
            if !spans.iter().any(|span| !span.gap && span.last - span.first + 1 > quantum) {
                self.coarse = false;
                return Ok(false);
            }
            let piece = self.shared.timeline.pieces[self.piece];
            let span = (self.ring_size() as Hour + self.shared.settings.ahead as Hour) * quantum;
            let (mut pending, mut jobs) = (BTreeMap::new(), Vec::with_capacity(self.slots.len()));
            for slot in &self.slots {
                self.generation += 1;
                pending.insert(slot.table, self.generation);
                let lane = &slot.lane;
                jobs.push(Job::Prepare(Prepare {
                    epoch: self.epoch,
                    purpose: Purpose::Restep,
                    table: slot.table,
                    generation: self.generation,
                    run: piece.run,
                    columns: lane.requested().to_vec(),
                    keyed: lane.keyed(),
                    passing: lane.passing(),
                    hour: piece.first,
                    through: piece.first + span,
                    frames: Some((piece.last, span)),
                    rebuilds: Vec::new(),
                }));
            }
            let count = jobs.len();
            self.shared.queue_joins(jobs);
            self.lend_join_threads(count);
            self.restepping = Some(Restep { quantum, pending, ready: BTreeMap::new() });
            return Ok(true);
        };
        if !restep.pending.is_empty() {
            return Ok(false);
        }
        let Some((position, hour, _)) = lock(&self.shared.ring).plan(self.epoch) else { return Ok(false) };
        let at = hour.min(self.shared.timeline.pieces[self.piece].last);
        let mut heads = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            let restep = self.restepping.as_ref().expect("restepping");
            let fits = match (restep.ready.get(&slot.table), slot.head_at(at)) {
                (Some((prepared, _)), Some(head)) if prepared.lane.head().is_some_and(|sought| sought <= head) && prepared.lane.declared() == slot.lane.declared() => Some(head),
                _ => None,
            };
            let Some(head) = fits else {
                let restep = self.restepping.take().expect("restepping");
                self.shared.free(restep.ready.into_values().map(|(prepared, _)| prepared).collect::<Vec<_>>());
                return Ok(true);
            };
            heads.push(head);
        }
        let restep = self.restepping.as_mut().expect("restepping");
        let mut cursors = Vec::new();
        let mut waiting = false;
        for (slot, &head) in self.slots.iter().zip(&heads) {
            let (prepared, fetching) = restep.ready.get_mut(&slot.table).expect("ready");
            let lane = &mut prepared.lane;
            while lane.head().is_some_and(|reached| reached < head) {
                if *fetching || !lane.holds_through(head) {
                    waiting = true;
                    break;
                }
                let before = lane.head();
                let mut passed = Changes::default();
                lane.step(head, &mut passed)?;
                lane.retire_into(&mut cursors);
                if lane.head() == before {
                    waiting = true;
                    break;
                }
            }
        }
        if !cursors.is_empty() {
            self.shared.free(cursors);
        }
        if waiting {
            return Ok(false);
        }
        let mut ring = lock(&self.shared.ring);
        if ring.epoch != self.epoch || ring.frames() != Some((position, hour)) {
            return Ok(true);
        }
        let (owed, landing) = ring.cut(position);
        drop(ring);
        let mut ready = self.restepping.take().expect("restepping").ready;
        let keep = self.ends_kept();
        let mut old = Vec::with_capacity(self.slots.len());
        for slot in &mut self.slots {
            let (prepared, _) = ready.remove(&slot.table).expect("ready");
            let Prepared { generation, mut lane, .. } = *prepared;
            lane.clear_texts();
            old.push(std::mem::replace(&mut slot.lane, lane));
            (slot.generation, slot.fetching) = (generation, false);
            slot.ends.clear();
            slot.ended(at, keep);
        }
        self.shared.free((old, ready.into_values().map(|(prepared, _)| prepared).collect::<Vec<_>>()));
        let newest = self.newest.expect("stepping");
        self.hidden = Some(newest).filter(|&newest| newest > hour).max(self.hidden);
        self.newest = Some(hour);
        self.restep += 1;
        self.coarse = false;
        self.counters.resteps += 1;
        for (reader, tables) in owed {
            self.owed.entry(reader).or_default().extend(tables);
        }
        self.lands.extend(landing);
        lock(&self.shared.report).2 = self.counters;
        let caught_up = self.caught_up_at();
        self.shared.nudge(|control| {
            self.publish_newest(control, hour);
            self.publish_caught_up(control, caught_up);
        });
        Ok(true)
    }

    fn view_round(&mut self, first: Hour, last: Hour, places: &[usize], fresh: &[bool], round: &mut [Option<Changes>]) {
        if self.hook.is_none() || !fresh.iter().zip(round.iter()).any(|(&fresh, held)| fresh && held.is_some()) {
            return;
        }
        let shown: Vec<usize> = (0..places.len()).filter(|&index| round[index].is_some()).collect();
        let mut view = Step { epoch: self.epoch, first, last, piece: self.piece_at(last), tables: shown.iter().map(|&index| round[index].take().expect("shown")).collect(), ..Step::default() };
        let mask: Vec<bool> = shown.iter().map(|&index| fresh[index]).collect();
        hook_fresh(&mut self.hook, &self.slots, &mut view, &mask);
        for (index, changes) in shown.into_iter().zip(view.tables) {
            round[index] = Some(changes);
        }
    }

    fn land_owed(&mut self) -> io::Result<bool> {
        let Some(newest) = self.newest else { return Ok(false) };
        if (self.owed.is_empty() && self.lands.is_empty()) || self.shared.epoch.load(Ordering::Acquire) != self.epoch {
            return Ok(false);
        }
        let last = self.shared.timeline.pieces[self.piece].last;
        let held = {
            let clock = lock_clock(&self.shared.clock);
            !clock.playing() || clock.held().is_some()
        };
        if held && lock(&self.shared.ring).frames().is_some_and(|(_, hour)| hour.min(last) < newest.min(last)) {
            let parked: Vec<(usize, Reader, Hour)> = {
                let ring = lock(&self.shared.ring);
                self.lands.iter().filter_map(|&id| Some((id, ring.reader_of(id)?, ring.parked(id)?))).collect()
            };
            self.lands.retain(|id| !parked.iter().any(|(held, ..)| held == id));
            for (id, reader, from) in parked {
                self.catch_up(id, reader, from);
            }
            return Ok(self.join_owed());
        }
        let mut targets: Vec<(Option<usize>, Reader, Vec<usize>)> = Vec::new();
        for (reader, tables) in std::mem::take(&mut self.owed) {
            let (now, later): (BTreeSet<usize>, BTreeSet<usize>) = tables.into_iter().partition(|table| !self.joining.contains_key(table));
            if !later.is_empty() {
                self.owed.insert(reader, later);
            }
            let now: Vec<usize> = now.into_iter().filter(|&table| self.slot_of(table).is_some()).collect();
            if !now.is_empty() {
                targets.push((None, reader, now));
            }
        }
        let asked: Vec<(usize, Option<Reader>)> = {
            let ring = lock(&self.shared.ring);
            self.lands.iter().map(|&id| (id, ring.reader_of(id))).collect()
        };
        let had_lands = !asked.is_empty();
        self.lands.clear();
        for (id, reader) in asked {
            let Some(reader) = reader else { continue };
            let tables: Vec<usize> = self.selection.iter().filter(|(_, selected)| selected.readings.iter().any(|reading| reading.reader == reader)).map(|(&table, _)| table).collect();
            if tables.iter().any(|table| self.joining.contains_key(table)) {
                self.lands.push(id);
                continue;
            }
            targets.push((Some(id), reader, tables.into_iter().filter(|&table| self.slot_of(table).is_some()).collect()));
        }
        if targets.is_empty() {
            if had_lands && self.lands.is_empty() {
                let caught_up = self.caught_up_at();
                self.shared.nudge(|control| self.publish_caught_up(control, caught_up));
            }
            return Ok(false);
        }
        let mut work: Vec<(usize, usize, Reader, Vec<usize>, Rebuild)> = Vec::new();
        for (target, (_, reader, tables)) in targets.iter().enumerate() {
            for &table in tables {
                let reading = self.selection.get(&table).and_then(|selected| selected.readings.iter().find(|reading| reading.reader == *reader));
                if let Some(reading) = reading
                    && let Some(rebuild) = &reading.rebuild
                {
                    work.push((target, self.slot_of(table).expect("held"), *reader, reading.columns.clone(), Arc::clone(rebuild)));
                }
            }
        }
        let mut shown: Vec<usize> = work.iter().map(|(_, place, ..)| *place).collect();
        shown.sort_unstable();
        shown.dedup();
        let pending: Vec<Texts> = shown.iter().map(|&place| self.slots[place].lane.show_known()).collect();
        let started = Instant::now();
        let rebuilt = rebuild_all(&mut self.slots, &work);
        self.counters.landing_rebuild_ns += started.elapsed().as_nanos() as u64;
        for (&place, texts) in shown.iter().zip(pending) {
            self.slots[place].lane.hide_known(texts);
        }
        let mut states: Vec<Vec<Rebuilt>> = (0..targets.len()).map(|_| Vec::new()).collect();
        for ((target, place, reader, ..), state) in work.iter().zip(rebuilt) {
            if let Some(state) = self.shared.hold(*reader, state?) {
                states[*target].push(Rebuilt { reader: *reader, table: self.slots[*place].table, state });
            }
        }
        let mut steps = Vec::with_capacity(targets.len());
        for ((consumer, reader, tables), states) in targets.into_iter().zip(states) {
            let mut sets = Vec::with_capacity(tables.len());
            for &table in &tables {
                let mut changes = Changes::default();
                self.slots[self.slot_of(table).expect("held")].lane.reset_into(&mut changes)?;
                sets.push(changes);
            }
            let step = Step { epoch: self.epoch, first: newest + 1, last: newest, piece: Some(self.piece), tables: sets, states: Mutex::new(states), ..Step::default() };
            steps.push((consumer, reader, step));
        }
        let mut ring = lock(&self.shared.ring);
        if ring.epoch != self.epoch {
            drop(ring);
            self.shared.free(steps.into_iter().map(|(_, _, step)| step).collect::<Vec<_>>());
            return Ok(true);
        }
        let before = ring.next_seq();
        self.counters.landings += steps.len() as u64;
        for (consumer, reader, mut step) in steps {
            step.seq = before;
            match consumer {
                Some(id) => ring.jump(id, Arc::new(step)),
                None => {
                    let tables = step.tables.iter().map(Changes::table).collect();
                    let step = Arc::new(step);
                    ring.give(reader, before, &[Arc::clone(&step)]);
                    self.given.push(Given { reader, tables, step, hour: newest, stood: self.stood, read: false });
                }
            }
        }
        drop(ring);
        lock(&self.shared.report).2 = self.counters;
        let caught_up = self.caught_up_at();
        self.shared.nudge(|control| {
            control.published_steps += 1;
            self.publish_caught_up(control, caught_up);
        });
        self.shared.notify_published();
        Ok(true)
    }

    fn schedule(&mut self) {
        let Some(newest) = self.newest else { return };
        let through = newest + self.shared.settings.ahead as Hour * self.quantum();
        let mut jobs = Vec::new();
        let slots = self.slots.iter_mut().map(|slot| (slot.table, slot.generation, &mut slot.fetching, &mut slot.lane));
        let joins = self.joining.iter_mut().filter_map(|(&table, join)| join.prepared.as_mut().map(|prepared| (table, join.generation, &mut join.fetching, &mut prepared.lane)));
        let resteps = self.restepping.iter_mut().flat_map(|restep| restep.ready.iter_mut()).map(|(&table, (prepared, fetching))| (table, prepared.generation, fetching, &mut prepared.lane));
        let catches = self.catching.iter_mut().flat_map(|catch| catch.lanes.iter_mut()).filter_map(|(&table, (_, slot))| slot.as_mut().map(|slot| (table, slot.generation, &mut slot.fetching, &mut slot.lane)));
        for (table, generation, fetching, lane) in slots.chain(joins).chain(resteps).chain(catches) {
            if *fetching {
                continue;
            }
            if let Some(fetch) = lane.want(through) {
                *fetching = true;
                let first = i64::from(fetch.first_hour());
                jobs.push(((2, first), Job::Fetch { epoch: self.epoch, table, generation, fetch }));
            }
        }
        self.shared.queue(jobs);
    }

    fn prepare_next_piece(&mut self) {
        let Some(newest) = self.newest else { return };
        let next = self.piece + 1;
        if next >= self.shared.timeline.pieces.len() {
            self.let_next_piece_go();
            return;
        }
        if self.next_piece.as_ref().is_some_and(|held| held.piece != next) {
            self.let_next_piece_go();
        }
        let pieces = &self.shared.timeline.pieces;
        let quantum = self.quantum();
        let lead = (quantum * (self.ring_size() as Hour + self.shared.settings.ahead as Hour)).max((self.pace * PREPARE_SECONDS) as Hour);
        if pieces[self.piece].last - newest > lead {
            return;
        }
        let (run, hour) = (pieces[next].run, pieces[next].first);
        let through = hour + self.shared.settings.ahead as Hour * quantum;
        let mut piece = self.next_piece.take().unwrap_or(NextPiece { piece: next, pending: BTreeMap::new(), prepared: Vec::new() });
        let mut jobs = Vec::new();
        for (&table, selected) in &self.selection {
            if piece.pending.contains_key(&table) || piece.prepared.iter().any(|prepared| prepared.lane.table() == table) {
                continue;
            }
            self.generation += 1;
            self.counters.piece_prepares += 1;
            piece.pending.insert(table, self.generation);
            let prepare = Prepare {
                epoch: self.epoch,
                purpose: Purpose::Piece(next),
                table,
                generation: self.generation,
                run,
                columns: selected.columns.clone(),
                keyed: selected.keyed,
                passing: selected.passing,
                hour,
                through,
                frames: None,
                rebuilds: selected.rebuilds(None),
            };
            jobs.push(((1, -seek_cost(&*self.shared.pages, run, table, hour)), Job::Prepare(prepare)));
        }
        self.shared.queue(jobs);
        self.next_piece = Some(piece);
    }

    fn let_next_piece_go(&mut self) {
        if let Some(held) = self.next_piece.take()
            && !held.prepared.is_empty()
        {
            self.shared.free(held.prepared);
        }
    }

    fn fresh_step(&self) -> Arc<Step> {
        let (step, spare) = lock(&self.shared.ring).reclaim();
        if !spare.is_empty() {
            self.shared.free(spare);
        }
        let mut step = step.unwrap_or_default();
        let held = Arc::get_mut(&mut step).expect("a reclaimed step is held once");
        self.retire_changes(std::mem::take(&mut held.hours));
        let left = std::mem::take(&mut *lock(&held.states));
        if !left.is_empty() {
            self.shared.free(left);
        }
        held.epoch = self.epoch;
        held.restep = self.restep;
        held.built = Built::default();
        step
    }

    fn retire_changes(&self, changes: Vec<Changes>) {
        if !changes.is_empty() {
            self.shared.free(changes);
        }
    }

    fn every_step(&self, reader: Reader) -> bool {
        self.selection.iter().any(|(&table, selected)| selected.readings.iter().any(|reading| reading.reader == reader && (reading.passing || self.slot_of(table).is_some_and(|place| self.slots[place].lane.log().is_some()))))
    }

    fn land_standing(&mut self, index: usize) -> io::Result<bool> {
        if self.catching[index].lanes.values().any(|(_, slot)| slot.is_none()) {
            self.counters.catch_waits[0] += 1;
            return Ok(false);
        }
        let mut catch = self.catching.swap_remove(index);
        let standing = catch.standing.take().expect("a standing read");
        let mut tables = Vec::with_capacity(catch.lanes.len());
        for (_, slot) in catch.lanes.values() {
            let mut changes = Changes::default();
            slot.as_ref().expect("read").lane.reset_into(&mut changes)?;
            tables.push(changes);
        }
        let step = Arc::new(Step { epoch: self.epoch, first: catch.from + 1, last: catch.from, piece: catch.piece, tables, states: Mutex::new(standing.states), ..Step::default() });
        let landed = lock(&self.shared.ring).land_standing(catch.id, catch.from, step, &standing.revoke);
        if let Err(step) = landed {
            if let Some(given) = self.given.iter_mut().find(|given| Arc::ptr_eq(&given.step, &standing.revoke)) {
                given.read = false;
            }
            self.counters.standing_dropped += 1;
            self.shared.free(step);
        }
        let held = catch.lanes();
        self.shared.free(held);
        lock(&self.shared.report).2 = self.counters;
        let caught_up = self.caught_up_at();
        self.shared.nudge(|control| {
            control.published_steps += 1;
            self.publish_caught_up(control, caught_up);
        });
        self.shared.notify_published();
        Ok(true)
    }

    fn adopt_parked(&mut self) -> bool {
        let parked = lock(&self.shared.ring).parked_seats();
        let held = {
            let clock = lock_clock(&self.shared.clock);
            !clock.playing() || clock.held().is_some()
        };
        let mut moved = false;
        for (id, reader, from) in parked {
            if self.lands.contains(&id) || self.catching.iter().any(|catch| catch.id == id) {
                continue;
            }
            if held || self.every_step(reader) {
                self.catch_up(id, reader, from);
            } else {
                self.lands.push(id);
            }
            self.counters.landed_behind += 1;
            moved = true;
        }
        if moved {
            self.shared.nudge(|control| control.caught_up_at = None);
        }
        moved
    }

    fn catch_up(&mut self, id: usize, reader: Reader, from: Hour) {
        let piece = match self.shared.timeline.region(from) {
            Region::Piece(piece) => Some(piece),
            Region::Gap { .. } => None,
        };
        let mut catch = CatchUp { id, reader, piece, from, at: from, lanes: BTreeMap::new(), steps: Vec::new(), started: Instant::now(), standing: None };
        let jobs = self.catch_reads(&mut catch);
        self.catching.push(catch);
        let count = jobs.len();
        self.shared.queue_joins(jobs);
        self.lend_join_threads(count);
    }

    fn catch_reads(&mut self, catch: &mut CatchUp) -> Vec<Job> {
        let Some(piece) = catch.piece.map(|piece| self.shared.timeline.pieces[piece]) else { return Vec::new() };
        let through = catch.from + self.shared.settings.ahead as Hour * self.quantum();
        let mut jobs = Vec::new();
        let wanted = |table: &usize, selected: &Selected| match &catch.standing {
            Some(standing) => standing.tables.contains(table),
            None => selected.readings.iter().any(|reading| reading.reader == catch.reader),
        };
        let reads: Vec<(usize, Selected)> = self.selection.iter().filter(|(table, selected)| wanted(table, selected)).map(|(&table, selected)| (table, selected.clone())).collect();
        for (table, selected) in reads {
            self.generation += 1;
            catch.lanes.insert(table, (self.generation, None));
            jobs.push(Job::Prepare(Prepare {
                epoch: self.epoch,
                purpose: Purpose::CatchUp,
                table,
                generation: self.generation,
                run: piece.run,
                columns: selected.columns.clone(),
                keyed: selected.keyed,
                passing: selected.passing,
                hour: catch.from,
                through,
                frames: None,
                rebuilds: if catch.standing.is_some() { selected.rebuilds(Some(&[catch.reader])) } else { Vec::new() },
            }));
        }
        jobs
    }

    fn restart_catch_ups(&mut self) {
        let mut jobs = Vec::new();
        for mut catch in std::mem::take(&mut self.catching) {
            let lanes: Vec<Box<Lane>> = std::mem::take(&mut catch.lanes).into_values().filter_map(|(_, slot)| slot.map(|slot| slot.lane)).collect();
            let states = catch.standing.as_mut().map(|standing| std::mem::take(&mut standing.states));
            self.shared.free((lanes, std::mem::take(&mut catch.steps), states));
            catch.at = catch.from;
            jobs.extend(self.catch_reads(&mut catch));
            self.catching.push(catch);
        }
        let count = jobs.len();
        self.shared.queue_joins(jobs);
        self.lend_join_threads(count);
    }

    fn land_catch_ups(&mut self) -> io::Result<bool> {
        if self.catching.is_empty() || self.shared.epoch.load(Ordering::Acquire) != self.epoch {
            return Ok(false);
        }
        let mut moved = false;
        let mut index = 0;
        while index < self.catching.len() {
            if self.catching[index].standing.is_some() {
                if self.land_standing(index)? {
                    moved = true;
                } else {
                    index += 1;
                }
                continue;
            }
            let target = lock(&self.shared.ring).catch_hour(self.catching[index].id);
            let Some(target) = target else {
                let held = self.catching.swap_remove(index).lanes();
                self.shared.free(held);
                moved = true;
                continue;
            };
            let catch = &mut self.catching[index];
            if catch.lanes.values().any(|(_, slot)| slot.as_ref().is_none_or(|slot| slot.fetching)) {
                self.counters.catch_waits[0] += 1;
                index += 1;
                continue;
            }
            if catch.lanes.values().any(|(_, slot)| slot.as_ref().is_some_and(|slot| !slot.lane.holds_through(target))) {
                self.counters.catch_waits[1] += 1;
                index += 1;
                continue;
            }
            let ends = catch.piece.map(|piece| self.shared.timeline.pieces[piece].last);
            let tables: Vec<usize> = catch.lanes.keys().copied().collect();
            let goals: Option<BTreeMap<usize, Hour>> = tables.iter().map(|&table| Some((table, if ends == Some(target) { target } else { self.slots[self.slot_of(table)?].head_at(target)? }))).collect();
            let Some(goals) = goals else {
                self.counters.catch_waits[2] += 1;
                index += 1;
                continue;
            };
            let catch = &mut self.catching[index];
            let at_goal = |catch: &CatchUp| catch.lanes.iter().all(|(table, (_, slot))| slot.as_ref().is_some_and(|slot| slot.lane.head().is_some_and(|head| head >= goals[table])));
            loop {
                let mut tables = Vec::new();
                let mut stepped = false;
                for (table, (_, slot)) in &mut catch.lanes {
                    let Some(slot) = slot.as_mut() else { continue };
                    let before = slot.lane.head();
                    if before.is_some_and(|head| head >= goals[table]) {
                        continue;
                    }
                    let mut changes = Changes::default();
                    step_slot(slot, goals[table], &mut changes, None)?;
                    stepped |= slot.lane.head() != before;
                    tables.push(changes);
                }
                let done = at_goal(catch);
                if done {
                    for (table, (_, slot)) in &mut catch.lanes {
                        let Some(slot) = slot.as_mut() else { continue };
                        if !tables.iter().any(|changes: &Changes| changes.table() == *table) {
                            let mut changes = Changes::default();
                            step_slot(slot, goals[table], &mut changes, None)?;
                            tables.push(changes);
                        }
                    }
                    tables.sort_by_key(Changes::table);
                }
                let last = if done { target } else { catch.lanes.values().filter_map(|(_, slot)| slot.as_ref()?.lane.head()).max().unwrap_or(catch.at).max(catch.at) };
                if last > catch.at || !tables.is_empty() {
                    catch.steps.push(Arc::new(Step { epoch: self.epoch, first: catch.at + 1, last, piece: catch.piece, tables, ..Step::default() }));
                    catch.at = last;
                    moved = true;
                }
                if done || !stepped {
                    break;
                }
            }
            if !at_goal(catch) {
                self.counters.catch_waits[1] += 1;
                index += 1;
                continue;
            }
            let at = catch.at;
            let (id, steps) = (self.catching[index].id, std::mem::take(&mut self.catching[index].steps));
            let landed = lock(&self.shared.ring).land_at(id, at, steps);
            match landed {
                Ok(()) => {
                    let catch = self.catching.swap_remove(index);
                    let ns = catch.started.elapsed().as_nanos() as u64;
                    self.counters.caught_up += 1;
                    self.counters.catch_up_ns += ns;
                    self.counters.catch_up_most_ns = self.counters.catch_up_most_ns.max(ns);
                    let held = catch.lanes();
                    self.shared.free(held);
                    lock(&self.shared.report).2 = self.counters;
                    let caught_up = self.caught_up_at();
                    self.shared.nudge(|control| {
                        control.published_steps += 1;
                        self.publish_caught_up(control, caught_up);
                    });
                    self.shared.notify_published();
                    moved = true;
                }
                Err(steps) => {
                    self.counters.catch_waits[3] += 1;
                    self.catching[index].steps = steps;
                    index += 1;
                }
            }
        }
        Ok(moved)
    }

    fn publish(&mut self, step: Arc<Step>) -> io::Result<()> {
        if self.hidden.is_none_or(|hidden| step.last > hidden) {
            self.hook_requested = false;
            if let Some(hook) = &mut self.hook {
                hook(&step, &Lanes { slots: &self.slots });
            }
        }
        if self.hidden.is_some_and(|hidden| step.last >= hidden) {
            self.hidden = None;
        }
        let last = step.last;
        let held = lock(&self.shared.ring).publish(step);
        match held {
            Ok(()) => {
                self.release();
                self.newest = Some(last);
                self.counters.steps += 1;
                lock(&self.shared.report).2 = self.counters;
                let caught_up = self.caught_up_at();
                self.shared.nudge(|control| {
                    self.publish_newest(control, last);
                    control.published_steps += 1;
                    self.publish_caught_up(control, caught_up);
                });
                self.shared.notify_published();
            }
            Err(step) => self.shared.free(step),
        }
        Ok(())
    }

    fn build(&mut self) -> io::Result<bool> {
        let Some(newest) = self.newest else { return Ok(false) };
        self.head = lock(&self.shared.control).head;
        if !lock(&self.shared.ring).room(self.head) {
            self.why = if newest < self.head { IDLE_ROOM_BEHIND } else { IDLE_RING_FULL };
            self.hold();
            return Ok(false);
        }
        if self.shared.timeline.region(newest) == Region::Piece(self.piece) {
            let behind = |slot: &&Slot| slot.lane.head().is_some_and(|head| head < newest);
            let (mut waiting, mut ready) = (false, false);
            for slot in self.slots.iter().filter(behind) {
                if slot.fetching { waiting = true } else { ready = true }
            }
            if ready {
                self.counters.catch_ups += 1;
                self.step_to(newest + 1, newest, self.piece)?;
                return Ok(true);
            }
            if waiting && self.head <= newest {
                self.why = IDLE_FETCH;
                return Ok(false);
            }
        }
        if self.hold_for_paused_seed() || self.hold_for_paused_reads(newest) {
            return Ok(false);
        }
        let timeline = &self.shared.timeline;
        if newest >= timeline.last() {
            self.why = IDLE_END;
            return Ok(false);
        }
        let quantum = self.quantum();
        self.paced = quantum;
        let start = newest + 1;
        let target = if newest < self.head { self.head } else { newest + quantum };
        let target = self.hidden.filter(|&hidden| hidden > newest).map_or(target, |hidden| target.min(hidden));
        match timeline.region(start) {
            Region::Gap { next } => {
                let end = target.min(timeline.pieces[next].first - 1);
                let mut step = self.fresh_step();
                let held = Arc::get_mut(&mut step).expect("a fresh step is held once");
                self.retire_changes(std::mem::take(&mut held.tables));
                (held.first, held.last, held.piece, held.reset) = (start, end, None, false);
                held.built.fetched = std::mem::take(&mut self.fetching);
                self.publish(step)?;
                Ok(true)
            }
            Region::Piece(piece) if piece != self.piece => self.enter_piece(piece),
            Region::Piece(piece) => {
                let held = timeline.pieces[piece];
                let run = &self.shared.pages.runs()[held.run];
                let block_end = run.first_hour + (((start - run.first_hour) as u32 / BLOCK + 1) * BLOCK) as Hour - 1;
                let end = target.min(held.last).min(block_end);
                self.step_to(start, end, piece)?;
                Ok(true)
            }
        }
    }

    fn enter_piece(&mut self, piece: usize) -> io::Result<bool> {
        let ready = self.next_piece.as_ref().is_some_and(|next| {
            next.piece == piece
                && next.pending.is_empty()
                && self.selection.keys().all(|table| next.prepared.iter().any(|held| held.lane.table() == *table))
        });
        if !ready {
            if !self.waiting_piece {
                self.waiting_piece = true;
                self.counters.piece_waits += 1;
            }
            return Ok(false);
        }
        self.waiting_piece = false;
        let mut next = self.next_piece.take().expect("ready");
        let mut prepared = std::mem::take(&mut next.prepared);
        prepared.sort_by_key(|held| held.lane.table());
        let hour = self.shared.timeline.pieces[piece].first;
        let started = Instant::now();
        let mut step = self.fresh_step();
        let made = Arc::get_mut(&mut step).expect("a fresh step is held once");
        let mut buffers = std::mem::take(&mut made.tables).into_iter();
        let mut tables = Vec::with_capacity(prepared.len());
        let mut states = Vec::new();
        let (mut crossed, mut resets, mut rows, mut cells) = (0, 0, 0, 0);
        for prepared in &mut prepared {
            let table = prepared.lane.table();
            let mut changes = buffers.next().unwrap_or_default();
            let at = self.slots.iter().position(|slot| slot.table == table);
            let carried = match at {
                Some(at) => crate::boundary::crossed(&self.slots[at].lane, &mut prepared.lane, &mut changes)?,
                None => None,
            };
            match carried {
                Some(held) => {
                    crossed += 1;
                    rows += u64::from(held.gone) + u64::from(held.born);
                    cells += u64::from(held.cells);
                }
                None => {
                    resets += 1;
                    prepared.lane.reset_into(&mut changes)?;
                    self.owed.values_mut().for_each(|owed| {
                        owed.remove(&table);
                    });
                }
            }
            for (reader, state) in std::mem::take(&mut prepared.states) {
                states.push(Rebuilt { reader, table, state });
            }
            tables.push(changes);
        }
        self.retire_changes(buffers.collect());
        let lanes: Vec<Box<Lane>> = std::mem::take(&mut self.slots).into_iter().map(|slot| slot.lane).collect();
        if !lanes.is_empty() {
            self.shared.free(lanes);
        }
        let abandoned = self.abandon_prepared();
        if !abandoned.is_empty() {
            self.shared.free(abandoned);
        }
        for held in prepared {
            let Prepared { generation, lane, .. } = held;
            self.slots.push(Slot::new(generation, lane));
        }
        self.piece = piece;
        let mut built = Built { threads: 1, ..Built::default() };
        for table in &tables {
            built.cells += table.cells() as u64;
            built.rows += table.rows() as u64;
            built.bytes += table.payload() as u64;
        }
        built.fetched = std::mem::take(&mut self.fetching);
        let ns = started.elapsed().as_nanos() as u64;
        built.ns = ns;
        let counted = &mut self.counters;
        counted.pieces_crossed += 1;
        counted.crossed_tables += crossed;
        counted.crossed_resets += resets;
        counted.crossed_rows += rows;
        counted.crossed_cells += cells;
        counted.cross_ns += ns;
        counted.cross_most_ns = counted.cross_most_ns.max(ns);
        let keep = self.ends_kept();
        self.slots.iter_mut().for_each(|slot| slot.ended(hour, keep));
        made.tables = tables;
        *lock(&made.states) = states;
        (made.first, made.last, made.piece, made.reset, made.built) = (hour, hour, Some(piece), false, built);
        self.publish(step)?;
        self.slots.iter_mut().for_each(|slot| slot.lane.clear_texts());
        Ok(true)
    }

    fn step_to(&mut self, start: Hour, end: Hour, piece: usize) -> io::Result<()> {
        let (started, cycles) = (Instant::now(), replay_frame::cpu::thread_cycles());
        let mut step = self.fresh_step();
        let held = Arc::get_mut(&mut step).expect("a fresh step is held once");
        let mut buffers = std::mem::take(&mut held.tables);
        let mut changes: Vec<Changes> = Vec::with_capacity(self.slots.len());
        for at in 0..self.slots.len() {
            changes.push(buffers.get_mut(at).map(std::mem::take).unwrap_or_default());
        }
        self.retire_changes(buffers.into_iter().filter(|changes| changes.bytes_held() > 0).collect());
        let mut hours: Vec<Option<Vec<Changes>>> = self.slots.iter().map(|slot| self.selection.get(&slot.table).is_some_and(|selected| selected.hourly).then(Vec::new)).collect();
        let threads = self.step_lanes(end, &mut changes, &mut hours)?;
        let mut cursors = Vec::new();
        let keep = self.ends_kept();
        for slot in &mut self.slots {
            slot.lane.retire_into(&mut cursors);
            slot.ended(end, keep);
        }
        if !cursors.is_empty() {
            self.shared.free(cursors);
        }
        let mut built = Built { threads, ..Built::default() };
        for table in &changes {
            built.cells += table.cells() as u64;
            built.rows += table.rows() as u64;
            built.bytes += table.payload() as u64;
            built.stalled += u32::from(table.stalled());
            if table.short() > 0 {
                built.short += 1;
                built.most_short = built.most_short.max(table.short());
            }
        }
        self.counters.stalls += u64::from(built.stalled);
        self.counters.stalled_steps += u64::from(built.stalled > 0);
        built.fetched = std::mem::take(&mut self.fetching);
        held.tables = changes;
        held.hours = hours.into_iter().flatten().flatten().collect();
        (held.first, held.last, held.piece, held.reset) = (start, end, Some(piece), false);
        built.ns = started.elapsed().as_nanos() as u64;
        built.cycles = replay_frame::cpu::thread_cycles().saturating_sub(cycles);
        held.built = built;
        self.publish(step)?;
        Ok(())
    }

    fn step_lanes(&mut self, end: Hour, changes: &mut [Changes], hours: &mut [Option<Vec<Changes>>]) -> io::Result<u32> {
        let quantum = self.quantum();
        for (slot, hours) in self.slots.iter().zip(hours.iter_mut()) {
            if slot.lane.head().is_none_or(|head| end - head > quantum) {
                *hours = None;
            }
        }
        let predicted = |slot: &Slot| slot.cost * f64::from((end - slot.lane.head().unwrap_or(end)).max(0) as u32);
        let total: f64 = self.slots.iter().map(predicted).sum();
        if self.helpers.is_empty() || total < SPLIT_NS {
            for ((slot, changes), hours) in self.slots.iter_mut().zip(changes.iter_mut()).zip(hours.iter_mut()) {
                step_slot(slot, end, changes, hours.as_mut())?;
            }
            return Ok(1);
        }
        self.counters.split_steps += 1;
        let mut items = Vec::with_capacity(self.slots.len());
        for (index, ((slot, buffer), hours)) in std::mem::take(&mut self.slots).into_iter().zip(changes.iter_mut()).zip(hours.iter_mut()).enumerate() {
            items.push(Item { index, predicted: predicted(&slot), slot, changes: std::mem::take(buffer), hours: hours.take(), result: Ok(()) });
        }
        items.sort_by(|a, b| a.predicted.total_cmp(&b.predicted));
        let wanted = ((total / SPLIT_NS).ceil() as usize).clamp(2, self.helpers.len() + 1);
        let batch = Arc::new(Batch { to: end, left: AtomicUsize::new(items.len()), queue: Mutex::new(items), done: Mutex::new(Vec::new()), finished: Condvar::new(), threads: AtomicUsize::new(0) });
        for helper in self.helpers.iter().take(wanted - 1) {
            let _ = helper.send(Help::Batch(Arc::clone(&batch)));
        }
        batch.work();
        batch.wait();
        let threads = batch.threads.load(Ordering::Relaxed) as u32;
        let mut done = std::mem::take(&mut *lock(&batch.done));
        done.sort_by_key(|item| item.index);
        let mut slots = Vec::with_capacity(done.len());
        let mut failed = None;
        for item in done {
            if let Err(error) = item.result {
                failed.get_or_insert(error);
            }
            changes[item.index] = item.changes;
            hours[item.index] = item.hours;
            slots.push(item.slot);
        }
        self.slots = slots;
        match failed {
            Some(error) => Err(error),
            None => Ok(threads),
        }
    }

    fn report(&mut self, force: bool) {
        if !force && self.report_at.elapsed() < REPORT_EVERY {
            return;
        }
        self.report_at = Instant::now();
        let mut report = Report { lanes: self.slots.len(), ..Report::default() };
        for slot in &self.slots {
            report.held.add(&slot.lane.held());
        }
        let steps = {
            let mut ring = lock(&self.shared.ring);
            if !self.slots.is_empty() {
                ring.set_bound(report.held.cells);
            }
            ring.steps()
        };
        report.ring_steps = steps.len();
        report.ring_bytes = steps.iter().map(|step| step.bytes_held()).sum();
        drop(steps);
        {
            let jobs = lock(&self.shared.jobs);
            report.fetches_queued = jobs.heap.iter().filter(|queued| queued.rank.0 == 2).count();
            report.jobs_running = jobs.running;
        }
        report.freed_ns = self.shared.freed_ns.load(Ordering::Relaxed);
        let preparing = self.joining.values().filter_map(|join| join.prepared.as_deref()).chain(self.next_piece.iter().flat_map(|next| next.prepared.iter())).chain(self.seek.iter().flat_map(|seek| seek.prepared.iter()));
        for prepared in preparing {
            report.preparing += 1;
            report.preparing_held.add(&prepared.lane.held());
        }
        let ((states, state_bytes), (peak_states, peak_state_bytes)) = self.shared.flight.read();
        (report.states, report.state_bytes) = (states, state_bytes);
        report.total = report.held.total() + report.ring_bytes + report.preparing_held.total() + report.state_bytes;
        if report.total > self.peak.total {
            self.peak = report;
        }
        (self.peak.states, self.peak.state_bytes) = (peak_states, peak_state_bytes);
        *lock(&self.shared.report) = (report, self.peak, self.counters);
    }
}

fn rebuild_all(slots: &mut Vec<Slot>, work: &[(usize, usize, Reader, Vec<usize>, Rebuild)]) -> Vec<io::Result<State>> {
    let mut by_place: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, (_, place, ..)) in work.iter().enumerate() {
        by_place.entry(*place).or_default().push(index);
    }
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(by_place.len());
    let mut held: Vec<Option<Slot>> = std::mem::take(slots).into_iter().map(Some).collect();
    let queue: Mutex<Vec<(usize, Slot, Vec<usize>)>> = Mutex::new(by_place.into_iter().map(|(place, indexes)| (place, held[place].take().expect("a slot"), indexes)).collect());
    let done: Mutex<Vec<(usize, Slot, Vec<(usize, io::Result<State>)>)>> = Mutex::new(Vec::new());
    let work_on = || {
        loop {
            let Some((place, slot, indexes)) = lock(&queue).pop() else { break };
            let states = indexes.into_iter().map(|index| (index, (work[index].4)(&slot.lane, &work[index].3))).collect();
            lock(&done).push((place, slot, states));
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(&work_on);
        }
        work_on();
    });
    let mut states: Vec<Option<io::Result<State>>> = (0..work.len()).map(|_| None).collect();
    for (place, slot, rebuilt) in done.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        held[place] = Some(slot);
        for (index, state) in rebuilt {
            states[index] = Some(state);
        }
    }
    *slots = held.into_iter().map(|slot| slot.expect("given back")).collect();
    states.into_iter().map(|state| state.expect("rebuilt")).collect()
}

fn hook_fresh(hook: &mut Option<Hook>, slots: &[Slot], step: &mut Step, fresh: &[bool]) {
    let Some(hook) = hook else { return };
    if !fresh.contains(&true) {
        return;
    }
    if !fresh.contains(&false) {
        hook(step, &Lanes { slots });
        return;
    }
    let mut aside = Vec::new();
    for (changes, &new) in std::mem::take(&mut step.tables).into_iter().zip(fresh) {
        if new { step.tables.push(changes) } else { aside.push(changes) }
    }
    hook(step, &Lanes { slots });
    let (mut shown, mut aside) = (std::mem::take(&mut step.tables).into_iter(), aside.into_iter());
    step.tables = fresh.iter().map(|&new| if new { shown.next() } else { aside.next() }.expect("one change set a table")).collect();
}

fn union(readers: Vec<(Reader, Declaration)>) -> BTreeMap<usize, Selected> {
    let mut selection: BTreeMap<usize, Selected> = BTreeMap::new();
    for (reader, declaration) in readers {
        for (held, since) in declaration.tables.into_iter().zip(declaration.since) {
            let entry = selection.entry(held.table).or_insert_with(|| Selected { columns: Vec::new(), keyed: false, passing: false, hourly: false, readings: Vec::new() });
            entry.columns.extend_from_slice(&held.columns);
            entry.keyed |= held.keyed;
            entry.passing |= held.passing;
            entry.hourly |= held.hourly;
            let mut columns = declaration.projections.get(&held.table).cloned().unwrap_or(held.columns);
            columns.sort_unstable();
            columns.dedup();
            entry.readings.push(Reading { reader, columns, keyed: held.keyed, passing: held.passing, since, rebuild: declaration.rebuild.clone() });
        }
    }
    for selected in selection.values_mut() {
        selected.columns.sort_unstable();
        selected.columns.dedup();
    }
    selection
}
