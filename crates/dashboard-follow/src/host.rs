
use crate::{
    Follow,
    live::{Group, GroupState, Live, cells_id},
    panel::{self, Panel, PanelData, PanelReader, PanelRows},
    push::{PANEL, Push},
    registry::{Anchor, Hosted, lacking},
};
use serde_json::Value;
use replay_frame::frame::DecodedFrame;
use std::collections::{BTreeMap, BTreeSet};
use playhead::{
    Declared, Playhead, Reader, Watcher,
    pages::Pages,
};
use replay_data::Hour;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, OnceLock, PoisonError, Weak, mpsc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_micros(16_667);
const PANEL_POLL: Duration = Duration::from_millis(8);

struct WaitGate {
    version: AtomicU64,
    state: Mutex<bool>,
    cv: Condvar,
}

impl WaitGate {
    fn new() -> Self {
        Self { version: AtomicU64::new(0), state: Mutex::new(false), cv: Condvar::new() }
    }

    fn stamp(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn wake(&self) {
        self.version.fetch_add(1, Ordering::Release);
        let _guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        drop(_guard);
        self.cv.notify_all();
    }

    fn stop(&self) {
        self.version.fetch_add(1, Ordering::Release);
        let mut stopped = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *stopped = true;
        drop(stopped);
        self.cv.notify_all();
    }

    fn wait(&self, seen: u64, timeout: Option<Duration>) -> bool {
        let mut stopped = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            return true;
        }
        if self.version.load(Ordering::Acquire) != seen {
            return false;
        }
        stopped = match timeout {
            Some(dur) if !dur.is_zero() => self.cv.wait_timeout(stopped, dur).unwrap_or_else(PoisonError::into_inner).0,
            Some(_) => return false,
            None => self.cv.wait(stopped).unwrap_or_else(PoisonError::into_inner),
        };
        *stopped
    }
}

pub fn held_tables(pages: &dyn Pages, tables: usize) -> Vec<bool> {
    pages.held_tables(tables)
}

struct Shown {
    panel: Box<dyn Panel>,
    reader: Box<dyn PanelReader>,
    frame: Option<Arc<DecodedFrame>>,
    stopped: Option<(String, u64)>,
    pushed: Option<Hour>,
    version: (u64, u64),
    class: (u32, u32),
}

impl Shown {
    fn new(panel: Box<dyn Panel>, reader: Box<dyn PanelReader>, class: (u32, u32)) -> Self {
        Self { panel, reader, frame: None, stopped: None, pushed: None, version: (0, 0), class }
    }
}

fn release(ids: &mut Vec<(Group, u32)>, cells: Option<&HostCells>) {
    if let Some(cells) = cells {
        for (_, id) in ids.drain(..) {
            let _ = cells.follow.unfollow(id);
        }
    }
    ids.clear();
}

#[derive(Clone)]
struct HostCells {
    follow: Follow,
    held: Vec<bool>,
}

fn retain_lanes(id: u32, read: &[Declared], host: Option<&HostCells>) {
    let Some(host) = host else { return };
    let held: Vec<Declared> = read.iter().filter(|wanted| host.held.get(wanted.table) == Some(&true)).cloned().collect();
    if !held.is_empty() {
        let _ = host.follow.retain_product_lanes(id, &held);
    }
}

fn register(group: &Group, ids: &mut Vec<(Group, u32)>, kept: &mut Vec<(Group, u32)>, cells: Option<&HostCells>, class: (u32, u32), seed_paused: bool) -> Result<u32, String> {
    let Some(cells) = cells else { return Err("This playhead follows no live cells".into()) };
    if let Some(reason) = lacking(group.table.index(), cells.held.get(group.table.index()).copied().unwrap_or(false)) {
        return Err(reason);
    }
    if let Some(at) = ids.iter().position(|(held, _)| held == group) {
        let (held, id) = ids.swap_remove(at);
        kept.push((held, id));
        return Ok(id);
    }
    let id = cells_id(class.0, class.1);
    match cells.follow.follow_cells(id, group.query(), seed_paused) {
        Ok(()) => {
            kept.push((group.clone(), id));
            Ok(id)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn with_cells(frame: &DecodedFrame, data: &PanelData, live: Option<&Live>, ids: &mut Vec<(Group, u32)>, cells: Option<&HostCells>, class: (u32, u32)) -> (PanelData, Vec<Declared>) {
    let (Ok(reply @ Value::Object(_)), Some(live)) = (data, live.filter(|live| !live.is_empty())) else {
        release(ids, cells);
        return (data.clone(), Vec::new());
    };
    let mut kept = Vec::with_capacity(live.groups.len());
    let mut cell_lanes = Vec::new();
    let states: Vec<GroupState> = live
        .groups
        .iter()
        .map(|group| {
            let seeded = group.seed(frame);
            match register(group, ids, &mut kept, cells, class, seeded.is_none()) {
                Ok(id) => {
                    cell_lanes.push(Declared { table: group.table.index(), columns: group.columns.clone(), keyed: false, passing: true, hourly: false });
                    match seeded {
                        Some(values) => GroupState::Seeded { follow: id, hour: frame.hour, values },
                        None => GroupState::Follow(id),
                    }
                }
                Err(reason) => GroupState::Unavailable(reason),
            }
        })
        .collect();
    release(ids, cells);
    *ids = kept;
    let mut reply = reply.clone();
    if let Some(fields) = reply.as_object_mut() {
        fields.insert("live".into(), live.json(&states));
    }
    (Ok(reply), cell_lanes)
}

type PanelTask = Box<dyn FnOnce() + Send>;

fn run_panel(job: impl FnOnce() + Send + 'static) {
    static JOBS: OnceLock<mpsc::Sender<PanelTask>> = OnceLock::new();
    let jobs = JOBS.get_or_init(|| {
        let (send, receive) = mpsc::channel::<PanelTask>();
        let receive = Arc::new(Mutex::new(receive));
        let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(4));
        for i in 0..threads {
            let receive = receive.clone();
            std::thread::Builder::new().name(format!("follow panel {i}")).spawn(move || loop {
                let job = receive.lock().unwrap_or_else(PoisonError::into_inner).recv();
                match job { Ok(job) => { let _ = catch_unwind(AssertUnwindSafe(job)); }, Err(_) => break }
            }).expect("a follow panel worker");
        }
        send
    });
    jobs.send(Box::new(job)).expect("follow panel workers");
}

struct ParkedPanel {
    hour: Hour,
    epoch: u64,
    token: Arc<AtomicBool>,
    task: PanelTask,
}

struct PanelParking {
    wake: panel::Wake,
    jobs: Mutex<Vec<ParkedPanel>>,
}

impl PanelParking {
    fn park(&self, job: ParkedPanel) {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).push(job);
        (self.wake)();
    }

    fn ready(&self, epoch: u64, held_at: Option<Hour>) -> Vec<PanelTask> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).extract_if(.., |job| {
            job.epoch != epoch || job.token.load(Ordering::Acquire) || held_at.is_none_or(|head| job.hour <= head)
        }).map(|job| job.task).collect()
    }
}

struct Work<T> {
    state: Mutex<(Option<T>, bool)>,
}

impl<T> Work<T> {
    fn new(value: Option<T>) -> Self { Self { state: Mutex::new((value, false)) } }

    fn take(&self) -> Option<T> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let value = state.0.take();
        if value.is_none() { state.1 = true; }
        value
    }

    fn put(&self, value: T) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = Some(value);
        std::mem::take(&mut state.1)
    }
}

#[derive(Default)]
struct Panels {
    stamp: (i32, u32, u32, u32),
    products: BTreeMap<u32, Arc<Work<StepProduct>>>,
    due: BTreeSet<(Hour, u32)>,
    stream: Option<(u64, u32, u64, Option<usize>)>,
    resend: Option<Hour>,
    pace: Option<(bool, f64)>,
    rows: Option<Arc<dyn PanelRows>>,
    push: Option<Arc<Push>>,
    shown: BTreeMap<u32, Arc<Work<Shown>>>,
    frame_version: u64,
    thread: Option<JoinHandle<()>>,
    cells: Option<HostCells>,
    session: u64,
}

fn unread_from(proven: Option<&(u32, Hour)>, seek: u32, through: Hour) -> Option<Hour> {
    proven.filter(|&&(at, _)| at == seek).map(|&(_, hour)| hour).filter(|&hour| hour < through).map(|hour| hour.saturating_add(1))
}

#[derive(Default)]
struct ProductReads {
    epoch: u64,
    seek: u32,
    tokens: BTreeMap<u32, Arc<AtomicBool>>,
    cells: BTreeMap<u32, Vec<(Group, u32)>>,
    host: Option<HostCells>,
    proven: BTreeMap<u32, (u32, Hour)>,
}

impl ProductReads {
    fn current(&self, epoch: u64, seek: u32) -> bool { epoch >= self.epoch && seek == self.seek }

    fn anchor(&mut self, epoch: u64, seek: u32) {
        if (self.epoch, self.seek) == (epoch, seek) { return; }
        self.epoch = epoch;
        self.seek = seek;
        for token in self.tokens.values_mut() { std::mem::take(token).store(true, Ordering::Release); }
    }

    fn remove(&mut self, id: u32) {
        self.proven.remove(&id);
        if let Some(token) = self.tokens.remove(&id) {
            token.store(true, Ordering::Release);
            if let Some(mut cells) = self.cells.remove(&id) { release(&mut cells, self.host.as_ref()); }
            if let Some(host) = &self.host { let _ = host.follow.clear_product_lanes(id); }
        }
    }

    fn register(&mut self, id: u32) {
        self.remove(id);
        self.tokens.insert(id, Arc::default());
    }
}

struct StepProduct {
    id: u32,
    pending_seed: Option<(u64, u32, Hour)>,
    started_on: Option<(u64, u32)>,
    read: panel::ProductReader,
    pending_preparation: Option<panel::PendingPreparation>,
    wire: panel::product::Delivery,
    last: Option<(u64, u32, Hour)>,
    catch_up_costs: [Option<Duration>; 2],
    joined: bool,
    resume: Option<PanelResume>,
    cells: Vec<(Group, u32)>,
    class: (u32, u32),
    host: Option<HostCells>,
    followed: Option<Arc<PanelData>>,
    input: Option<Arc<PanelData>>,
    resend: bool,
}

enum PanelResume {
    Read(Hour),
    Pending(Hour, Option<Arc<AtomicBool>>),
    Ready(Hour, u8, Vec<u8>),
}

impl PanelResume {
    fn hour(&self) -> Hour {
        match self {
            Self::Read(hour) | Self::Pending(hour, _) | Self::Ready(hour, ..) => *hour,
        }
    }

    fn due_read(&self, through: Hour) -> Option<Hour> {
        if self.hour() > through { return None; }
        match self {
            Self::Read(_) => Some(through),
            Self::Pending(hour, ready) if ready.as_ref().is_none_or(|ready| ready.load(Ordering::Acquire)) => Some(*hour),
            _ => None,
        }
    }
}

enum PanelRead {
    Seed(Hour),
    CatchUp { through: Hour, quantum: Hour, parked: bool },
    Resume(Hour),
}

struct PanelClock {
    epoch: u64,
    head: Hour,
    playing: Option<(f64, Hour)>,
    newest: Option<Hour>,
}

fn playing_catch_up_hour(last: Hour, quantum: Hour, head: Hour, speed: f64, newest: Hour, costs: [Option<Duration>; 2]) -> Hour {
    let next = last.saturating_add(quantum);
    let cost = costs.into_iter().flatten().max().unwrap_or_default();
    let quantum = f64::from(quantum);
    let distance = f64::from(head) - f64::from(last) + cost.as_secs_f64() * speed;
    let aimed = (f64::from(last) + (distance / quantum).ceil() * quantum) as Hour;
    next.max(aimed).min(newest)
}

struct PanelJob {
    stamp: (i32, u32, u32),
    session: u64,
    epoch: u64,
    token: Arc<AtomicBool>,
    reads: Arc<Mutex<ProductReads>>,
    push: Arc<Push>,
    wake: panel::Wake,
    parking: Weak<PanelParking>,
}

impl PanelJob {
    fn start<E: Send + Sync + 'static>(self, read: PanelRead, product: StepProduct, work: Arc<Work<StepProduct>>, owner: &Arc<E>, get: fn(&E) -> &Playhead) {
        if let PanelRead::Seed(hour) = read {
            let (clock, route, seek) = self.stamp;
            let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
            if reads.current(self.epoch, seek) && !self.token.load(Ordering::Acquire) && get(owner).epoch() == self.epoch {
                let from = match product.last.filter(|(_, g, _)| *g == seek) {
                    Some((_, _, h)) => h.checked_add(1),
                    None => Some(hour),
                };
                if let Some(from) = from {
                    reads.proven.remove(&product.id);
                    self.push.offer_from(self.session, PANEL, panel::dated::encoded(clock, route, seek, product.id, from, panel::dated::WAITING, &[]));
                }
            }
        }
        let owner = Arc::downgrade(owner);
        run_panel(move || self.run(read, product, work, move || {
            let owner = owner.upgrade()?;
            let playhead = get(&owner);
            let (epoch, head, speed) = playhead.clock_state_at(Instant::now());
            let playing = speed.map(|speed| (speed, playhead.step_hours()));
            Some(PanelClock { epoch, head, playing, newest: speed.and_then(|_| playhead.newest()) })
        }));
    }

    fn run(self, read: PanelRead, mut product: StepProduct, work: Arc<Work<StepProduct>>, clock: impl Fn() -> Option<PanelClock> + Send + 'static) {
        let head = || clock().map(|state| (state.epoch, state.head));
        let mut preparation = None;
        match read {
            PanelRead::Seed(hour) => self.seed(&mut product, hour, head),
            PanelRead::Resume(hour) => preparation = self.prepare_resume(&mut product, hour, &clock),
            PanelRead::CatchUp { through, quantum, parked } => {
                if let Some((_, _, mut last)) = product.last {
                    while last < through {
                        let mut hour = last.saturating_add(quantum).min(through);
                        if let Some(state) = clock() && state.epoch == self.epoch && !self.token.load(Ordering::Acquire) {
                            if let Some((speed, quantum)) = state.playing {
                                if parked {
                                    preparation = self.prepare_resume(&mut product, through, &clock);
                                    break;
                                }
                                let newest = through.max(state.newest.unwrap_or(through));
                                hour = playing_catch_up_hour(last, quantum, state.head, speed, newest, product.catch_up_costs);
                            } else if hour > state.head {
                                match self.push.reply_lead_full() {
                                    Err(()) => break,
                                    Ok(true) => {
                                        let Some(parking) = self.parking.upgrade() else { break };
                                        parking.park(ParkedPanel {
                                            hour, epoch: self.epoch, token: self.token.clone(),
                                            task: Box::new(move || self.run(PanelRead::CatchUp { through, quantum, parked: true }, product, work, clock)),
                                        });
                                        return;
                                    }
                                    Ok(false) => {}
                                }
                            }
                        }
                        let started = Instant::now();
                        if !self.read_one(&mut product, hour, false, false, true, &head) {
                            product.resend = true;
                            product.joined = true;
                            break;
                        }
                        product.catch_up_costs = [Some(started.elapsed()), product.catch_up_costs[0]];
                        last = hour;
                    }
                }
            }
        }
        let id = product.id;
        let pending = matches!(product.resume, Some(PanelResume::Pending(..)));
        work.put(product);
        let live = !self.token.load(Ordering::Acquire);
        if live && let Some((subscribe, ready)) = preparation {
            let wake = self.wake;
            subscribe(Arc::new(move || {
                ready.store(true, Ordering::Release);
                wake();
            }));
            return;
        }
        let replaced = !live && self.reads.lock().unwrap_or_else(PoisonError::into_inner).tokens.contains_key(&id);
        if (live && !pending) || replaced {
            (self.wake)();
        }
    }

    fn seed(&self, product: &mut StepProduct, hour: Hour, current_head: impl Fn() -> Option<(u64, Hour)>) {
        product.started_on = Some((self.epoch, self.stamp.2));
        product.joined = false;
        product.resume = None;
        product.pending_seed = None;
        if !self.read_one(product, hour, true, true, false, &current_head) {
            product.pending_seed = Some((self.epoch, self.stamp.2, hour));
        }
    }

    fn prepare_resume(&self, product: &mut StepProduct, hour: Hour, clock: &impl Fn() -> Option<PanelClock>) -> Option<(panel::OnPrepared, Arc<AtomicBool>)> {
        product.resume = None;
        if self.token.load(Ordering::Acquire) || !clock().is_some_and(|state| state.epoch == self.epoch && (state.playing.is_some() || state.head == hour)) { return None; }
        let next = product.last.map_or(hour, |(_, _, last)| last.saturating_add(1));
        let resend = product.resend;
        let Some((change, from, fresh)) = product.read_at(self.stamp.2, self.epoch, hour, false, false, true, &self.token, None, &self.reads) else {
            product.resend = resend;
            let subscribe = product.pending_preparation.as_ref().and_then(|pending| pending(hour));
            let ready = subscribe.as_ref().map(|_| Arc::new(AtomicBool::new(false)));
            product.resume = Some(PanelResume::Pending(hour, ready.clone()));
            return subscribe.zip(ready);
        };
        let unproven = from > next || (hour > next && fresh);
        let (kind, body) = change.unwrap_or((if unproven { panel::dated::AGAIN } else { panel::dated::STANDS }, &[]));
        product.resume = Some(PanelResume::Ready(hour, kind, body.to_vec()));
        None
    }

    fn read_one(&self, product: &mut StepProduct, hour: Hour, reset: bool, seed_paused: bool, catch_up: bool, current_head: &impl Fn() -> Option<(u64, Hour)>) -> bool {
        let seek = self.stamp.2;
        let id = product.id;
        let live_epoch = || {
            if self.token.load(Ordering::Acquire) { return None; }
            let (epoch, head) = current_head()?;
            (epoch == self.epoch || (seed_paused && epoch > self.epoch && head == hour)).then_some(epoch)
        };
        if live_epoch().is_none() { return false; }
        let unread_from = product.last.filter(|&(e, g, _)| (e, g) == (self.epoch, seek)).map(|(_, _, last)| last.saturating_add(1));
        let Some((change, from, fresh)) = product.read_at(seek, self.epoch, hour, reset, seed_paused, catch_up, &self.token, None, &self.reads) else { return false };
        let unproven = unread_from.is_some_and(|next| from > next || (hour > next && fresh));
        let (kind, body) = change.unwrap_or(if unproven { (panel::dated::AGAIN, &[]) } else { (panel::dated::STANDS, &[]) });
        let packet = panel::dated::at_one_hour(self.stamp, id, hour, kind, body);
        let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(epoch) = live_epoch().filter(|_| reads.current(self.epoch, seek)) else { return false };
        if seed_paused && epoch != self.epoch {
            product.started_on = Some((epoch, seek));
        }
        product.last = Some((epoch, seek, hour));
        reads.proven.remove(&id);
        self.push.offer_from(self.session, PANEL, packet);
        product.wire.commit();
        true
    }
}

impl StepProduct {
    fn resumed_step(&mut self, through: Hour) -> (Hour, Option<(Hour, u8, Vec<u8>)>, Option<Hour>) {
        let (hour, kind, body) = match self.resume.take().expect("a resuming product") {
            pending @ (PanelResume::Read(_) | PanelResume::Pending(..)) => {
                let next = pending.hour();
                self.resume = Some(pending);
                return (next, None, None);
            }
            PanelResume::Ready(hour, kind, body) => (hour, kind, body),
        };
        if hour > through {
            self.resume = Some(PanelResume::Ready(hour, kind, body));
            return (hour, None, None);
        }
        self.resume = (hour < through).then_some(PanelResume::Read(through));
        let next = if hour < through { through } else { hour.saturating_add(1) };
        (next, Some((hour, kind, body)), Some(hour))
    }

    fn step(&mut self, input: panel::ProductStep<'_>, (clock, route, seek): (i32, u32, u32), session: u64, token: &AtomicBool,
        active: &Mutex<ProductReads>, push: &Push, playhead: &Playhead) -> (Hour, Option<(Hour, u8, Vec<u8>)>, Option<Hour>) {
        let step = input.step;
        let (epoch, hour, id) = (step.epoch(), step.last(), self.id);
        let current = || !token.load(Ordering::Acquire) && playhead.epoch() == epoch;
        if step.reset() { self.pending_seed = None; }
        if !self.joined && let Some((e, g, last)) = self.last && (e, g) == (epoch, seek) && hour < last {
            return (last, None, None);
        }
        if !self.joined && self.last == Some((epoch, seek, hour)) {
            return (hour.saturating_add(1), Some((hour, panel::dated::STANDS, Vec::new())), Some(hour));
        }
        if let Some((e, g, at)) = self.pending_seed {
            if (e, g) == (epoch, seek) && at < hour && self.last.is_none_or(|(le, lg, _)| (le, lg) != (e, g)) {
                let head = playhead.head(Instant::now()).clamp(at, hour);
                if head < hour {
                    let Some((change, ..)) = self.read_at(seek, epoch, head, true, true, false, token, Some(input), active) else {
                        self.resend = true;
                        return (hour, Some((hour, panel::dated::WAITING, Vec::new())), None);
                    };
                    let guard = active.lock().unwrap_or_else(PoisonError::into_inner);
                    if guard.current(epoch, seek) && current() {
                        if let Some((kind, body)) = change {
                            push.offer_from(session, PANEL, panel::dated::encoded(clock, route, seek, id, head, kind, body));
                        }
                        self.last = Some((epoch, seek, head));
                        self.wire.commit();
                    }
                }
            }
            self.pending_seed = None;
        }
        let joining = !self.joined;
        let unread_from = self.last.filter(|&(e, g, _)| (e, g) == (epoch, seek)).map(|(_, _, last)| last.saturating_add(1));
        let Some((change, from, _)) = self.read_at(seek, epoch, hour, step.reset(), step.reset(), false, token, Some(input), active) else {
            if self.last.is_none_or(|(e, g, _)| (e, g) != (epoch, seek)) && self.pending_seed.is_none() {
                self.pending_seed = Some((epoch, seek, hour));
                self.joined = false;
            }
            self.resend = true;
            return (hour, Some((hour, panel::dated::WAITING, Vec::new())), None);
        };
        let change = change.map(|(kind, body)| (kind, body.to_vec()));
        let start = if self.followed.is_some() { hour } else { unread_from.map_or(from, |next| from.max(next)).min(hour) };
        let change = match change {
            Some((kind, body)) => Some((start, kind, body)),
            None if unread_from.is_some_and(|next| from > next) => Some((start, panel::dated::AGAIN, Vec::new())),
            None => joining.then(|| (hour, panel::dated::STANDS, Vec::new())),
        };
        (hour.saturating_add(1), change, Some(hour))
    }

    fn read_at(&mut self, page_seek: u32, epoch: u64, hour: Hour, reset: bool, seed_paused: bool, catch_up: bool, cancelled: &AtomicBool, step: Option<panel::ProductStep<'_>>, bindings: &Mutex<ProductReads>) -> Option<(Option<(u8, &[u8])>, Hour, bool)> {
        let seed = reset || self.last.is_none_or(|(e, g, h)| e != epoch || g != page_seek || h > hour);
        let send = seed || std::mem::take(&mut self.resend);
        let want_values = send || (catch_up && self.followed.is_some());
        let held = self.host.as_ref().map(|host| host.held.clone());
        let step = step.map(|step| panel::ProductStep { held: held.as_deref(), ..step });
        let reader = Arc::clone(&self.read);
        let run = |values, step| catch_unwind(AssertUnwindSafe(|| reader(hour, values, cancelled, step)))
            .unwrap_or_else(|p| panel::ProductRead { value: Arc::new(Err(format!("Panel read at hour {hour} failed: {}", panicked(&*p)))),
                rows: 0, fresh: true, pending: false, live: None, lanes: Vec::new(), from: hour });
        let mut read = run(want_values, step);
        if catch_up && !want_values && !read.fresh && !read.pending && read.live.as_ref().is_some_and(|live| !live.is_empty()) {
            read = run(true, None);
        }
        let mut active = bindings.lock().unwrap_or_else(PoisonError::into_inner);
        if cancelled.load(Ordering::Acquire) || !active.current(epoch, page_seek) { return None; }
        if !read.lanes.is_empty() { retain_lanes(self.id, &read.lanes, self.host.as_ref()); }
        if read.pending { return None; }
        self.cells = active.cells.remove(&self.id).unwrap_or_default();
        drop(active);
        let (value, cell_lanes) = self.follow_cells(&read.value, read.live.as_deref(), hour, send, catch_up, read.fresh, seed_paused);
        let mut current = bindings.lock().unwrap_or_else(PoisonError::into_inner);
        if cancelled.load(Ordering::Acquire) || !current.current(epoch, page_seek) {
            drop(current);
            release(&mut self.cells, self.host.as_ref());
            return None;
        }
        if !cell_lanes.is_empty() { retain_lanes(self.id, &cell_lanes, self.host.as_ref()); }
        if !self.cells.is_empty() { current.cells.insert(self.id, std::mem::take(&mut self.cells)); }
        drop(current);
        Some((self.wire.update(send, &value), read.from.min(hour), read.fresh))
    }

    fn follow_cells(&mut self, value: &Arc<PanelData>, live: Option<&Live>, hour: Hour, seed: bool, catch_up: bool, fresh: bool, seed_paused: bool) -> (Arc<PanelData>, Vec<Declared>) {
        let (Ok(reply @ Value::Object(_)), Some(live)) = (&**value, live.filter(|live| !live.is_empty())) else {
            release(&mut self.cells, self.host.as_ref());
            self.followed = None;
            self.input = None;
            return (value.clone(), Vec::new());
        };
        if !seed && !catch_up && let (Some(input), Some(followed)) = (&self.input, &self.followed) && Arc::ptr_eq(input, value) { return (followed.clone(), Vec::new()); }
        self.input = Some(value.clone());
        let mut reply = reply.clone();
        let mut kept = Vec::with_capacity(live.groups.len());
        let mut cell_lanes = Vec::new();
        let mut inline_wide = false;
        if let Some(groups) = reply.get_mut("live").and_then(|live| live.get_mut("groups")).and_then(Value::as_array_mut) {
            for (group, out) in live.groups.iter().zip(groups) {
                let Some(out) = out.as_object_mut() else { continue };
                let new = !self.cells.iter().any(|(held, _)| held == group);
                let exact = out.get("values").and_then(Value::as_array).is_some_and(|values| values.iter().all(|value| value.as_i64().is_none_or(|n| n.unsigned_abs() < 1 << 53)));
                if catch_up && fresh && !exact && out.get("values").and_then(Value::as_array).is_some() {
                    inline_wide = true;
                    cell_lanes.push(Declared { table: group.table.index(), columns: group.columns.clone(), keyed: false, passing: false, hourly: false });
                    continue;
                }
                let seeded = (new || seed || catch_up) && fresh && exact;
                match register(group, &mut self.cells, &mut kept, self.host.as_ref(), self.class, seed_paused && !seeded) {
                    Ok(id) => {
                        cell_lanes.push(Declared { table: group.table.index(), columns: group.columns.clone(), keyed: false, passing: true, hourly: false });
                        out.insert("follow".into(), id.into());
                        if seeded { out.insert("seedHour".into(), hour.into()); } else { out.remove("values"); out.remove("rawValues"); }
                    }
                    Err(reason) => {
                        out.remove("values");
                        out.remove("rawValues");
                        out.insert("unavailable".into(), reason.into());
                    }
                }
            }
        }
        release(&mut self.cells, self.host.as_ref());
        self.cells = kept;
        if inline_wide {
            self.input = None;
        }
        let made = Arc::new(Ok(reply));
        let out = match &self.followed {
            Some(last) if **last == *made => last.clone(),
            _ => { self.followed = Some(made.clone()); made }
        };
        (out, cell_lanes)
    }
}

impl Panels {
    fn remove(&mut self, id: u32) {
        if self.products.remove(&id).is_some() {
            self.due.retain(|&(_, due)| due != id);
        }
        self.shown.remove(&id);
    }
}

fn panicked(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic with no message".to_owned())
}

fn evaluate(id: u32, shown: &mut Shown, stamp: (i32, u32, u32), epoch: u64, version: u64, token: &AtomicBool,
    active: &Mutex<ProductReads>, push: &Push, session: u64, current_head: impl Fn() -> Option<(u64, Hour)>) {
    use panel::dated::{self, Update};
    let (clock, route, seek) = stamp;
    let live_epoch = |hour: Hour| {
        if token.load(Ordering::Acquire) { return None; }
        let (now_epoch, head) = current_head()?;
        (now_epoch == epoch || (now_epoch > epoch && head == hour)).then_some(now_epoch)
    };
    if shown.version != (version, epoch) {
        if shown.version.0 != version || shown.pushed.is_none_or(|pushed| current_head().is_none_or(|(_, head)| head != pushed)) {
            shown.frame = None;
            shown.pushed = None;
        }
        shown.version = (version, epoch);
    }
    if let Some((hour, reason)) = shown.reader.stopped() {
        if shown.stopped.as_ref().is_none_or(|(held, at)| *held != reason || *at != epoch) {
            let packet = dated::message(clock, route, seek, &[Update::Stopped(id, hour, &reason)]);
            let mut reads = active.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(now_epoch) = live_epoch(hour).filter(|_| reads.seek == seek) else { return };
            if let Some(mut cells) = reads.cells.remove(&id) { release(&mut cells, reads.host.as_ref()); }
            push.offer_from(session, PANEL, packet);
            (shown.stopped, shown.frame, shown.pushed) = (Some((reason, epoch)), None, None);
            shown.version = (version, now_epoch);
        }
        return;
    }
    shown.stopped = None;
    let Some(frame) = shown.reader.frame() else { return };
    let Some((through, _)) = shown.reader.through_seek(&frame).filter(|&(_, at)| at == epoch) else { return };
    let mut data = None;
    let same_frame = shown.frame.as_ref().is_some_and(|last| {
        Arc::ptr_eq(last, &frame)
            || (last.branch == frame.branch
                && last.hour == frame.hour
                && last.tables.iter().zip(&frame.tables).all(|(a, b)| {
                    if a.is_not_declared() || b.is_not_declared() {
                        a.is_not_declared() && b.is_not_declared()
                    } else {
                        (a.as_ptr() == b.as_ptr() || a == b)
                            && a.sampled_at() == b.sampled_at()
                            && a.fields() == b.fields()
                            && a.keys() == b.keys()
                    }
                }))
    });
    if same_frame {
        shown.frame = Some(frame.clone());
    } else {
        let made = catch_unwind(AssertUnwindSafe(|| shown.panel.data(&frame)))
            .unwrap_or_else(|p| Err(format!("This panel could not be made at hour {} ({})", frame.hour, panicked(&*p))));
        let (mut bound, host) = {
            let mut reads = active.lock().unwrap_or_else(PoisonError::into_inner);
            if live_epoch(frame.hour).is_none() || reads.seek != seek { return; }
            (reads.cells.remove(&id).unwrap_or_default(), reads.host.clone())
        };
        let (made_cells, cell_lanes) = with_cells(&frame, &made, shown.panel.live(), &mut bound, host.as_ref(), shown.class);
        let mut reads = active.lock().unwrap_or_else(PoisonError::into_inner);
        if live_epoch(frame.hour).is_none() || reads.seek != seek {
            drop(reads);
            release(&mut bound, host.as_ref());
            return;
        }
        if !cell_lanes.is_empty() { retain_lanes(id, &cell_lanes, host.as_ref()); }
        if !bound.is_empty() { reads.cells.insert(id, bound); }
        drop(reads);
        data = Some(made_cells);
    }
    let pushed = if data.is_some() { Some(frame.hour) } else { shown.pushed };
    let want_stands = pushed.is_some_and(|last| through > last);
    let data_packet = data.as_ref().map(|data| dated::message(clock, route, seek, &[Update::Data(id, frame.hour, data)]));
    let stands_packet = (want_stands && live_epoch(frame.hour) == Some(epoch)).then(|| match data.as_ref() {
        Some(data) => dated::message(clock, route, seek, &[Update::Data(id, frame.hour, data), Update::Stands(id, through)]),
        None => dated::message(clock, route, seek, &[Update::Stands(id, through)]),
    });
    if data_packet.is_some() || stands_packet.is_some() {
        let reads = active.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(now_epoch) = live_epoch(frame.hour).filter(|_| reads.seek == seek) {
            let (packet, next_pushed) = if now_epoch == epoch && let Some(packet) = stands_packet {
                (Some(packet), Some(through))
            } else {
                (data_packet, pushed)
            };
            if let Some(packet) = packet {
                push.offer_from(session, PANEL, packet);
                if data.is_some() {
                    shown.frame = Some(frame.clone());
                }
                shown.pushed = next_pushed;
                shown.version = (version, now_epoch);
            }
        }
    }
}

pub struct PlayheadHost<E: Send + Sync + 'static> {
    owner: Arc<E>,
    panels: Arc<Mutex<Panels>>,
    reads: Arc<Mutex<ProductReads>>,
    get: fn(&E) -> &Playhead,
    anchored: fn(&E, Anchor),
    follow: Follow,
    reader: Reader,
    held: Vec<bool>,
    parking: Arc<PanelParking>,
    stop: Arc<AtomicBool>,
    driver_gate: Arc<WaitGate>,
    panel_gate: Arc<WaitGate>,
    panel_work: Arc<AtomicU64>,
    _driver_watch: Watcher,
    _panel_watch: Watcher,
    driver: Mutex<Option<JoinHandle<()>>>,
}

impl<E: Send + Sync + 'static> PlayheadHost<E> {
    pub fn new(owner: Arc<E>, get: fn(&E) -> &Playhead, anchored: fn(&E, Anchor), follow: Follow, held: Vec<bool>) -> std::io::Result<Arc<Self>> {
        Self::new_with_budget(owner, get, anchored, follow, held, crate::lead::budget())
    }

    pub fn new_with_budget(owner: Arc<E>, get: fn(&E) -> &Playhead, anchored: fn(&E, Anchor), follow: Follow, held: Vec<bool>, budget: Arc<playhead::ByteBudget>) -> std::io::Result<Arc<Self>> {
        let playhead = get(&owner);
        let reader = playhead.declare(Vec::new(), None)?;
        let mut consumer = playhead.consumer(reader);
        let bound = Arc::downgrade(&owner);
        follow.bind(move |tables, joining| {
            let Some(owner) = bound.upgrade() else { return Ok(()) };
            let playhead = get(&owner);
            playhead.redeclare_seeding(reader, tables, joining)
        }, playhead.log_reads())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (driven, stopping) = (Arc::clone(&owner), Arc::clone(&stop));
        let driver_gate = Arc::new(WaitGate::new());
        let parking = Arc::new(PanelParking { wake: {
            let gate = driver_gate.clone();
            Arc::new(move || gate.wake())
        }, jobs: Mutex::default() });
        let park = Arc::downgrade(&parking);
        let panels = Arc::new(Mutex::new(Panels { stamp: (follow.clock(), follow.route(), playhead.epoch() as u32, 0), cells: Some(HostCells { follow: follow.clone(), held: held.clone() }), ..Panels::default() }));
        let weak = Arc::downgrade(&panels);
        let reads = Arc::new(Mutex::new(ProductReads { host: Some(HostCells { follow: follow.clone(), held: held.clone() }), ..Default::default() }));
        let active = Arc::downgrade(&reads);
        let product_owner = Arc::downgrade(&owner);
        let wake = follow.rehooker();
        follow.product_hook(Arc::new(move |step, lanes| {
            let (Some(panels), Some(owner), Some(active)) = (weak.upgrade(), product_owner.upgrade(), active.upgrade()) else { return };
            let epoch = step.epoch();
            let (stamp, session, push, due, resend) = {
                let mut panels = panels.lock().unwrap_or_else(PoisonError::into_inner);
                if panels.products.is_empty() || get(&owner).epoch() != epoch { return; }
                let Some(push) = panels.push.clone() else { return };
                let active = active.lock().unwrap_or_else(PoisonError::into_inner);
                let (clock, route, _, _) = panels.stamp;
                let stream = (epoch, active.seek, step.restep(), step.piece());
                let changed = panels.stream.replace(stream) != Some(stream) || step.reset();
                let resend = !changed && panels.resend.is_some_and(|reset| step.last() > reset);
                if changed || resend {
                    panels.resend = changed.then_some(step.last());
                    let Panels { due, products, .. } = &mut *panels;
                    due.clear();
                    due.extend(products.keys().map(|&id| (step.last(), id)));
                }
                let mut due = Vec::new();
                while let Some(id) = panel::product::take_due(&mut panels.due, step.last()) {
                    if let (Some(work), Some(token)) = (panels.products.get(&id), active.tokens.get(&id)) {
                        due.push((id, work.clone(), token.clone()));
                    }
                }
                ((clock, route, active.seek), panels.session, push, due, resend)
            };
            let mut replies = Vec::with_capacity(due.len());
            let task = |token| PanelJob { stamp, session, epoch, token, reads: active.clone(), push: push.clone(), wake: wake.clone(), parking: park.clone() };
            for (id, work, token) in due {
                if token.load(Ordering::Acquire) || get(&owner).epoch() != epoch { continue; }
                let Some(mut product) = work.take() else {
                    replies.push((id, None, token, step.last(), None, None));
                    continue;
                };
                let playhead = get(&owner);
                let quantum = playhead.step_hours();
                let seed_hour = playhead.head(Instant::now()).min(step.last());
                if product.started_on != Some((epoch, stamp.2)) && product.last.is_some_and(|(_, g, h)| g == stamp.2 && h == seed_hour) {
                    product.started_on = Some((epoch, stamp.2));
                    product.last = Some((epoch, stamp.2, seed_hour));
                    product.joined = false;
                    product.resume = None;
                }
                if product.resume.as_ref().is_some_and(|resume| step.reset() || {
                    let (_, head, speed) = playhead.clock_state_at(Instant::now());
                    speed.is_none() && resume.hour() != head
                }) {
                    product.resume = None;
                    product.resend = true;
                }
                let job = if product.started_on != Some((epoch, stamp.2)) {
                    Some(PanelRead::Seed(seed_hour))
                } else if let Some(hour) = product.resume.as_ref().and_then(|resume| resume.due_read(step.last())) {
                    Some(PanelRead::Resume(hour))
                } else if !product.joined && product.resume.is_none() && !step.reset() && product.last.is_some_and(|(e, g, h)| (e, g) == (epoch, stamp.2) && h.saturating_add(quantum) < step.first()) {
                    Some(PanelRead::CatchUp { through: step.last(), quantum, parked: false })
                } else {
                    None
                };
                if let Some(job) = job {
                    if matches!(job, PanelRead::CatchUp { .. }) && seed_hour < step.last() && playhead.paused_seed_pending() {
                        work.put(product);
                    } else {
                        task(token.clone()).start(job, product, work, &owner, get);
                    }
                    replies.push((id, None, token, step.last(), None, None));
                    continue;
                }
                product.resend |= resend;
                let input = panel::ProductStep { step, lanes, held: None, wake: Some(&wake) };
                let (next, change, stepped) = if product.resume.is_some() {
                    product.resumed_step(step.last())
                } else {
                    product.step(input, stamp, session, &token, &active, &push, get(&owner))
                };
                replies.push((id, Some((work, product)), token, next, change, stepped));
            }
            let mut held_products = Vec::with_capacity(replies.len());
            {
                let mut panels = panels.lock().unwrap_or_else(PoisonError::into_inner);
                let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                let ok = active.current(epoch, stamp.2) && get(&owner).epoch() == epoch;
                if ok {
                    let mut batch = panel::dated::ProductBatch::new(stamp);
                    for (id, held, token, next, change, stepped) in replies {
                        let live = !token.load(Ordering::Acquire);
                        let waiting = held.as_ref().is_none_or(|(_, product)| product.resume.is_some());
                        if let Some((work, mut product)) = held {
                            if live && let Some(hour) = stepped {
                                product.last = Some((epoch, stamp.2, hour));
                                product.wire.commit();
                                product.joined = product.resume.is_none();
                                active.proven.insert(id, (stamp.2, hour));
                            }
                            let owed = (live && stepped.is_some() && matches!(product.resume, Some(PanelResume::Read(hour)) if hour == step.last()) && get(&owner).playing())
                                .then(|| task(token.clone()));
                            held_products.push((work, product, owed));
                        }
                        if !live { continue; }
                        panels.due.insert((next, id));
                        if let Some((at, kind, body)) = change { batch.changed(id, at, kind, &body); }
                        if waiting && let Some(from) = unread_from(active.proven.get(&id), stamp.2, step.last()) {
                            batch.changed(id, from, panel::dated::WAITING, &[]);
                        }
                    }
                    push.offer_from(session, PANEL, batch.finish(Some(step.last())));
                } else {
                    for (_, held, ..) in replies {
                        if let Some((work, mut product)) = held {
                            product.resend = true;
                            held_products.push((work, product, None));
                        }
                    }
                }
            }
            for (work, product, owed) in held_products {
                if let Some(task) = owed {
                    task.start(PanelRead::Resume(step.last()), product, work, &owner, get);
                } else {
                    work.put(product);
                }
            }
        }));
        playhead.share_budget(budget)?;
        playhead.lead(crate::lead::LEAD);
        let panel_gate = Arc::new(WaitGate::new());
        let panel_work = Arc::new(AtomicU64::new(0));
        follow.set_wake({
            let driver_gate = Arc::clone(&driver_gate);
            Arc::new(move || driver_gate.wake())
        });
        let driver_watch = playhead.watch({
            let driver_gate = Arc::clone(&driver_gate);
            Arc::new(move || driver_gate.wake())
        });
        let panel_watch = playhead.watch_control({
            let panel_gate = Arc::clone(&panel_gate);
            let panel_work = Arc::clone(&panel_work);
            Arc::new(move || {
                panel_work.fetch_add(1, Ordering::Release);
                panel_gate.wake();
            })
        });
        let (waiting, gate, follow_ref, parked) = (panels.clone(), Arc::clone(&driver_gate), follow.clone(), parking.clone());
        let driver = std::thread::Builder::new().name("follow driver".into()).spawn(move || {
            let mut taken = Vec::new();
            let mut next = Instant::now();
            while !stopping.load(Ordering::Acquire) {
                let seen = gate.stamp();
                let playhead = get(&driven);
                let (has_work, told) = {
                    let held = waiting.lock().unwrap_or_else(PoisonError::into_inner);
                    (!held.products.is_empty() || !held.shown.is_empty() || follow_ref.has_work(), held.push.clone().map(|push| (push, held.stamp.1, held.stamp.3)))
                };
                let (epoch, head, speed) = playhead.clock_state_at(Instant::now());
                let playing = speed.is_some();
                if let Some((push, route, seek)) = told {
                    push.head(route, seek, head);
                    let held_at = (!playing && push.reply_lead_full() == Ok(true)).then_some(head);
                    for task in parked.ready(epoch, held_at) { run_panel(task); }
                }
                let release = if playing { head.saturating_add(playhead.step_hours().saturating_mul(crate::lead::RELEASE_FRAMES)) } else { head };
                let advance_ok = consumer.try_advance(release);
                let take_ok = consumer.try_take(release, &mut taken).is_some();
                taken.clear();
                let contended = !advance_ok || !take_ok;
                let timeout = if contended {
                    Some(Duration::from_millis(2))
                } else if playing && has_work {
                    let now = Instant::now();
                    if next <= now {
                        next = now + FRAME;
                    }
                    Some(next.saturating_duration_since(now))
                } else {
                    next = Instant::now();
                    None
                };
                if gate.wait(seen, timeout) {
                    return;
                }
            }
        })?;

        Ok(Arc::new(Self { owner, panels, reads, get, anchored, follow, reader, held, parking, stop, driver_gate, panel_gate, panel_work, _driver_watch: driver_watch, _panel_watch: panel_watch, driver: Mutex::new(Some(driver)) }))
    }

    fn install_panel(&self, id: u32, panel: Box<dyn Panel>, lanes: u32, reader: Option<Box<dyn PanelReader>>) -> Result<(), String> {
        let class = (id % lanes.max(1), lanes.max(1));
        if let Some(exact) = panel.exact() {
            let mut panels = self.panels.lock().unwrap_or_else(PoisonError::into_inner);
            let push = panels.push.clone().ok_or("This playhead has no panel transport")?;
            let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
            reads.register(id);
            panels.remove(id);
            let epoch = self.playhead().epoch();
            let hour = self.playhead().head(Instant::now());
            let host = panels.cells.clone();
            let product = StepProduct { id, pending_seed: None, started_on: None, read: exact.advance.clone(), pending_preparation: panel.pending_preparation().cloned(), wire: Default::default(), last: None, catch_up_costs: Default::default(), joined: false, resume: None, cells: Vec::new(), class, host, followed: None, input: None, resend: false };
            let task = PanelJob { stamp: (panels.stamp.0, panels.stamp.1, reads.seek), session: panels.session, epoch, token: reads.tokens[&id].clone(), reads: self.reads.clone(), push, wake: self.follow.rehooker(), parking: Weak::new() };
            let work = Arc::new(Work::new(None));
            panels.due.insert((Hour::MIN, id));
            panels.products.insert(id, work.clone());
            drop(reads);
            drop(panels);
            task.start(PanelRead::Seed(hour), product, work, &self.owner, self.get);
            self.driver_gate.wake();
            return Ok(());
        }
        let reader = reader.expect("a prepared row reader");
        {
            let mut panels = self.panels.lock().unwrap_or_else(PoisonError::into_inner);
            self.reads.lock().unwrap_or_else(PoisonError::into_inner).register(id);
            panels.remove(id);
            panels.shown.insert(id, Arc::new(Work::new(Some(Shown::new(panel, reader, class)))));
            if panels.thread.is_none() {
                let (held, stopping, gate, work_seq) = (Arc::downgrade(&self.panels), Arc::clone(&self.stop), Arc::clone(&self.panel_gate), Arc::clone(&self.panel_work));
                let (owner, get, tokens) = (self.owner.clone(), self.get, self.reads.clone());
                let thread = std::thread::Builder::new()
                    .name("follow panels".into())
                    .spawn(move || {
                        let mut next: Option<Instant> = None;
                        let mut seen_work = u64::MAX;
                        while !stopping.load(Ordering::Acquire) {
                            let seen = gate.stamp();
                            let work = work_seq.load(Ordering::Acquire);
                            let now = Instant::now();
                            let Some(panels) = held.upgrade() else { return };
                            let playhead = get(&owner);
                            let playing = playhead.playing();
                            let poll = (playhead.frame_period() / 2).clamp(Duration::from_millis(4), PANEL_POLL);
                            let due = work != seen_work || next.is_some_and(|deadline| now >= deadline);
                            let active = if due {
                                let (jobs, stamp, epoch, version, session, push) = {
                                    let mut panels = panels.lock().unwrap_or_else(PoisonError::into_inner);
                                    let epoch = playhead.epoch();
                                    panels.stamp.2 = epoch as u32;
                                    let reads = tokens.lock().unwrap_or_else(PoisonError::into_inner);
                                    let jobs = panels.shown.iter().filter_map(|(&id, work)| Some((id, work.clone(), reads.tokens.get(&id)?.clone()))).collect::<Vec<_>>();
                                    (jobs, (panels.stamp.0, panels.stamp.1, panels.stamp.3), epoch, panels.frame_version, panels.session, panels.push.clone())
                                };
                                let active = !jobs.is_empty() && playing;
                                if let Some(push) = push {
                                    for (id, work, token) in jobs {
                                        let Some(mut shown) = work.take() else { continue };
                                        let (push, tokens, gate, sequence, owner) = (push.clone(), tokens.clone(), gate.clone(), work_seq.clone(), Arc::downgrade(&owner));
                                        run_panel(move || {
                                            if !token.load(Ordering::Acquire) {
                                                evaluate(id, &mut shown, stamp, epoch, version, &token, &tokens, &push, session, || owner.upgrade().map(|owner| {
                                                    let playhead = get(&owner);
                                                    (playhead.epoch(), playhead.head(Instant::now()))
                                                }));
                                            }
                                            if work.put(shown) {
                                                sequence.fetch_add(1, Ordering::Release);
                                                gate.wake();
                                            }
                                        });
                                    }
                                }
                                seen_work = work;
                                active
                            } else {
                                !panels.lock().unwrap_or_else(PoisonError::into_inner).shown.is_empty() && playing
                            };
                            drop(panels);
                            let timeout = if active {
                                let deadline = match next {
                                    Some(deadline) if deadline > now => deadline,
                                    _ => {
                                        let deadline = now + poll;
                                        next = Some(deadline);
                                        deadline
                                    }
                                };
                                Some(deadline.saturating_duration_since(Instant::now()))
                            } else {
                                next = None;
                                None
                            };
                            if gate.wait(seen, timeout) {
                                return;
                            }
                        }
                    })
                    .map_err(|error| error.to_string())?;
                panels.thread = Some(thread);
            }
        }
        self.driver_gate.wake();
        self.wake_panels();
        Ok(())
    }

    pub fn playhead(&self) -> &Playhead {
        (self.get)(&self.owner)
    }

    #[cfg(test)]
    pub(crate) fn hold_product(&self, id: u32) -> Option<impl FnOnce()> {
        let work = self.panels.lock().unwrap_or_else(PoisonError::into_inner).products.get(&id)?.clone();
        let product = work.take()?;
        Some(move || { work.put(product); })
    }

    fn wake_panels(&self) {
        self.panel_work.fetch_add(1, Ordering::Release);
        self.panel_gate.wake();
    }

    pub fn show_panels(&self, rows: Arc<dyn PanelRows>, push: Arc<Push>) {
        push.watch_reply_lead(&self.parking.wake);
        rows.on_publish({
            let panel_gate = Arc::clone(&self.panel_gate);
            let panel_work = Arc::clone(&self.panel_work);
            Arc::new(move || {
                panel_work.fetch_add(1, Ordering::Release);
                panel_gate.wake();
            })
        });
        {
            let mut panels = self.panels.lock().unwrap_or_else(PoisonError::into_inner);
            panels.rows = Some(rows);
            panels.push = Some(push);
        }
        self.driver_gate.wake();
        self.wake_panels();
    }
}

impl<E: Send + Sync + 'static> Hosted for PlayheadHost<E> {
    fn follow(&self) -> &Follow {
        &self.follow
    }

    fn lacks(&self, table: usize) -> Option<String> {
        lacking(table, self.held.get(table).copied().unwrap_or(false))
    }

    fn prepare_panel(&self, panel: Box<dyn Panel>) -> Result<crate::registry::PreparedPanel<'_>, String> {
        let reader = if panel.exact().is_some() {
            None
        } else {
            let rows = self.panels.lock().unwrap_or_else(PoisonError::into_inner).rows.clone().ok_or("This playhead's host gives panels no rows at the head")?;
            Some(rows.reader(panel.tables()).map_err(|error| format!("Rows at the head are refused: {error}"))?)
        };
        Ok(Box::new(move |id, lanes| self.install_panel(id, panel, lanes, reader)))
    }

    fn serve(&self, session: u64) {
        self.follow.serve(session);
        self.panels.lock().unwrap_or_else(PoisonError::into_inner).session = session;
        self.driver_gate.wake();
        self.wake_panels();
    }

    fn unpanel(&self, id: u32) {
        self.reads.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        self.panels.lock().unwrap_or_else(PoisonError::into_inner).remove(id);
        self.driver_gate.wake();
        self.wake_panels();
    }

    fn anchor(&self, anchor: Anchor) {
        let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
        let anchor = Anchor { sought: anchor.sought || anchor.seek != reads.seek, ..anchor };
        self.follow.anchor(anchor.seek, || {
            (self.anchored)(&self.owner, anchor);
            self.playhead().epoch()
        });
        let epoch = self.playhead().epoch();
        reads.anchor(epoch, anchor.seek);
        drop(reads);
        {
            let mut panels = self.panels.lock().unwrap_or_else(PoisonError::into_inner);
            let epoch = epoch as u32;
            let previous = panels.pace.replace((anchor.playing, anchor.speed));
            panels.stamp.2 = epoch;
            let different_seek = panels.stamp.3 != anchor.seek;
            panels.stamp.3 = anchor.seek;
            if different_seek || anchor.sought || previous.is_some_and(|(playing, _)| playing && !anchor.playing) {
                panels.frame_version = panels.frame_version.wrapping_add(1);
                let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
                for id in panels.shown.keys() {
                    if let Some(token) = reads.tokens.get_mut(id) { std::mem::take(token).store(true, Ordering::Release); }
                }
            }
        }
        self.driver_gate.wake();
        self.wake_panels();
    }
}

impl<E: Send + Sync + 'static> Drop for PlayheadHost<E> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.driver_gate.stop();
        self.panel_gate.stop();
        for token in self.reads.lock().unwrap_or_else(PoisonError::into_inner).tokens.values() { token.store(true, Ordering::Release); }
        self.follow.retire();
        if let Some(driver) = self.driver.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = driver.join();
        }
        let thread = self.panels.lock().unwrap_or_else(PoisonError::into_inner).thread.take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
        self.playhead().retire(self.reader);
    }
}

#[cfg(test)]
#[test]
fn an_unread_product_waits_only_from_the_first_hour_it_has_not_proven() {
    assert_eq!(unread_from(Some(&(3, 8)), 3, 0), None, "a paused step at a proven hour leaves a busy product's panel as it is");
    assert_eq!(unread_from(Some(&(3, 8)), 3, 8), None);
    assert_eq!(unread_from(Some(&(3, 8)), 3, 14), Some(9), "a step past the proven hours waits from the first one unproven");
    assert_eq!(unread_from(Some(&(2, 8)), 3, 14), None, "a proof from another seek is not this panel's: its records closed or replaced it");
    assert_eq!(unread_from(None, 3, 14), None, "a panel never stepped is closed by its seed's own WAITING");
}

#[cfg(test)]
#[test]
fn a_retired_product_read_cannot_publish_its_seed() {
    let reads = Arc::new(Mutex::new(ProductReads::default()));
    reads.lock().unwrap().register(4);
    let token = reads.lock().unwrap().tokens[&4].clone();
    let (started, wait_started) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let wait_release = Mutex::new(wait_release);
    let mut product = StepProduct {
        id: 4, pending_seed: None, started_on: None, last: None, catch_up_costs: Default::default(), joined: false, resume: None, pending_preparation: None, wire: Default::default(), cells: Vec::new(), class: (0, 1), host: None, followed: None, input: None, resend: false,
        read: Arc::new(move |_, _, _, _| {
            started.send(()).unwrap();
            wait_release.lock().unwrap().recv().unwrap();
            panel::ProductRead { value: Arc::new(Ok(Value::Null)), rows: 0, fresh: true, pending: false, live: None, lanes: Vec::new(), from: Hour::MIN }
        }),
    };
    let push = Arc::new(Push::default());
    std::thread::scope(|threads| {
            let job = PanelJob { stamp: (0, 1, 0), session: 0, epoch: 1, token: token.clone(), reads: reads.clone(), push: push.clone(), wake: Arc::new(|| {}), parking: Weak::new() };
        threads.spawn(move || job.seed(&mut product, 12, || Some((1, 12))));
        wait_started.recv().unwrap();
        reads.lock().unwrap().anchor(2, 9);
        release.send(()).unwrap();
    });
    assert!(token.load(Ordering::Acquire));
    assert_eq!(push.counters().offered, 0);
    let mut current = reads.lock().unwrap();
    let new = current.tokens[&4].clone();
    current.remove(4);
    assert!(new.load(Ordering::Acquire));
}

#[cfg(test)]
#[test]
fn a_paused_panel_catches_up_to_the_head_without_computing_past_its_credit() {
    for release in ["play", "credit", "seek"] {
        check_parked_panel(release);
    }
}

#[cfg(test)]
#[test]
fn a_playing_panel_jumps_to_the_visible_head_when_it_is_behind() {
    check_parked_panel("playing");
}

#[cfg(test)]
#[test]
fn a_playing_panel_within_one_quantum_reads_each_current_boundary() {
    check_parked_panel("playing-near");
}

#[cfg(test)]
#[test]
fn playing_catch_up_targets_the_boundary_it_can_deliver() {
    let slow = Duration::from_millis(20);
    let fast = Duration::from_millis(10);
    let target = |last, newest, costs| playing_catch_up_hour(last, 6, 100, 1440.0, newest, costs);

    assert_eq!(target(82, 150, [None, None]), 100, "the first read aims at the head");
    assert_eq!(playing_catch_up_hour(82, 6, 101, 1440.0, 130, [None, None]), 106, "the first read lands on a boundary");
    assert_eq!(target(118, 150, [Some(fast), None]), 124, "an ahead panel reads the next quantum");
    assert_eq!(target(82, 150, [Some(fast), Some(slow)]), 130, "one fast read cannot lower the cost");
    assert_eq!(target(82, 150, [Some(slow), Some(fast)]), 130, "the slower of the last two determines the target");
    assert_eq!(target(82, 126, [Some(slow), None]), 126, "the newest request bounds the target");
}

#[cfg(test)]
fn check_parked_panel(release: &str) {
    let (head, epoch, route, seek) = (10, 1, 1, 1);
    let future = panel::dated::encoded(0, route, seek, 2, head + 1, panel::dated::DATA, &[1; 2048]);
    let push = Arc::new(Push::with_budget(playhead::ByteBudget::new(crate::answer::charge(&future) * 4)));
    let gate = Arc::new(WaitGate::new());
    let parking = Arc::new(PanelParking { wake: {
        let gate = gate.clone();
        Arc::new(move || gate.wake())
    }, jobs: Mutex::default() });
    push.watch_reply_lead(&parking.wake);
    push.head(route, seek, head + 90);
    push.offer_kind(PANEL, future);
    assert!(push.try_take().is_some());

    let reads = Arc::new(Mutex::new(ProductReads { epoch, seek, ..Default::default() }));
    reads.lock().unwrap().register(4);
    let token = reads.lock().unwrap().tokens[&4].clone();
    let sampled = Arc::new(Mutex::new(Vec::new()));
    let recorded = sampled.clone();
    let pending_at = (release == "preparation").then_some(head + 1);
    let prepared = Arc::new(AtomicBool::new(false));
    let ready = prepared.clone();
    let (subscribe, subscriber) = mpsc::channel::<panel::Wake>();
    let pending_preparation = pending_at.map(|hour| Arc::new(move |asked| {
        assert_eq!(asked, hour);
        let subscribe = subscribe.clone();
        Some(Box::new(move |wake| subscribe.send(wake).unwrap()) as panel::OnPrepared)
    }) as panel::PendingPreparation);
    let (last, quantum) = match release {
        "playing" => (head - 3, 1),
        "playing-near" => (head - 2, 6),
        _ => (head - 1, 1),
    };
    let product = StepProduct {
        id: 4, pending_seed: None, started_on: Some((epoch, seek)), last: Some((epoch, seek, last)), catch_up_costs: Default::default(), joined: false, resume: None, pending_preparation, wire: Default::default(), cells: Vec::new(), class: (0, 1), host: None, followed: None, input: None, resend: false,
        read: Arc::new(move |hour, _, _, _| {
            recorded.lock().unwrap().push(hour);
            let pending = pending_at == Some(hour) && !ready.load(Ordering::Acquire);
            panel::ProductRead { value: Arc::new(Ok(serde_json::json!(hour))), rows: 1, fresh: !pending, pending, live: None, lanes: Vec::new(), from: hour }
        }),
    };
    let wake_gate = Arc::new(WaitGate::new());
    let job = || PanelJob { stamp: (0, route, seek), session: push.session(), epoch, token: token.clone(), reads: reads.clone(), push: push.clone(), wake: {
        let wake_gate = wake_gate.clone();
        Arc::new(move || wake_gate.wake())
    }, parking: Arc::downgrade(&parking) };
    let work = Arc::new(Work::new(None));
    let now = Arc::new(AtomicU64::new(epoch));
    let playing = Arc::new(AtomicBool::new(release.starts_with("playing")));
    let clock = now.clone();
    let live = playing.clone();
    let through = match release {
        "play" => head + 4,
        "playing" => head,
        "playing-near" => last + 2 * quantum,
        _ => head + 1,
    };
    let latest = through + 24;
    let scheduled = if release == "playing-near" { 1 } else { quantum };
    job().run(PanelRead::CatchUp { through, quantum: scheduled, parked: false }, product, work.clone(), move || Some(PanelClock {
        epoch: clock.load(Ordering::Acquire), head, playing: live.load(Ordering::Acquire).then_some((0.0, quantum)), newest: Some(latest),
    }));
    if release == "playing" {
        assert_eq!(*sampled.lock().unwrap(), [head]);
        let (_, bytes, ..) = push.try_take().expect("the latest reading is sent");
        let records = panel::dated::records(&bytes).unwrap();
        assert_eq!((records[0].hour, records[0].kind), (head, panel::dated::DATA));
        assert_eq!(rmp_serde::from_slice::<Hour>(records[0].body).unwrap(), head);
        assert_eq!((records[1].hour, records[1].kind), (head + 1, panel::dated::WAITING));
        assert_eq!(work.take().unwrap().last, Some((epoch, seek, head)));
        return;
    }
    if release == "playing-near" {
        assert_eq!(*sampled.lock().unwrap(), [last + quantum, through]);
        assert_eq!(work.take().unwrap().last, Some((epoch, seek, through)));
        return;
    }
    assert_eq!(*sampled.lock().unwrap(), [head]);
    let (_, bytes, ..) = push.try_take().expect("the head answer fits while future replies hold credit");
    assert_eq!(panel::dated::records(&bytes).unwrap()[0].hour, head);
    assert!(push.try_take().is_none(), "the parked catch-up has offered nothing past the head");
    let held_at = || (!playing.load(Ordering::Acquire) && push.reply_lead_full() == Ok(true)).then_some(head);
    assert!(parking.ready(epoch, held_at()).is_empty());
    assert!(work.take().is_none());

    let mut next_epoch = epoch;
    match release {
        "play" | "preparation" => playing.store(true, Ordering::Release),
        "credit" => {
            let seen = gate.stamp();
            push.credit(push.session(), u64::MAX);
            assert_ne!(seen, gate.stamp(), "the refund wakes the driver's gate");
        }
        _ => {
            next_epoch += 1;
            now.store(next_epoch, Ordering::Release);
            reads.lock().unwrap().anchor(next_epoch, seek + 1);
            push.head(route, seek + 1, head);
        }
    }
    let mut tasks = parking.ready(next_epoch, held_at());
    assert_eq!(tasks.len(), 1);
    tasks.pop().unwrap()();
    let mut product = work.take().expect("the worker returned the product");
    if release == "play" {
        assert_eq!(*sampled.lock().unwrap(), [head, through], "the resumed worker reads the boundary once and skips the backlog");
        assert!(push.try_take().is_none(), "Play does not send replies from the parked catch-up");
        assert_eq!(product.last, Some((epoch, seek, head)), "the handoff does not claim a newer reading");
        assert!(!product.joined, "the first live step still owes its panel confirmation");
        assert_eq!(product.resumed_step(through - 1), (through, None, None), "an earlier step cannot claim the prepared reply");
        let (next, change, proof) = product.resumed_step(through);
        assert_eq!((next, proof), (through + 1, Some(through)));
        let (hour, kind, body) = change.expect("the step can publish the worker's reply");
        assert_eq!((hour, kind), (through, panel::dated::DATA));
        assert_eq!(rmp_serde::from_slice::<Hour>(&body).unwrap(), through);
        assert!(product.resume.is_none(), "the product can join once the reply reaches the step");
        product.resume = Some(PanelResume::Ready(hour, kind, body));
        let (next, _, proof) = product.resumed_step(through + 2);
        assert_eq!((next, proof), (through + 2, Some(through)), "the newer step confirms only the worker's hour");
        assert!(matches!(product.resume, Some(PanelResume::Read(at)) if at == through + 2));
        assert_eq!(*sampled.lock().unwrap(), [head, through], "taking the reply performs no read");
    } else if release == "preparation" {
        let (next, _, proof) = product.resumed_step(through);
        assert_eq!((next, proof), (through, None), "a pending read must leave its own hour eligible");
        let resume = |product: &StepProduct| product.resume.as_ref().and_then(|resume| resume.due_read(through));
        assert_eq!(resume(&product), None, "the worker cannot retry while preparation is pending");
        let wake = subscriber.try_recv().expect("the parked read subscribed to preparation");
        let waiting = wake_gate.stamp();
        playing.store(false, Ordering::Release);
        prepared.store(true, Ordering::Release);
        wake();
        assert_ne!(wake_gate.stamp(), waiting, "preparation wakes the paused host");
        let due = resume(&product).expect("the paused hour is ready to retry");
        job().run(PanelRead::Resume(due), product, work.clone(), move || Some(PanelClock { epoch, head: through, playing: None, newest: Some(through) }));
        let (next, change, proof) = work.take().unwrap().resumed_step(through);
        let (hour, kind, body) = change.expect("ready preparation supplies the paused picture");
        assert_eq!((next, proof, hour, kind), (through + 1, Some(through), through, panel::dated::DATA));
        assert_eq!(rmp_serde::from_slice::<Hour>(&body).unwrap(), through);
        assert_eq!(*sampled.lock().unwrap(), [head, through, through]);
    } else if release == "seek" {
        assert_eq!(*sampled.lock().unwrap(), [head], "the seek wakes the job so it can retire without computing");
        assert!(push.try_take().is_none());
    } else {
        assert_eq!(*sampled.lock().unwrap(), [head, head + 1]);
        let (_, bytes, ..) = push.try_take().unwrap();
        assert_eq!(panel::dated::records(&bytes).unwrap()[0].hour, head + 1);
    }
}

#[cfg(test)]
#[test]
fn a_pending_handoff_can_retry_the_paused_hour() {
    check_parked_panel("preparation");
}

#[cfg(test)]
#[test]
fn an_older_handoff_dispatches_the_final_hour_without_another_native_step() {
    use crate::panel::{ProductRead, dated};
    use crate::tests::playing_panel_with_hook;

    #[derive(Debug, PartialEq, Eq)]
    enum Gate {
        FinalStep,
        BeforeArming,
        BeforeHandoff,
        AfterHandoff,
    }

    let armed = Arc::new(AtomicBool::new(false));
    let observe = Arc::clone(&armed);
    let (gates, hook) = mpsc::channel();
    let mut at_final = false;
    let mut unarmed_hook = false;
    let mut handoff_hook = false;
    let advance = Arc::new(|hour, _, _: &AtomicBool, _: Option<panel::ProductStep<'_>>| ProductRead {
        value: Arc::new(Ok(serde_json::json!(hour))), rows: 1, fresh: true, pending: false, live: None, lanes: Vec::new(), from: hour,
    });
    let (host, push, ..) = playing_panel_with_hook(advance, true, move |step, last, after| {
        if step.last() != last { return; }
        let gate = if step.first() <= last {
            if after || at_final { return; }
            at_final = true;
            Gate::FinalStep
        } else if after {
            if !std::mem::take(&mut handoff_hook) { return; }
            Gate::AfterHandoff
        } else if !unarmed_hook {
            unarmed_hook = true;
            Gate::BeforeArming
        } else if observe.swap(false, Ordering::AcqRel) {
            handoff_hook = true;
            Gate::BeforeHandoff
        } else {
            return;
        };
        let (permit, wait) = mpsc::channel();
        if gates.send((gate, permit)).is_ok() { let _ = wait.recv(); }
    });
    let hook = hook;
    let playhead = host.playhead();
    let last = playhead.timeline().last();
    let (gate, permit) = hook.recv().unwrap();
    assert_eq!(gate, Gate::FinalStep);
    let work = host.panels.lock().unwrap().products[&1].clone();
    let mut product = None;
    playhead.wait_for(|| {
        product = work.take();
        product.is_some()
    }).unwrap();
    let mut product = product.unwrap();
    permit.send(()).unwrap();
    playhead.wait_newest(last).unwrap();
    host.follow.rehooker()();
    let (gate, permit) = hook.recv().unwrap();
    assert_eq!(gate, Gate::BeforeArming);
    armed.store(true, Ordering::Release);
    host.follow.rehooker()();
    permit.send(()).unwrap();
    let (gate, permit) = hook.recv().unwrap();
    assert_eq!(gate, Gate::BeforeHandoff);
    let older = serde_json::json!({ "late": last - 1 });
    let old = Arc::new(Ok(older.clone()));
    let (kind, body) = product.wire.update(true, &old).unwrap();
    product.resume = Some(PanelResume::Ready(last - 1, kind, body.to_vec()));
    work.put(product);
    permit.send(()).unwrap();
    let (gate, permit) = hook.recv().unwrap();
    assert_eq!(gate, Gate::AfterHandoff);
    {
        let state = work.state.lock().unwrap();
        assert!(state.0.as_ref().is_none_or(|product| matches!(product.resume, Some(PanelResume::Ready(hour, ..)) if hour == last)),
            "the hook must consume the older reply and dispatch the final hour's read");
    }
    let mut delivered_older = false;
    while let Some((kind, bytes, ..)) = push.try_take() {
        push.credit(push.session(), u64::MAX);
        if kind != PANEL { continue; }
        for record in dated::records(&bytes).unwrap().into_iter().filter(|r| r.id == 1 && r.kind == dated::DATA) {
            assert!(record.hour < last, "the final hour cannot publish while the handoff hook is held");
            let value = rmp_serde::from_slice::<Value>(record.body).unwrap();
            if record.hour == last - 1 && value == older { delivered_older = true; }
        }
    }
    assert!(delivered_older, "the injected reply must reach the wire before the final reading");
    permit.send(()).unwrap();
    loop {
        let (kind, bytes, session) = push.take_sent().unwrap();
        push.credit(session, u64::MAX);
        if kind != PANEL { continue; }
        if let Some(record) = dated::records(&bytes).unwrap().into_iter().find(|r| r.id == 1 && r.hour == last && r.kind == dated::DATA) {
            assert_eq!(rmp_serde::from_slice::<Hour>(record.body).unwrap(), last);
            break;
        }
    }
}

#[cfg(test)]
#[test]
fn a_product_lane_redeclared_on_its_table_keeps_every_flag_it_asked_for() {
    let follow = Follow::new(0);
    let host = HostCells { follow: follow.clone(), held: vec![true; 4] };
    let lane = |flag| Declared { table: 2, columns: vec![1], keyed: flag, passing: flag, hourly: flag };
    retain_lanes(4, &[lane(false)], Some(&host));
    retain_lanes(4, &[lane(true)], Some(&host));
    assert_eq!(follow.declared().unwrap(), [lane(true)], "the playhead is told the lane's flags, not only its columns");
}

#[cfg(test)]
#[test]
fn wait_gate_never_misses_wake_before_wait_and_stops_blocked_waiter() {
    let gate = Arc::new(WaitGate::new());
    let seen = gate.stamp();
    gate.wake();
    assert!(!gate.wait(seen, None));

    let seen = gate.stamp();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = Arc::clone(&gate);
    let handle = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let stopped = worker.wait(seen, None);
        done_tx.send(stopped).unwrap();
    });
    ready_rx.recv().unwrap();
    gate.stop();
    assert!(done_rx.recv().unwrap());
    handle.join().unwrap();
}

#[cfg(test)]
#[test]
fn shown_stands_interleaved_across_paused_reset_preserves_head_and_refreshes_on_change() {
    use replay_data::rows::TableId;
    use replay_frame::frame::{DecodedFrame, Metrics, SharedView, TableView};
    use std::sync::atomic::AtomicUsize;

    struct InterleavedReader {
        frame: Mutex<(Arc<DecodedFrame>, Hour, u64)>,
    }
    impl panel::PanelReader for InterleavedReader {
        fn frame(&self) -> Option<Arc<DecodedFrame>> { Some(Arc::clone(&self.frame.lock().unwrap().0)) }
        fn through_seek(&self, _: &Arc<DecodedFrame>) -> Option<(Hour, u64)> {
            let g = self.frame.lock().unwrap();
            Some((g.1, g.2))
        }
    }
    struct SampledPanel(Arc<AtomicUsize>);
    impl panel::Panel for SampledPanel {
        fn tables(&self) -> &[TableId] { &[] }
        fn data(&mut self, frame: &DecodedFrame) -> panel::PanelData {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(serde_json::json!({ "sampledAt": frame.tables[2].sampled_at() }))
        }
    }

    let shared: SharedView = TableView::Complete(replay_data::rows::TableRows::Countries(Vec::new())).into();
    let mk_frame = |sampled_at: Option<Hour>| {
        let mut tables = std::array::from_fn(|_| replay_frame::frame::not_selected());
        tables[2] = shared.clone().with_sampled_at(sampled_at);
        Arc::new(DecodedFrame { branch: 1, hour: 0, tables, metrics: Metrics::default() })
    };
    let state = Arc::new(InterleavedReader { frame: Mutex::new((mk_frame(Some(0)), 0, 1)) });
    struct Wrap(Arc<InterleavedReader>);
    impl panel::PanelReader for Wrap {
        fn frame(&self) -> Option<Arc<DecodedFrame>> { self.0.frame() }
        fn through_seek(&self, f: &Arc<DecodedFrame>) -> Option<(Hour, u64)> { self.0.through_seek(f) }
    }
    let evals = Arc::new(AtomicUsize::new(0));
    let mut shown = Shown::new(Box::new(SampledPanel(Arc::clone(&evals))), Box::new(Wrap(Arc::clone(&state))), (0, 1));
    let active = Mutex::new(ProductReads { epoch: 1, seek: 1, ..Default::default() });
    active.lock().unwrap().register(43);
    let token = active.lock().unwrap().tokens[&43].clone();
    let push = Push::default();
    let live_epoch_state = Arc::new(AtomicU64::new(1));

    evaluate(43, &mut shown, (0, 1, 1), 1, 0, &token, &active, &push, 0, || Some((live_epoch_state.load(Ordering::Acquire), 0)));
    let _ = push.try_take().expect("initial DATA packet");
    assert_eq!(evals.load(Ordering::Acquire), 1);

    *state.frame.lock().unwrap() = (mk_frame(Some(0)), 5, 1);
    let calls = AtomicUsize::new(0);
    evaluate(43, &mut shown, (0, 1, 1), 1, 0, &token, &active, &push, 0, || {
        let current = live_epoch_state.load(Ordering::Acquire);
        if calls.fetch_add(1, Ordering::AcqRel) == 1 {
            live_epoch_state.store(2, Ordering::Release);
        }
        Some((current, 0))
    });
    let (_, bytes, _, _) = push.try_take().expect("interleaved STANDS(5) packet offered after epoch bumped to 2");
    let records = panel::dated::records(&bytes).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!((records[0].id, records[0].kind, records[0].hour), (43, panel::dated::STANDS, 5));

    *state.frame.lock().unwrap() = (mk_frame(Some(0)), 0, 2);
    evaluate(43, &mut shown, (0, 1, 1), 2, 0, &token, &active, &push, 0, || Some((2, 0)));
    let (_, bytes, _, _) = push.try_take().expect("epoch 2 invalidates old-epoch pushed=5 and publishes fresh DATA(0)");
    let records = panel::dated::records(&bytes).unwrap();
    assert_eq!((records[0].id, records[0].kind, records[0].hour), (43, panel::dated::DATA, 0));
    let body: Value = rmp_serde::from_slice(records[0].body).unwrap();
    assert_eq!(body["sampledAt"], 0);
    assert_eq!(evals.load(Ordering::Acquire), 2);

    *state.frame.lock().unwrap() = (mk_frame(Some(2)), 0, 2);
    evaluate(43, &mut shown, (0, 1, 1), 2, 0, &token, &active, &push, 0, || Some((2, 0)));
    let (_, bytes, _, _) = push.try_take().expect("changed metadata at epoch 2 re-evaluates and publishes DATA");
    let records = panel::dated::records(&bytes).unwrap();
    assert_eq!((records[0].id, records[0].kind, records[0].hour), (43, panel::dated::DATA, 0));
    let body: Value = rmp_serde::from_slice(records[0].body).unwrap();
    assert_eq!(body["sampledAt"], 2);
    assert_eq!(evals.load(Ordering::Acquire), 3);
}
