pub mod combat;
pub(crate) mod shown_vision;
pub mod details;
pub mod mirror;
pub mod piece;
pub mod standing;
mod script_values;
pub mod timeline;
pub mod views;
#[cfg(any(test, feature = "test-support"))]
pub mod shared_maps_tests;
#[cfg(test)]
mod tests;
pub mod units;

use crate::layers::{Read, Upstream, Settling, Gpu, Inputs, Installs, Layer, View};
use mirror::{Mirror, Mirrors};
use playhead::{Consumer, Declared, Lanes, Playhead, Reader, Settings, Step, pages::Pages};
use replay_columnar::state::Kind;
use replay_data::{Hour, rows::TableId};
use viewer_counter_kit::Kinds;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::Instant,
};
use units::Units;

pub type Hook = Box<dyn FnMut(&Step, &Lanes<'_>) + Send>;

type HookList = Vec<(u64, Arc<Mutex<Hook>>)>;

#[derive(Default)]
pub struct Hooks {
    held: Mutex<Arc<HookList>>,
    next: AtomicU64,
}

impl Hooks {
    pub fn add(&self, hook: Hook) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        Arc::make_mut(&mut lock(&self.held)).push((id, Arc::new(Mutex::new(hook))));
        id
    }

    pub fn remove(&self, id: u64) {
        let removed = {
            let mut hooks = lock(&self.held);
            hooks.iter().position(|(held, _)| *held == id).map(|at| Arc::make_mut(&mut hooks).remove(at))
        };
        drop(removed);
    }

    fn run(&self, step: &Step, lanes: &Lanes<'_>) {
        let hooks = Arc::clone(&lock(&self.held));
        for (_, hook) in hooks.iter() {
            lock(hook)(step, lanes);
        }
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct Entry {
    page_time: Mutex<playhead::PageTime>,
    playhead: Playhead,
    pages: Arc<dyn Pages>,
    kinds: Arc<Vec<Vec<Kind>>>,
    hooks: Arc<Hooks>,
    anchors: Mutex<(Option<Hour>, Option<(bool, u64)>)>,
    capture: std::sync::OnceLock<replay_data::scope::CaptureMode>,
    maps: std::sync::atomic::AtomicUsize,
    rosters: std::sync::OnceLock<Option<Arc<viewer_counter_products::map::province_tooltips::air_region::Sampler>>>,
}

impl Entry {
    pub fn follow_anchor(&self, anchor: dashboard_follow::registry::Anchor) {
        let at = lock(&self.page_time).composed(anchor.timestamps, Instant::now());
        let mut held = lock(&self.anchors);
        let pace = (anchor.playing && anchor.speed > 0.0, anchor.speed.to_bits());
        let previous_step = self.playhead.step_hours();
        self.playhead.frame_rate(anchor.frame_rate);
        if held.1.is_none_or(|(_, speed)| speed != pace.1) { self.playhead.pace(anchor.speed); }
        let seek = anchor.sought || self.playhead.step_hours() < previous_step || (!pace.0 && held.1.is_some_and(|(moving, _)| moving));
        held.1 = Some(pace);
        if seek { held.0 = Some(anchor.hour); }
        self.playhead.anchor(anchor.position, pace.0, anchor.speed, at, seek);
    }

    pub fn anchor(&self, hour: Hour, playing: bool, speed: f64, sought: bool) {
        self.follow_rate();
        let mut held = lock(&self.anchors);
        let playhead = &self.playhead;
        let pace = (playing && speed > 0.0, speed.to_bits());
        let old_step = playhead.step_hours();
        let at = held.0 == Some(hour) && (playhead.holding().is_some() || (playhead.head(Instant::now()) == hour && held.1.is_none_or(|(moving, _)| !moving)));
        if held.1 != Some(pace) {
            if pace.0 {
                playhead.play(speed);
            } else {
                playhead.pace(speed);
                playhead.pause();
            }
            held.1 = Some(pace);
        }
        if (sought && !at) || playhead.step_hours() < old_step {
            playhead.seek(hour);
            held.0 = Some(hour);
        } else if !pace.0 && playhead.head(Instant::now()) != playhead.timeline().clamp(hour) {
            playhead.place(hour);
            held.0 = Some(hour);
        }
    }

    pub fn follow_rate(&self) {
        self.playhead.frame_rate(step_rate());
    }

    pub fn paused(&self) -> bool {
        matches!(lock(&self.anchors).1, Some((false, _)))
    }

    fn exact(&self, hour: Hour) {
        let mut held = lock(&self.anchors);
        self.playhead.seek(hour);
        held.0 = Some(hour);
    }

    pub fn anchored(&self) -> bool {
        lock(&self.anchors).0.is_some()
    }

    pub fn set_capture(&self, capture: replay_data::scope::CaptureMode) {
        let _ = self.capture.set(capture);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn drawn(&self) -> bool {
        self.maps.load(Ordering::Acquire) > 0
    }

    pub fn playhead(&self) -> &Playhead {
        &self.playhead
    }

    pub fn hooks(&self) -> &Hooks {
        &self.hooks
    }

    pub fn kinds(&self) -> &[Vec<Kind>] {
        &self.kinds
    }

    pub fn pages(&self) -> &Arc<dyn Pages> {
        &self.pages
    }

    pub fn rosters(&self, replay: &Arc<replay::campaign::Replay>) -> Option<Arc<viewer_counter_products::map::province_tooltips::air_region::Sampler>> {
        use viewer_counter_products::map::province_tooltips::air_region::Sampler;
        self.rosters
            .get_or_init(|| crate::lane_rows::rosters::Lanes::of(self, replay).map(|lanes| Arc::new(Sampler::new(Arc::clone(replay), Box::new(lanes)))))
            .clone()
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Map,
    Follow,
}

type Key = (Side, String, Vec<(usize, Hour, Hour)>, Hour);

static ENTRIES: Mutex<Vec<(Key, Weak<Entry>)>> = Mutex::new(Vec::new());

pub fn open(units: Units, settings: Settings) -> io::Result<Arc<Entry>> {
    open_clocked(units, settings, 0, Side::Map)
}

pub fn open_private(units: Units, settings: Settings, side: Side) -> io::Result<Arc<Entry>> {
    let settings = Settings { entering: Some(if side == Side::Follow { entered } else { entered_map }), ..settings };
    let hooks = Arc::new(Hooks::default());
    let stepper_hooks = Arc::clone(&hooks);
    let hook: playhead::Hook = Box::new(move |step: &Step, lanes: &Lanes<'_>| stepper_hooks.run(step, lanes));
    let playhead = Playhead::new(Arc::clone(&units.pages), units.timeline, settings, Some(hook))?;
    Ok(Arc::new(Entry { page_time: Default::default(), playhead, pages: units.pages, kinds: units.kinds, hooks, anchors: Mutex::new((None, None)), capture: Default::default(), maps: Default::default(), rosters: Default::default() }))
}

pub fn open_clocked(units: Units, settings: Settings, clock: Hour, side: Side) -> io::Result<Arc<Entry>> {
    let key: Key = (side, units.name.clone(), units.timeline.pieces().iter().map(|piece| (piece.run, piece.first, piece.last)).collect(), clock);
    let mut entries = lock(&ENTRIES);
    entries.retain(|(_, entry)| entry.strong_count() > 0);
    if let Some(entry) = entries.iter().find(|(held, _)| *held == key).and_then(|(_, entry)| entry.upgrade()) {
        return Ok(entry);
    }
    let settings = Settings { entering: Some(if side == Side::Follow { entered } else { entered_map }), ..settings };
    let hooks = Arc::new(Hooks::default());
    let stepper_hooks = Arc::clone(&hooks);
    let hook: playhead::Hook = Box::new(move |step: &Step, lanes: &Lanes<'_>| stepper_hooks.run(step, lanes));
    let playhead = Playhead::new(Arc::clone(&units.pages), units.timeline, settings, Some(hook))?;
    let entry = Arc::new(Entry { page_time: Default::default(), playhead, pages: units.pages, kinds: units.kinds, hooks, anchors: Mutex::new((None, None)), capture: Default::default(), maps: Default::default(), rosters: Default::default() });
    entries.push((key, Arc::downgrade(&entry)));
    Ok(entry)
}

const HANDS_BACK: std::time::Duration = std::time::Duration::from_secs(2);

const HURRY: std::time::Duration = std::time::Duration::from_secs(1);

const CATCHING_UP: std::time::Duration = std::time::Duration::from_secs(60);

fn step_rate() -> f64 {
    viewer_runtime::simulation_rate::rate()
}

pub fn settings() -> Settings {
    Settings { step_threads: 2, fetch_threads: 1, ring: 8, ahead: 4, rate: step_rate(), entering: Some(entered), ..Settings::default() }
}

fn entered_map(thread: playhead::Thread) {
    use viewer_runtime::worker_priority::{Class, enter};
    enter(match thread {
        playhead::Thread::Stepper | playhead::Thread::Step | playhead::Thread::Seeking => Class::Kept,
        playhead::Thread::Fetch => Class::Worker,
    });
}

fn entered(thread: playhead::Thread) {
    use viewer_runtime::worker_priority::{Class, enter};
    enter(match thread {
        playhead::Thread::Stepper => Class::Playback,
        playhead::Thread::Step | playhead::Thread::Seeking => Class::Kept,
        playhead::Thread::Fetch => Class::Worker,
    });
}

static REGISTERED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub fn register_only(names: Vec<String>) {
    let _ = REGISTERED.set(names);
}

pub fn keep_registered(layers: &mut Vec<Box<dyn Layer>>) {
    if let Some(names) = REGISTERED.get() {
        layers.retain(|layer| names.iter().any(|name| name == layer.name()));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct At {
    pub branch: replay_data::BranchId,
    pub hour: Hour,
    pub playing: bool,
    pub speed: f64,
}

pub struct Opening<P: PlaybackProducts = NoProducts> {
    made: Receiver<io::Result<MapSide<P>>>,
}

impl<P: PlaybackProducts> Opening<P> {
    pub fn spawn(make: impl FnOnce() -> io::Result<MapSide<P>> + Send + 'static) -> io::Result<Self> {
        let (send, made) = mpsc::channel();
        std::thread::Builder::new()
            .name(OPENING.into())
            .spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                let _ = send.send(make());
            })
            .map_err(|error| io::Error::other(format!("no thread to open it: {error}")))?;
        Ok(Self { made })
    }

    pub fn poll(&self) -> Option<io::Result<MapSide<P>>> {
        match self.made.try_recv() {
            Ok(side) => Some(side),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(io::Error::other("its opening thread stopped before it was made"))),
        }
    }
}

pub const OPENING: &str = "map reader opening";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head {
    Clock,
    Paced(Hour),
    Stepped(Hour),
}

enum Job {
    Compose(Box<Away>),
    Drop(Box<dyn Send>),
}

struct Away {
    mirrors: Arc<Mirrors>,
    layers: Vec<Box<dyn Layer>>,
    compose: Vec<bool>,
    upstream: Vec<Vec<usize>>,
    cost: Vec<u64>,
    cancel: Arc<AtomicBool>,
    results: Vec<(io::Result<()>, u64, bool)>,
}

struct Group {
    at: Vec<usize>,
    layers: Vec<Box<dyn Layer>>,
    results: Vec<(io::Result<()>, u64, bool)>,
    cost: u64,
}

impl Group {
    fn compose(&mut self, mirrors: &Mirrors, compose: &[bool], cancel: &AtomicBool) {
        for member in 0..self.layers.len() {
            let at = self.at[member];
            let wanted = compose[at] && !cancel.load(Ordering::Acquire);
            let (before, rest) = self.layers.split_at_mut(member);
            let started = Instant::now();
            let result = if wanted { rest[0].compose_with(mirrors, &Upstream::new(before)) } else { Ok(()) };
            self.results.push((result, started.elapsed().as_nanos() as u64, wanted));
        }
    }
}

fn groups(upstream: &[Vec<usize>]) -> Vec<usize> {
    fn root(of: &mut [usize], mut at: usize) -> usize {
        while of[at] != at {
            of[at] = of[of[at]];
            at = of[at];
        }
        at
    }
    let mut of: Vec<usize> = (0..upstream.len()).collect();
    for (at, uppers) in upstream.iter().enumerate() {
        for &upper in uppers {
            let (a, b) = (root(&mut of, at), root(&mut of, upper));
            of[a.max(b)] = a.min(b);
        }
    }
    (0..upstream.len()).map(|at| root(&mut of, at)).collect()
}

const COMPOSING: &str = "playhead host compose";

const COMPOSE_CLASS: viewer_runtime::worker_priority::Class = viewer_runtime::worker_priority::Class::Kept;

fn compose_away(away: &mut Away) {
    let count = away.layers.len();
    let first = groups(&away.upstream);
    let mut group_of = vec![usize::MAX; count];
    let mut all: Vec<Group> = Vec::new();
    for (at, layer) in std::mem::take(&mut away.layers).into_iter().enumerate() {
        let root = first[at];
        if group_of[root] == usize::MAX {
            group_of[root] = all.len();
            all.push(Group { at: Vec::new(), layers: Vec::new(), results: Vec::new(), cost: 0 });
        }
        let group = &mut all[group_of[root]];
        group.at.push(at);
        group.layers.push(layer);
        if away.compose[at] {
            group.cost += away.cost.get(at).copied().unwrap_or(0).max(1);
        }
    }
    let (mut waiting, idle): (Vec<Group>, Vec<Group>) = all.into_iter().partition(|group| group.cost > 0);
    waiting.sort_by_key(|group| group.cost);
    let threads = waiting.len().min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
    let (queue, done) = (Mutex::new(waiting), Mutex::new(Vec::with_capacity(count)));
    let (mirrors, compose, cancel) = (&*away.mirrors, &away.compose[..], &*away.cancel);
    let work = || {
        loop {
            let next = lock(&queue).pop();
            let Some(mut group) = next else { break };
            group.compose(mirrors, compose, cancel);
            lock(&done).push(group);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            if std::thread::Builder::new().name(COMPOSING.into()).spawn_scoped(scope, || {
                viewer_runtime::worker_priority::enter(COMPOSE_CLASS);
                work();
            }).is_err() {
                break;
            }
        }
        work();
    });
    let mut layers: Vec<Option<Box<dyn Layer>>> = (0..count).map(|_| None).collect();
    let mut results: Vec<Option<(io::Result<()>, u64, bool)>> = (0..count).map(|_| None).collect();
    let idle = idle.into_iter().map(|group| Group { results: group.at.iter().map(|_| (Ok(()), 0, false)).collect(), ..group });
    for group in done.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner).into_iter().chain(idle) {
        for ((at, layer), result) in group.at.into_iter().zip(group.layers).zip(group.results) {
            layers[at] = Some(layer);
            results[at] = Some(result);
        }
    }
    away.layers = layers.into_iter().map(|layer| layer.expect("every layer comes back from its group")).collect();
    away.results = results.into_iter().map(|result| result.expect("every layer of a group has a result")).collect();
}

struct Worker {
    jobs: Sender<Job>,
    done: viewer_runtime::landed::Receiver<Box<Away>>,
}

impl Worker {
    fn new() -> io::Result<Self> {
        let (jobs, receive) = mpsc::channel::<Job>();
        let (send, done) = viewer_runtime::landed::channel();
        std::thread::Builder::new().name("playhead host worker".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(COMPOSE_CLASS);
            for job in receive {
                match job {
                    Job::Compose(mut away) => {
                        compose_away(&mut away);
                        if send.send(away).is_err() {
                            return;
                        }
                    }
                    Job::Drop(held) => drop(held),
                }
            }
        })?;
        Ok(Self { jobs, done })
    }

    fn drop_later(&self, held: impl Send + 'static) {
        let _ = self.jobs.send(Job::Drop(Box::new(held)));
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LayerFrame {
    pub apply_ns: u64,
    pub settle_ns: u64,
    pub apply_cycles: u64,
    pub settle_cycles: u64,
    pub dirty: usize,
    pub stage_ns: u64,
    pub stage_cycles: u64,
    pub upload_ns: u64,
    pub upload_cycles: u64,
    pub uploads: u64,
    pub upload_bytes: u64,
    pub write_ns: u64,
    pub create_ns: u64,
    pub upload_carried: bool,
    pub published: u8,
    pub ready: bool,
    pub shown: Option<Hour>,
    pub reset_shown: bool,
    pub ready_at_hour: Option<bool>,
    pub ahead_ns: u64,
    pub scope: Option<crate::layers::Scope>,
}

#[cfg(any(test, feature = "test-support"))]
#[must_use = "the settles are released as soon as this is dropped"]
pub struct SettlesHeld(Arc<AtomicBool>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for SettlesHeld {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stages {
    pub ns: [u64; Stages::NAMES.len()],
    pub cycles: [u64; Stages::NAMES.len()],
}

impl Stages {
    pub const NAMES: [&str; 8] = ["views", "take", "apply", "settles", "joins", "in_view", "stage", "shown"];
}

pub struct Doing(AtomicU8);

impl Doing {
    const IDLE: u8 = 0;
    const WHOLE: u8 = Stages::NAMES.len() as u8 + 1;

    pub fn name(&self) -> &'static str {
        match self.0.load(Ordering::Relaxed) {
            Self::IDLE => "between publications",
            Self::WHOLE => "a whole settle",
            stage => Stages::NAMES.get(stage as usize - 1).copied().unwrap_or("a stage of its own"),
        }
    }

    fn set(&self, doing: u8) {
        self.0.store(doing, Ordering::Relaxed);
    }
}

impl Default for Doing {
    fn default() -> Self {
        Self(AtomicU8::new(Self::IDLE))
    }
}

struct Watch<'a> {
    at: Instant,
    cycles: u64,
    doing: &'a Doing,
}

impl<'a> Watch<'a> {
    fn new(doing: &'a Doing) -> Self {
        doing.set(1);
        Self { at: Instant::now(), cycles: replay_frame::cpu::thread_cycles(), doing }
    }

    fn lap(&mut self, stages: &mut Stages, stage: usize) {
        let (at, cycles) = (Instant::now(), replay_frame::cpu::thread_cycles());
        stages.ns[stage] = at.saturating_duration_since(self.at).as_nanos() as u64;
        stages.cycles[stage] = cycles.saturating_sub(self.cycles);
        (self.at, self.cycles) = (at, cycles);
        self.doing.set(stage as u8 + 2);
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        self.doing.set(Doing::IDLE);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Waited {
    pub late_ns: u64,
    pub for_an_hour_ns: u64,
    pub for_the_frame_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub made: u64,
    pub head: Hour,
    pub steps: u32,
    pub first: Option<Hour>,
    pub applied: Option<Hour>,
    pub mirror_ns: u64,
    pub mirror_cycles: u64,
    pub take_ns: u64,
    pub take_cycles: u64,
    pub mirror_tables: Vec<(u64, u64)>,
    pub mirror_split: mirror::Split,
    pub cells: u64,
    pub rows: u64,
    pub layers: Vec<LayerFrame>,
    pub voided: u32,
    pub composing: bool,
    pub exact: bool,
    pub swapped: Option<Vec<u64>>,
    pub resets: u32,
    pub sought: Option<playhead::Seeking>,
    pub drawn: Option<Hour>,
    pub oldest: Option<Hour>,
    pub step_ns: u64,
    pub step_most_ns: u64,
    pub step_cycles: u64,
    pub side_ns: u64,
    pub side_cycles: u64,
    pub stages: Stages,
    pub waited: Waited,
    pub renumbers: Vec<(usize, u32, u32)>,
    pub reset_tables: Vec<usize>,
    pub reset_step: bool,
    pub reached_ns: u64,
    pub superseded: bool,
    pub reset_shown: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Held {
    pub mirrors: Vec<(TableId, usize)>,
    pub views: Vec<(TableId, usize)>,
    pub layers: Vec<(&'static str, usize)>,
    pub pending: usize,
    pub states: (usize, usize),
    pub playhead: usize,
}

impl Held {
    pub fn total(&self) -> usize {
        self.mirrors.iter().map(|(_, bytes)| bytes).sum::<usize>() + self.views.iter().map(|(_, bytes)| bytes).sum::<usize>() + self.layers.iter().map(|(_, bytes)| bytes).sum::<usize>() + self.states.1
    }
}

pub trait PlaybackProducts: 'static {
    type Sight: Send + Sync;
    type Combat: combat::Asking;
    type OverlayAsk;
    type OverlayMade;

    fn sight(content: &Self::Sight) -> crate::layers::SightInputs<'_>;
    fn overlay_viewer(made: &Self::OverlayMade) -> u32;
}

pub struct NoProducts;

impl PlaybackProducts for NoProducts {
    type Sight = crate::scene_content::SceneContent;
    type Combat = combat::NoCombat;
    type OverlayAsk = std::convert::Infallible;
    type OverlayMade = std::convert::Infallible;

    fn sight(content: &Self::Sight) -> crate::layers::SightInputs<'_> {
        crate::layers::SightInputs { scene: content, state_provinces: &content.products.triggers.state_provinces }
    }

    fn overlay_viewer(made: &Self::OverlayMade) -> u32 {
        match *made {}
    }
}

pub const COUNTER_LAYERS: [(&str, Kinds); 6] = [
    ("counters", Kinds::LAND),
    ("land_markers", Kinds::LAND_MARKERS),
    ("naval_markers", Kinds::NAVAL_MARKERS),
    ("base_counters", Kinds::BASES),
    ("gun_counters", Kinds::GUNS),
    ("air_mission_counters", Kinds::AIR_MISSIONS),
];

fn counters_at(names: &[&'static str]) -> Option<usize> {
    names.iter().position(|name| COUNTER_LAYERS.iter().any(|(held, _)| held == name))
}

pub trait Overlaying<P: PlaybackProducts>: Send {
    fn ask(&mut self, ask: P::OverlayAsk);
    fn ask_step(&mut self, side: OverlayStep<'_>, whole: bool, lay: bool, moved: Arc<viewer_unit_reads::counter_move::MovedHours>) -> bool;
    fn join(&mut self, layers: &[Box<dyn Layer>], perspective: &viewer_foundation::perspective::Choice, branch: Option<replay_data::BranchId>, settling: Settling) -> bool;
    fn stepped(&mut self);
    fn stands_at(&self, hour: Hour) -> bool;
    fn take(&mut self) -> Option<viewer_runtime::landed::Landed<P::OverlayMade>>;
    fn reset(&mut self);
    fn generation(&self) -> u64;
    fn hour(&self) -> Option<Hour>;
    fn loading(&self) -> &[String];
    fn missed(&self) -> &[TableId];
    fn refused(&self) -> bool;
    fn needs_reask(&self) -> bool;
    fn offered_stale(&self, layers: LayerParts<'_>, branch: Option<replay_data::BranchId>) -> bool;
}

#[derive(Clone, Copy)]
pub struct LayerParts<'a> {
    names: &'a [&'static str],
    reads: &'a [Result<BTreeSet<usize>, String>],
    ready: &'a [bool],
    mirrors: Option<&'a Mirrors>,
    layers: &'a [Box<dyn Layer>],
    composing: bool,
    superseded: bool,
}

impl<'a> LayerParts<'a> {
    pub fn draws(&self, name: &str) -> bool {
        self.names.iter().enumerate().any(|(at, held)| *held == name && self.reads[at].is_ok() && self.ready[at])
    }

    pub fn parts(&self) -> Option<(&'a Mirrors, &'a [Box<dyn Layer>])> {
        self.mirrors.filter(|_| !self.superseded).map(|mirrors| (mirrors, self.layers))
    }

    pub fn drawn<T: 'static>(&self, name: &str) -> Option<&'a T> {
        if !self.draws(name) {
            return None;
        }
        self.parts()?.1.iter().find_map(|layer| layer.any().downcast_ref::<T>())
    }

    pub fn viewer(&self) -> Option<u32> {
        self.vision_layer()?.ours()
    }

    fn vision_layer(&self) -> Option<&'a crate::vision::Vision> {
        vision_of(self.names, self.reads, self.ready, self.layers, self.composing || self.superseded)
    }

    fn engaged(&self) -> Option<Arc<viewer_engagements::engagement_history::History>> {
        self.layers.iter().find_map(|layer| layer.any().downcast_ref::<crate::engagements::Engagements>()).and_then(crate::engagements::Engagements::history).map(Arc::clone)
    }
}

pub struct OverlayStep<'a> {
    pub layers: LayerParts<'a>,
    pub views: &'a views::Store,
    pub applied: Option<Hour>,
    pub branch: Option<replay_data::BranchId>,
    pub perspective: &'a viewer_foundation::perspective::Choice,
    vision: &'a mut shown_vision::ShownVision,
}

impl OverlayStep<'_> {
    pub fn engaged(&self) -> Option<Arc<viewer_engagements::engagement_history::History>> {
        self.layers.engaged()
    }

    pub fn vision(&mut self, whole: bool) -> Result<viewer_runtime::landed::Landed<Arc<viewer_foundation::perspective::Vision>>, String> {
        if whole {
            let home = self.layers.mirrors.is_some() && !self.layers.composing && !self.layers.superseded;
            let layer = self.layers.vision_layer();
            self.vision.shown(layer, self.applied, home, true)
        } else {
            self.vision.kept_for(self.perspective)
        }
    }
}

pub struct MapSide<P: PlaybackProducts = NoProducts> {
    entry: Arc<Entry>,
    reader: Reader,
    consumer: Consumer,
    view: View,
    declared: BTreeMap<usize, Read>,
    reads: Vec<Result<BTreeSet<usize>, String>>,
    ready: Vec<bool>,
    upstream: Vec<Vec<usize>>,
    recompose: Vec<bool>,
    names: Vec<&'static str>,
    counter_at: Option<usize>,
    mirrors: Option<Arc<Mirrors>>,
    views: views::Store,
    declared_layers: BTreeMap<usize, Read>,
    layers_awake: Vec<bool>,
    for_the_makers: BTreeMap<usize, Read>,
    layers: Vec<Box<dyn Layer>>,
    worker: Worker,
    composing: bool,
    pending: VecDeque<Arc<Step>>,
    taken: Vec<Arc<Step>>,
    view_later: Option<View>,
    sizes: (Vec<(TableId, usize)>, Vec<(&'static str, usize)>),
    clear: Vec<bool>,
    shown: Vec<Option<Hour>>,
    applied: Option<Hour>,
    drawn: Option<Hour>,
    oldest: Option<Hour>,
    untimed: (Option<Hour>, Option<Hour>),
    last: Frame,
    frame: u64,
    failure: Option<String>,
    branch: Option<replay_data::BranchId>,
    resets_taken: u64,
    resets_shown: u64,
    mode: Option<viewer_map_modes::map_mode::MapMode>,
    mode_settled: Vec<bool>,
    view_shown: View,
    handed_viewer: Option<(viewer_foundation::perspective::Choice, u32)>,
    switched: Option<Instant>,
    stepped: bool,
    #[cfg(any(test, feature = "test-support"))]
    settles_held: Arc<AtomicBool>,
    declared_each: Vec<LayerDeclared>,
    placed: Vec<Option<u64>>,
    sight: Option<Arc<P::Sight>>,
    overlay: Option<Box<dyn Overlaying<P>>>,
    vision: shown_vision::ShownVision,
    combat: combat::Combat<P::Combat>,
    combat_whole: Option<Result<(), String>>,
    compose_ns: Vec<u64>,
    epoch: u64,
    cancel: Arc<AtomicBool>,
    superseded: bool,
    demand: u64,
    landed: u64,
    whole: bool,
    fresh: Vec<bool>,
    reset_shown: bool,
    judged_shown: Vec<Option<Hour>>,
    judged_resets: u64,
    offered: Vec<Option<Hour>>,
    resteps: Vec<u64>,
    standing: standing::Standings,
    waited: Waited,
    doing: Arc<Doing>,
    composed: Arc<AtomicU64>,
}

fn offer(applied: &[Arc<Step>], later: &[Arc<Step>], epoch: u64, reads: &BTreeSet<usize>, offered: &mut Option<Hour>, out: &mut Vec<Arc<Step>>) {
    out.clear();
    for step in applied {
        if step.first() > step.last() {
            out.push(Arc::clone(step));
        } else if offered.is_none_or(|offered| step.last() > offered) {
            *offered = Some(step.last());
            out.push(Arc::clone(step));
        }
    }
    for step in later {
        if step.first() > step.last() || step.epoch() != epoch || step.reset() || step.tables().iter().any(|changes| (changes.reset() || changes.renumber().is_some()) && reads.contains(&changes.table())) {
            break;
        }
        if offered.is_none_or(|offered| step.last() > offered) {
            *offered = Some(step.last());
            out.push(Arc::clone(step));
        }
    }
}

impl MapSide<NoProducts> {
    pub fn new(entry: Arc<Entry>, layers: Vec<Box<dyn Layer>>, view: View) -> io::Result<Self> {
        Self::with_products(entry, layers, view)
    }
}

impl<P: PlaybackProducts> MapSide<P> {
    pub fn lay_overlay_again(&mut self) -> Result<viewer_runtime::landed::Landed<P::OverlayMade>, String> {
        let until = Instant::now() + HANDS_BACK;
        while !self.ask_overlay_step(true, true, Arc::default()) {
            if Instant::now() >= until {
                let loading = self.overlay.as_deref().map(Overlaying::loading).unwrap_or_default().join("; ");
                return Err(format!(
                    "the overlay's maker was not asked for hour {:?}: composing {}, superseded {}, branch {:?}, an overlay {}{}",
                    self.applied,
                    self.composing,
                    self.superseded,
                    self.branch,
                    self.overlay.is_some(),
                    if loading.is_empty() { String::new() } else { format!("; {loading}") },
                ));
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        self.join_overlay(Settling::Whole);
        let hour = self.applied;
        self.take_overlay().ok_or_else(|| {
            let overlay = self.overlay.as_deref().expect("asked above");
            format!(
                "the overlay's maker laid nothing out for hour {hour:?}: it stands at {:?}, missing {:?}{}",
                overlay.hour(),
                overlay.missed(),
                match overlay.loading().join("; ") {
                    loading if loading.is_empty() => String::new(),
                    loading => format!("; {loading}"),
                },
            )
        })
    }

    pub fn with_products(entry: Arc<Entry>, mut layers: Vec<Box<dyn Layer>>, view: View) -> io::Result<Self> {
        let names: Vec<_> = layers.iter().map(|layer| layer.name()).collect();
        let counter_at = counters_at(&names);
        let views = views::Store::for_map(&entry);
        for layer in &mut layers {
            layer.views(&views)?;
        }
        let Declaring { union: declared, reads, upstream, each } = declare(&entry, &mut layers, &view);
        let placed = layers.iter_mut().map(|layer| layer.placed(&view)).collect();
        let declared_layers = declared;
        let for_the_makers = for_the_makers(&entry);
        let declared = merged(&declared_layers, &for_the_makers);
        let reader = entry.playhead.declare(to_declared(&declared), Some(mirror::rebuild_map(Arc::clone(&entry.pages))))?;
        let consumer = entry.playhead.consumer(reader);
        let reads_len = reads.len();
        let timed = layers.iter().any(|layer| layer.timed());
        if timed {
            for (at, layer) in layers.iter().enumerate() {
                let read_by_others = upstream.iter().enumerate().any(|(other, uppers)| other != at && uppers.contains(&at));
                if layer.timed() && (!upstream[at].is_empty() || read_by_others) {
                    return Err(io::Error::other(format!("The map's layer {} is timed and reads or is read by another layer", layer.name())));
                }
            }
        }
        entry.maps.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            entry,
            reader,
            consumer,
            view,
            declared,
            declared_each: each,
            placed,
            ready: vec![false; reads.len()],
            recompose: vec![false; reads.len()],
            upstream,
            reads,
            names,
            counter_at,
            mirrors: Some(Arc::default()),
            views,
            layers_awake: awake_mask(&declared_layers),
            declared_layers,
            for_the_makers,
            layers,
            worker: Worker::new()?,
            composing: false,
            pending: VecDeque::new(),
            taken: Vec::new(),
            view_later: None,
            sizes: Default::default(),
            clear: Vec::new(),
            shown: Vec::new(),
            applied: None,
            drawn: None,
            oldest: None,
            untimed: (None, None),
            last: Frame::default(),
            frame: 0,
            failure: None,
            branch: None,
            resets_taken: 0,
            resets_shown: 0,
            mode: None,
            mode_settled: Vec::new(),
            view_shown: View::default(),
            handed_viewer: None,
            switched: None,
            stepped: false,
            #[cfg(any(test, feature = "test-support"))]
            settles_held: Arc::default(),
            sight: None,
            overlay: None,
            vision: shown_vision::ShownVision::default(),
            combat: combat::Combat::default(),
            combat_whole: None,
            compose_ns: Vec::new(),
            epoch: 0,
            cancel: Arc::default(),
            superseded: false,
            demand: 0,
            landed: 0,
            whole: false,
            fresh: Vec::new(),
            reset_shown: false,
            judged_shown: Vec::new(),
            judged_resets: 0,
            offered: Vec::new(),
            resteps: Vec::new(),
            standing: standing::Standings::new(reads_len),
            waited: Waited::default(),
            doing: Arc::default(),
            composed: Arc::default(),
        })
    }

    pub fn take_installs(&mut self) -> Vec<Box<dyn Installs>> {
        self.layers.iter_mut().filter_map(|layer| layer.installs()).collect()
    }

    pub fn doing(&self) -> &Arc<Doing> {
        &self.doing
    }

    pub fn composed(&self) -> &Arc<AtomicU64> {
        &self.composed
    }

    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    pub fn entry(&self) -> &Arc<Entry> {
        &self.entry
    }

    pub fn set_sight(&mut self, content: Arc<P::Sight>) {
        self.sight = Some(content);
    }

    pub fn sight(&self) -> Option<&Arc<P::Sight>> {
        self.sight.as_ref()
    }

    pub fn set_overlay(&mut self, overlay: Box<dyn Overlaying<P>>) {
        self.overlay = Some(overlay);
    }

    pub fn overlay(&self) -> Option<&dyn Overlaying<P>> {
        self.overlay.as_deref()
    }

    pub fn vision(&self) -> Result<viewer_runtime::landed::Landed<Arc<viewer_foundation::perspective::Vision>>, String> {
        self.vision.kept_for(&self.view_shown.perspective)
    }

    pub fn view_shown(&self) -> &View {
        &self.view_shown
    }

    pub fn ask_overlay(&mut self, ask: P::OverlayAsk) {
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.ask(ask);
        }
    }

    pub fn take_overlay(&mut self) -> Option<viewer_runtime::landed::Landed<P::OverlayMade>> {
        let made = self.overlay.as_deref_mut().and_then(Overlaying::take);
        let made = made.filter(|_| self.vision_refused().is_none());
        if let Some(made) = &made {
            self.handed_viewer = Some((self.view_shown.perspective.clone(), P::overlay_viewer(made)));
        }
        made
    }

    pub fn ask_combat(&mut self, asking: Option<P::Combat>) {
        self.combat.ask(asking);
    }

    pub fn take_combat(&mut self) -> Option<viewer_runtime::landed::Landed<<P::Combat as combat::Asking>::Made>> {
        self.combat.take()
    }

    pub fn combat_loading(&self) -> &[String] {
        self.combat.loading()
    }

    fn engaged(&self) -> Option<Arc<viewer_engagements::engagement_history::History>> {
        self.layer_parts().engaged()
    }

    fn ask_overlay_step(&mut self, whole: bool, lay: bool, moved: Arc<viewer_unit_reads::counter_move::MovedHours>) -> bool {
        if self.overlay.is_none() || self.composing || self.superseded {
            return false;
        }
        let layers = LayerParts {
            names: &self.names,
            reads: &self.reads,
            ready: &self.ready,
            mirrors: self.mirrors.as_deref(),
            layers: &self.layers,
            composing: self.composing,
            superseded: self.superseded,
        };
        let step = OverlayStep {
            layers,
            views: &self.views,
            applied: self.applied,
            branch: self.branch,
            perspective: &self.view.perspective,
            vision: &mut self.vision,
        };
        self.overlay.as_deref_mut().expect("held above").ask_step(step, whole, lay, moved)
    }

    fn moved_before(&self, step: &Step, into: &mut viewer_unit_reads::counter_move::MovedHours) {
        if let Some(mirrors) = self.mirrors.as_deref() {
            for (at, layer) in self.layers.iter().enumerate() {
                if self.reads[at].is_ok() && self.ready[at] {
                    layer.moved_at(step, mirrors, into);
                }
            }
        }
    }

    fn join_overlay(&mut self, settling: Settling) -> bool {
        let at = self.counter_at.unwrap_or(0);
        self.overlay.as_deref_mut().is_some_and(|overlay| overlay.join(&self.layers[at..], &self.view.perspective, self.branch, settling))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn hand_viewer(&mut self, choice: viewer_foundation::perspective::Choice, ours: u32) {
        self.handed_viewer = Some((choice, ours));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn hold_settles(&self) -> SettlesHeld {
        self.settles_held.store(true, Ordering::Release);
        SettlesHeld(Arc::clone(&self.settles_held))
    }

    pub fn anchor(&self, hour: Hour, playing: bool, speed: f64, sought: bool) {
        self.entry.anchor(hour, playing, speed, sought);
    }

    pub fn head(&self, now: Instant) -> Hour {
        self.entry.playhead.head(now)
    }

    pub fn head_from(&self, at: Hour, ahead: std::time::Duration) -> Option<Hour> {
        self.entry.playhead.head_from(at, ahead)
    }

    pub fn reaches(&self, head: Hour) -> bool {
        self.pending.iter().any(|step| step.last() <= head) || self.consumer.reaches(head) != Some(false)
    }

    pub fn readers(&self) -> Vec<(TableId, usize, bool, Vec<&'static str>)> {
        self.declared
            .iter()
            .map(|(&table, read)| {
                let layers = self.names.iter().zip(&self.reads).filter(|(_, reads)| reads.as_ref().is_ok_and(|tables| tables.contains(&table))).map(|(name, _)| *name).collect();
                (TableId::ALL[table], read.columns.len(), read.keyed, layers)
            })
            .collect()
    }

    pub fn refusals(&self) -> impl Iterator<Item = (&'static str, &str)> + '_ {
        self.names.iter().zip(&self.reads).filter_map(|(name, reads)| reads.as_ref().err().map(|why| (*name, why.as_str())))
    }

    pub fn set_view(&mut self, view: View) -> io::Result<()> {
        if view == self.view {
            return Ok(());
        }
        if self.composing || self.superseded {
            self.view_later = Some(view);
            return Ok(());
        }
        if view.same_declaration(&self.view) && {
            let _lap = replay_frame::cpu::scope("side.placed", Some("other"));
            self.place(&view)
        } {
            return Ok(());
        }
        let _lap = replay_frame::cpu::scope("side.declare", Some("other"));
        let Declaring { union: declared, reads, upstream, each } = declare(&self.entry, &mut self.layers, &view);
        if view.map_mode != self.view.map_mode {
            self.mode_settled.clear();
        }
        if view.perspective != self.view.perspective {
            if let Some(at) = self.names.iter().position(|name| *name == "vision").filter(|&at| self.reads[at].is_ok()) {
                (self.ready[at], self.recompose[at]) = (false, true);
            }
            if let Some(overlay) = self.overlay.as_deref_mut() {
                overlay.reset();
            }
            self.switched = Some(Instant::now());
        }
        self.view = view;
        for (at, read) in reads.iter().enumerate() {
            if read.is_ok() && self.reads[at].is_err() {
                self.ready[at] = false;
                self.recompose[at] = true;
            }
        }
        self.reads = reads;
        self.upstream = upstream;
        self.declared_each = each;
        let placed_at = &self.view;
        self.placed = self.layers.iter_mut().map(|layer| layer.placed(placed_at)).collect();
        if let Some(mirrors) = self.mirrors.as_deref() {
            for (at, layer) in self.layers.iter_mut().enumerate() {
                if self.reads[at].is_ok() && self.ready[at] {
                    layer.viewed(&self.view, mirrors);
                }
            }
        }
        let moved: BTreeSet<usize> = declared.iter().filter(|(table, read)| self.declared_layers.get(table) != Some(read)).map(|(&table, _)| table).chain(self.declared_layers.keys().filter(|table| !declared.contains_key(table)).copied()).collect();
        self.declared_layers = declared;
        self.layers_awake = awake_mask(&self.declared_layers);
        self.remirror(&moved)
    }

    fn remirror(&mut self, moved: &BTreeSet<usize>) -> io::Result<()> {
        for at in 0..self.reads.len() {
            if self.reads[at].as_ref().is_ok_and(|tables| tables.iter().any(|table| moved.contains(table))) {
                (self.ready[at], self.recompose[at]) = (false, true);
            }
        }
        let declared = merged(&self.declared_layers, &self.for_the_makers);
        if declared == self.declared {
            return Ok(());
        }
        self.views.settled();
        if let Some(mirrors) = self.mirrors.as_mut().and_then(Arc::get_mut) {
            for table in self.declared.keys().filter(|table| !declared.contains_key(table)) {
                if let Some(old) = mirrors.remove(*table) {
                    self.worker.drop_later(old);
                }
            }
        }
        let changed: BTreeSet<usize> = declared.iter().filter(|(table, read)| self.declared.get(table) != Some(read)).map(|(&table, _)| table).collect();
        for (at, reads) in self.reads.iter().enumerate() {
            if reads.as_ref().is_ok_and(|tables| tables.iter().any(|table| changed.contains(table))) {
                self.ready[at] = false;
            }
        }
        self.declared = declared;
        self.entry.playhead.redeclare(self.reader, to_declared(&self.declared))
    }

    fn place(&mut self, view: &View) -> bool {
        let kinds = self.entry.kinds();
        for (at, layer) in self.layers.iter_mut().enumerate() {
            match layer.placed(view) {
                Some(key) => {
                    if self.placed.get(at) != Some(&Some(key)) {
                        return false;
                    }
                }
                None => {
                    let mut inputs = Inputs::new(kinds, layer.name());
                    if layer.inputs(view, &mut inputs).is_err() {
                        return false;
                    }
                    let after = inputs.upstream().to_vec();
                    if self.declared_each.get(at) != Some(&Some((inputs.into_tables(), after))) {
                        return false;
                    }
                }
            }
        }
        self.view = view.clone();
        if let Some(mirrors) = self.mirrors.as_deref() {
            for (at, layer) in self.layers.iter_mut().enumerate() {
                if self.reads[at].is_ok() && self.ready[at] {
                    layer.viewed(&self.view, mirrors);
                }
            }
        }
        true
    }

    pub fn frame(&mut self, head: Hour) -> io::Result<Frame> {
        self.frame_within(head, crate::layers::Settling::step(), true)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn frame_waiting(&mut self, head: Hour) -> io::Result<Frame> {
        self.views.settled();
        let was = self.consumer.set_blocking(true);
        let result = self.frame_step(head, crate::layers::Settling::step(), true, true);
        self.consumer.set_blocking(was);
        result
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn frame_synced(&mut self, head: Hour) -> io::Result<Frame> {
        self.views.settled();
        let was = self.consumer.set_blocking(true);
        let result = self.frame_within(head, crate::layers::Settling::step(), true);
        self.consumer.set_blocking(was);
        result
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn wait_step_after(&self, before: (Option<Hour>, u64), cap: Hour) -> Result<bool, String> {
        if self.pending.iter().any(|step| step.last() <= cap) {
            return Ok(true);
        }
        self.consumer.wait_step_after(before, cap)
    }

    pub fn frame_still(&mut self, head: Hour) -> io::Result<Frame> {
        self.frame_within(head, crate::layers::Settling::still(), true)
    }

    fn frame_within(&mut self, head: Hour, settling: crate::layers::Settling, lay: bool) -> io::Result<Frame> {
        self.frame_step(head, settling, lay, false)
    }

    fn frame_step(&mut self, head: Hour, settling: crate::layers::Settling, lay: bool, wait_compose: bool) -> io::Result<Frame> {
        self.frame += 1;
        (self.demand, self.landed) = (self.views.demand(), viewer_runtime::landed::count());
        self.vision.take_made();
        let stepping = Instant::now();
        let doing = Arc::clone(&self.doing);
        let mut watch = Watch::new(&doing);
        timeline::step();
        let mut frame = Frame { made: self.frame, head, waited: self.waited, layers: vec![LayerFrame::default(); self.names.len()], ..Frame::default() };
        self.views.settled();
        timeline::views_settled();
        watch.lap(&mut frame.stages, 0);
        let take_lap = replay_frame::cpu::scope("side.take", Some("other"));
        let playhead = &self.entry.playhead;
        self.consumer.advance(head);
        if self.composing {
            if playhead.epoch() != self.epoch {
                self.cancel.store(true, Ordering::Release);
            }
            let received = if wait_compose {
                self.worker.done.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                self.worker.done.try_recv()
            };
            match received {
                Ok(away) if away.cancel.load(Ordering::Acquire) => {
                    frame.superseded = true;
                    self.take_home(*away.into_inner());
                }
                Ok(away) => {
                    frame.swapped = Some(away.results.iter().map(|(_, ns, _)| *ns).collect());
                    self.swap_in(*away.into_inner())?;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(io::Error::other("The playhead host's worker stopped")),
            }
        }
        let swapped = frame.swapped.is_some();
        if !swapped && (!self.composing || self.entry.maps.load(Ordering::Acquire) > 1) {
            let (started, cycles) = (Instant::now(), replay_frame::cpu::thread_cycles());
            self.consumer.take(head, &mut self.taken);
            frame.take_ns += started.elapsed().as_nanos() as u64;
            frame.take_cycles += replay_frame::cpu::thread_cycles().saturating_sub(cycles);
        }
        let mut steps: VecDeque<Arc<Step>> = VecDeque::new();
        if !swapped {
            let epoch = self.entry.playhead.epoch();
            self.pending.retain(|step| step.epoch() == epoch);
            steps = std::mem::take(&mut self.pending);
            steps.extend(self.taken.drain(..));
        }
        drop(take_lap);
        watch.lap(&mut frame.stages, 1);
        let apply_lap = replay_frame::cpu::scope("side.apply", Some("other"));
        let mut compose = vec![false; self.layers.len()];
        let mut applied = Vec::with_capacity(steps.len());
        let (resets_before, applied_before) = (self.resets_taken, self.applied);
        let (mut waiting, mut after_reset, mut crossing) = (self.composing, false, false);
        let mut moved = viewer_unit_reads::counter_move::MovedHours::default();
        while let Some(step) = steps.pop_front() {
            waiting |= step.last() > head;
            if waiting {
                self.pending.push_back(step);
                continue;
            }
            let reset = step.reset();
            if self.overlay.is_some() {
                self.moved_before(&step, &mut moved);
            }
            crossing |= self.take_step(&step, &mut frame, &mut compose);
            applied.push(step);
            (waiting, after_reset) = (waiting || reset, after_reset || reset);
        }
        if self.superseded && self.resets_taken == resets_before {
            compose.fill(false);
        } else if !self.composing {
            for (wanted, again) in compose.iter_mut().zip(&mut self.recompose) {
                *wanted |= std::mem::take(again);
            }
            if self.applied.is_some() {
                for (at, wanted) in compose.iter_mut().enumerate() {
                    *wanted |= !self.ready[at] && self.reads[at].as_ref().is_ok_and(BTreeSet::is_empty);
                }
            }
        }
        let home = |compose: &mut [bool], mirrors: Option<&Mirrors>| {
            if let Some(mirrors) = mirrors {
                for (wanted, reads) in compose.iter_mut().zip(&self.reads) {
                    *wanted &= reads.as_ref().is_ok_and(|tables| tables.iter().all(|&table| mirrors.get(TableId::ALL[table]).is_some()));
                }
            }
        };
        home(&mut compose[..], self.mirrors.as_deref());
        for at in 0..compose.len() {
            if self.reads[at].is_ok() && self.upstream[at].iter().any(|&before| compose[before]) {
                compose[at] = true;
            }
        }
        home(&mut compose[..], self.mirrors.as_deref());
        let composed_here = crossing && !after_reset && !self.composing && !self.superseded && compose.contains(&true);
        if composed_here {
            self.compose_here(&compose, &mut frame);
        }
        drop(apply_lap);
        watch.lap(&mut frame.stages, 2);
        timeline::applied();
        timeline::settling();
        if self.composing || self.superseded || self.resets_taken != resets_before {
            self.standing.reset();
        } else {
            self.standing.step(self.applied, Instant::now());
        }
        let settles_lap = replay_frame::cpu::scope("side.settles", Some("other"));
        if !self.composing {
            let composing = compose.iter().any(|&wanted| wanted) && !composed_here;
            let overlay_at = (!composing && self.overlay.is_some()).then(|| self.counter_at.unwrap_or(0));
            if overlay_at.is_some() {
                if viewer_runtime::view_record::on() {
                    let (movers, hours) = moved.counts();
                    viewer_runtime::view_record::add("moved", "movers", movers as u64);
                    viewer_runtime::view_record::add("moved", "hours", hours as u64);
                }
                self.ask_overlay_step(settling.strict(), lay, Arc::new(std::mem::take(&mut moved)));
            }
            if !composing && let Some(hour) = self.applied {
                if lay {
                    let engaged = self.engaged();
                    self.combat.ask_step(&self.views, Some(hour), self.branch, self.epoch, engaged);
                }
                let _views = replay_frame::cpu::scope("side.views", Some("other"));
                let (store, held) = (&mut self.views, self.mirrors.as_ref().expect("the mirrors are home"));
                store.post(held, hour, self.branch.unwrap_or_default(), self.epoch, false);
            }
            let mirrors = self.mirrors.as_deref().expect("the mirrors are home while not composing");
            self.clear.resize(self.layers.len(), false);
            for (layer, clear) in self.layers.iter_mut().zip(&mut self.clear) {
                if std::mem::take(clear) {
                    layer.clear_changed();
                }
            }
            let mut settled = vec![false; self.layers.len()];
            self.offered.resize(self.layers.len(), None);
            self.resteps.resize(self.layers.len(), 0);
            let (mut lent, mut handed): (Option<Vec<Arc<Step>>>, Vec<Arc<Step>>) = (None, Vec::new());
            let mut counted: Vec<(usize, usize)> = Vec::with_capacity(self.layers.len());
            let mut mark = replay_frame::cpu::thread_cycles();
            let applied_now = self.applied;
            let composed = self.composed.load(Ordering::Acquire);
            let mut joins = vec![false; self.layers.len()];
            let mut dated = vec![false; self.layers.len()];
            for at in 0..self.layers.len() {
                if overlay_at == Some(at) {
                    if let Some(overlay) = self.overlay.as_deref_mut() {
                        overlay.join(&self.layers[at..], &self.view.perspective, self.branch, settling);
                    }
                }
                if !(self.reads[at].is_ok() && self.ready[at]) || compose[at] || self.superseded {
                    if self.reads[at].is_ok() && (!self.ready[at] || compose[at]) {
                        viewer_runtime::view_record::add("layers", "frames_not_ready", 1);
                    }
                    continue;
                }
                let bound = self.layers[at].behind_bound();
                let (before, rest) = self.layers.split_at_mut(at);
                let (layer, upstream) = (&mut rest[0], Upstream::new(before));
                let part = &mut frame.layers[at];
                let timed = layer.timed();
                let started = Instant::now();
                for step in applied.iter() {
                    layer.apply_with(step, mirrors, &upstream);
                }
                part.apply_ns += started.elapsed().as_nanos() as u64;
                if !applied.is_empty() {
                    let now = replay_frame::cpu::thread_cycles();
                    part.apply_cycles += now.saturating_sub(std::mem::replace(&mut mark, now));
                }
                let fed = layer.wants_ahead();
                if fed && self.applied.is_some() {
                    let started = Instant::now();
                    let lent = lent.get_or_insert_with(|| {
                        let mut steps: Vec<Arc<Step>> = self.pending.iter().cloned().collect();
                        self.consumer.ahead(&mut steps);
                        steps
                    });
                    let restep = applied.iter().chain(lent.iter()).map(|step| step.restep()).max().unwrap_or(0);
                    if restep > self.resteps[at] {
                        if self.offered[at].is_some_and(|offered| self.applied.is_none_or(|applied| offered > applied)) {
                            layer.void_ahead(mirrors, applied_before);
                            frame.voided += 1;
                            self.offered[at] = applied_before;
                        }
                        self.resteps[at] = restep;
                    }
                    let reads = self.reads[at].as_ref().expect("a drawn layer reads its tables");
                    offer(&applied, lent, self.epoch, reads, &mut self.offered[at], &mut handed);
                    if !handed.is_empty() {
                        layer.ahead(&handed);
                        handed.clear();
                    }
                    part.ahead_ns = started.elapsed().as_nanos() as u64;
                }
                let held = self.upstream[at].iter().any(|&upper| !self.ready[upper]);
                part.dirty = layer.carried().entities;
                #[cfg(any(test, feature = "test-support"))]
                let held = held || self.settles_held.load(Ordering::Acquire);
                if timed {
                    settled[at] = true;
                    let started = Instant::now();
                    layer.settle_with(mirrors, &upstream, settling)?;
                    dated[at] = applied_now.is_some();
                    part.settle_ns += started.elapsed().as_nanos() as u64;
                    let now = replay_frame::cpu::thread_cycles();
                    part.settle_cycles = now.saturating_sub(std::mem::replace(&mut mark, now));
                } else if !composing && !held {
                    joins[at] = layer.ask(mirrors, &upstream)? || self.upstream[at].iter().any(|&upper| joins[upper]);
                    if !joins[at] {
                        settled[at] = true;
                        let started = Instant::now();
                        layer.settle_with(mirrors, &upstream, settling)?;
                        part.settle_ns = started.elapsed().as_nanos() as u64;
                        let now = replay_frame::cpu::thread_cycles();
                        part.settle_cycles = now.saturating_sub(std::mem::replace(&mut mark, now));
                    }
                }
                if self.names[at] == "vision" {
                    let shown = layer.any().downcast_ref::<crate::vision::Vision>();
                    drop(self.vision.shown(shown, applied_now, true, false));
                }
                part.ready = true;
                if !joins[at] {
                    counted.push((at, layer.carried().entities));
                    if fed && !applied.is_empty() {
                        part.ready_at_hour = Some(layer.published() == crate::layers::Published::Current);
                    }
                    let published = layer.published();
                    self.standing.settled(at, self.names[at], published, Instant::now(), bound, settling)?;
                }
            }
            if let Some(hour) = applied_now.filter(|_| dated.contains(&true)) {
                hand_naval(&self.names, &mut self.layers, &self.reads, &self.ready);
                for at in (0..self.layers.len()).filter(|&at| dated[at]) {
                    let started = Instant::now();
                    self.layers[at].timed_at(hour, composed)?;
                    frame.layers[at].settle_ns += started.elapsed().as_nanos() as u64;
                }
            }
            timeline::joining();
            watch.lap(&mut frame.stages, 3);
            for at in 0..self.layers.len() {
                if !joins[at] {
                    continue;
                }
                let bound = self.layers[at].behind_bound();
                let (before, rest) = self.layers.split_at_mut(at);
                let (layer, upstream) = (&mut rest[0], Upstream::new(before));
                let part = &mut frame.layers[at];
                settled[at] = true;
                let started = Instant::now();
                layer.join(mirrors, &upstream, settling)?;
                part.settle_ns = started.elapsed().as_nanos() as u64;
                let now = replay_frame::cpu::thread_cycles();
                part.settle_cycles = now.saturating_sub(std::mem::replace(&mut mark, now));
                counted.push((at, layer.carried().entities));
                if layer.wants_ahead() && !applied.is_empty() {
                    part.ready_at_hour = Some(layer.published() == crate::layers::Published::Current);
                }
                let published = layer.published();
                self.standing.settled(at, self.names[at], published, Instant::now(), bound, settling)?;
            }
            if !self.stepped && settling.waits() && self.standing.hours_stopped() {
                let drawn: Vec<(usize, &'static str)> = (0..self.layers.len()).filter(|&at| self.reads[at].is_ok() && self.ready[at] && !compose[at]).map(|at| (at, self.names[at])).collect();
                self.standing.stopped_whole(&drawn)?;
            }
            watch.lap(&mut frame.stages, 4);
            {
                let _in_view = replay_frame::cpu::scope("side.in-view", Some("other"));
                let content = self.sight.clone();
                let sight = crate::layers::Sight::of(content.as_deref().map(P::sight), &self.view);
                for (at, left) in counted {
                    let part = &mut frame.layers[at];
                    part.scope = self.layers[at].scope(&sight).or_else(|| settled[at].then(|| crate::layers::Scope { settled: part.dirty.saturating_sub(left) as u64, in_view: None }));
                }
            }
            watch.lap(&mut frame.stages, 5);
            {
                let _staging = replay_frame::cpu::scope("side.stage", Some("other"));
                for at in 0..self.layers.len() {
                    if !(self.reads[at].is_ok() && self.ready[at]) || compose[at] || self.superseded {
                        continue;
                    }
                    let started = Instant::now();
                    self.layers[at].stage()?;
                    frame.layers[at].stage_ns = started.elapsed().as_nanos() as u64;
                    let now = replay_frame::cpu::thread_cycles();
                    frame.layers[at].stage_cycles = now.saturating_sub(std::mem::replace(&mut mark, now));
                }
            }
            watch.lap(&mut frame.stages, 6);
            for upper in 0..self.layers.len() {
                let drawn = |at: usize| self.reads[at].is_ok() && self.ready[at];
                self.clear[upper] = (0..self.layers.len()).filter(|&at| drawn(at) && self.upstream[at].contains(&upper)).all(|at| settled[at]);
            }
            if composing {
                frame.resets += 1;
                self.send_away(compose)?;
            }
        }
        drop(settles_lap);
        let _shown_lap = replay_frame::cpu::scope("side.shown", Some("other"));
        let playhead = &self.entry.playhead;
        if !self.composing
            && !self.superseded
            && !swapped
            && !self.pending.iter().any(|step| step.last() <= head)
            && ((self.stepped && self.applied.is_some()) || self.entry.paused())
            && playhead.holding().is_none()
            && (self.applied.is_some_and(|applied| applied > head)
                || (self.applied.is_none_or(|applied| applied < head)
                    && playhead.newest().is_some_and(|newest| newest > head)
                    && self.consumer.reaches(head) == Some(false)))
        {
            self.entry.exact(head);
            frame.exact = true;
        }
        frame.composing = self.composing || self.superseded;
        if !frame.composing {
            self.resets_shown = self.resets_taken;
            self.view_shown.clone_from(&self.view);
            self.update_shown();
        }
        for ((part, shown), fresh) in frame.layers.iter_mut().zip(&self.shown).zip(&self.fresh) {
            (part.shown, part.reset_shown) = (*shown, *fresh);
        }
        frame.reset_shown = self.reset_shown;
        (frame.drawn, frame.oldest) = (self.drawn, self.oldest);
        frame.side_ns = stepping.elapsed().as_nanos() as u64;
        watch.lap(&mut frame.stages, 7);
        frame.side_cycles = frame.stages.cycles.iter().sum();
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.stepped();
        }
        self.combat.join(settling);
        timeline::close(frame.composing);
        self.last = frame.clone();
        Ok(frame)
    }

    fn update_shown(&mut self) {
        use crate::layers::Published;
        self.shown.resize(self.layers.len(), None);
        self.fresh.resize(self.layers.len(), false);
        self.mode_settled.resize(self.layers.len(), false);
        let ready = self.reads.iter().zip(&self.ready).all(|(reads, &ready)| ready || reads.is_err());
        let (mut whole, mut untimed_whole) = (ready, ready);
        let mut mode_settled = true;
        let (mut oldest, mut untimed_oldest): (Option<Hour>, Option<Hour>) = (None, None);
        let (mut every, mut untimed_every): (Option<Hour>, Option<Hour>) = (None, None);
        let (mut drawn, mut untimed_drawn) = (0, 0);
        let mut fresh = whole;
        for (at, layer) in self.layers.iter().enumerate() {
            if !(self.reads[at].is_ok() && self.ready[at]) {
                continue;
            }
            if layer.uploaded() {
                (self.shown[at], self.fresh[at]) = match layer.published() {
                    Published::Current => (layer.shown_through().or(self.applied), true),
                    Published::At(hour) | Published::Cadence(hour) => (Some(hour), true),
                    Published::Nothing => (None, false),
                };
            }
            let timed = layer.timed();
            if let Some(hour) = self.shown[at] {
                every = Some(every.map_or(hour, |held| held.min(hour)));
                if !timed {
                    untimed_every = Some(untimed_every.map_or(hour, |held| held.min(hour)));
                }
            }
            if matches!(layer.published(), Published::Cadence(_)) {
                continue;
            }
            self.mode_settled[at] |= layer.uploaded() && layer.carried().entities == 0 && !matches!(layer.published(), Published::Nothing);
            mode_settled &= self.mode_settled[at];
            fresh &= self.fresh[at];
            drawn += 1;
            untimed_drawn += u32::from(!timed);
            match (self.shown[at], timed) {
                (Some(hour), false) => untimed_oldest = Some(untimed_oldest.map_or(hour, |held| held.min(hour))),
                (None, false) => untimed_whole = false,
                (Some(_), true) => {}
                (None, true) => {}
            }
            match self.shown[at] {
                Some(hour) => oldest = Some(oldest.map_or(hour, |held| held.min(hour))),
                None => whole = false,
            }
        }
        self.whole = whole;
        if whole {
            self.drawn = if drawn == 0 { self.applied } else { oldest };
            self.oldest = match (self.drawn, every) {
                (Some(drawn), Some(every)) => Some(drawn.min(every)),
                (drawn, every) => drawn.or(every),
            };
            if mode_settled && self.drawn.is_some() {
                self.mode = Some(self.view.map_mode.clone());
            }
        }
        if untimed_whole {
            let shown = if untimed_drawn == 0 { self.applied } else { untimed_oldest };
            self.untimed = (
                shown,
                match (shown, untimed_every) {
                    (Some(shown), Some(every)) => Some(shown.min(every)),
                    (shown, every) => shown.or(every),
                },
            );
        }
        self.reset_shown = fresh;
    }

    pub fn untimed_hours(&self) -> (Option<Hour>, Option<Hour>) {
        self.untimed
    }

    pub fn reached(&mut self, took: Option<std::time::Duration>) {
        self.last.reached_ns = took.map_or(0, |took| took.as_nanos() as u64);
    }

    pub fn waited(&mut self, waited: Waited) {
        self.waited = waited;
    }


    pub fn last_frame(&self) -> &Frame {
        &self.last
    }

    pub fn layer_states(&self) -> Vec<(&'static str, bool, bool)> {
        self.names.iter().enumerate().map(|(at, &name)| (name, self.reads[at].is_ok() && self.ready[at], self.last.layers.get(at).is_some_and(|part| part.upload_carried))).collect()
    }

    pub fn caught_up(&self) -> Vec<CaughtUp> {
        use crate::layers::Published;
        (0..self.layers.len())
            .filter(|&at| self.reads[at].is_ok() && self.ready[at])
            .map(|at| {
                let layer = &self.layers[at];
                let published = layer.published();
                let current = match published {
                    Published::Current => true,
                    Published::At(hour) | Published::Cadence(hour) => self.applied.is_some_and(|applied| hour >= applied),
                    Published::Nothing => false,
                };
                CaughtUp {
                    name: self.names[at],
                    caught: current && layer.uploaded() && layer.carried().entities == 0,
                    withheld: matches!(published, Published::Nothing),
                    viewer: layer.held_viewer(),
                }
            })
            .collect()
    }

    pub fn shown_states(&self) -> String {
        let layers: Vec<String> = (0..self.layers.len())
            .filter(|&at| self.reads[at].is_ok())
            .map(|at| {
                let layer = &self.layers[at];
                format!("{} ready {} shown {:?} {:?} carried {} uploaded {}", self.names[at], self.ready[at], self.shown.get(at).copied().flatten(), layer.published(), layer.carried().entities, layer.uploaded())
            })
            .collect();
        format!("applied {:?}, drawn {:?}, composing {}, superseded {}, last head {}, exact {}; {}", self.applied, self.drawn, self.composing, self.superseded, self.last.head, self.last.exact, layers.join("; "))
    }

    pub fn draws(&self, name: &str) -> bool {
        self.layer_parts().draws(name)
    }

    pub fn catch_up(&mut self, head: Hour) -> io::Result<()> {
        self.stepped = true;
        self.combat_whole = None;
        let started = Instant::now();
        loop {
            let newest = self.entry.playhead.newest().unwrap_or(head);
            self.frame_within(head.min(newest.max(self.applied.unwrap_or(newest))), crate::layers::Settling::step(), false)?;
            if !self.composing && !self.superseded && self.applied == Some(head) && self.ready.iter().zip(&self.reads).all(|(&ready, reads)| ready || reads.is_err()) {
                self.settle_all()?;
                if self.head_products_reached(head) && self.overlay_whole(head)? {
                    self.combat_whole = self.combat_whole();
                    self.update_shown();
                    return Ok(());
                }
            }
            if let Some(error) = self.entry.playhead.error() {
                return Err(io::Error::other(format!("The playhead stopped: {error}")));
            }
            if started.elapsed() > CATCHING_UP {
                return Err(io::Error::new(io::ErrorKind::TimedOut, self.waiting_state(head)));
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }

    pub fn waiting_state(&self, head: Hour) -> String {
        let waiting: Vec<String> = self
            .names
            .iter()
            .enumerate()
            .filter(|(at, _)| !self.ready[*at] && self.reads[*at].is_ok())
            .map(|(at, name)| {
                let missing: Vec<String> = self.reads[at]
                    .iter()
                    .flatten()
                    .filter(|&&table| self.mirrors.as_deref().is_none_or(|mirrors| mirrors.get(TableId::ALL[table]).is_none()))
                    .map(|&table| format!("{:?}", TableId::ALL[table]))
                    .collect();
                match missing.is_empty() {
                    true => (*name).to_owned(),
                    false => format!("{name} (no mirror of {})", missing.join(", ")),
                }
            })
            .collect();
        let overlay = self.overlay.as_deref().map_or_else(String::new, |overlay| format!("the overlay stands at {:?}, missing {:?}, {}; ", overlay.hour(), overlay.missed(), overlay.loading().join("; ")));
        let (report, _, counters) = self.entry.playhead.held();
        format!(
            "Hour {head} was not reached by the map's layers in a minute: {overlay}applied {:?}, newest decoded {:?}, composing {}, not composed {waiting:?}, steps held here {}, ring steps {}, fetches queued {}, jobs running {}, preparing {}, {counters:?}",
            self.applied,
            self.entry.playhead.newest(),
            self.composing,
            self.pending.len() + self.taken.len(),
            report.ring_steps,
            report.fetches_queued,
            report.jobs_running,
            report.preparing,
        )
    }

    fn overlay_whole(&mut self, head: Hour) -> io::Result<bool> {
        if self.overlay.is_none() {
            return Ok(true);
        }
        let until = Instant::now() + HANDS_BACK;
        for lay in [false, true] {
            loop {
                if self.vision_refused().is_some() {
                    return Ok(true);
                }
                self.post_makers();
                let asked = self.ask_overlay_step(true, lay, Arc::default());
                self.post_makers();
                if asked && self.join_overlay(Settling::Whole) {
                    break;
                }
                if Instant::now() >= until {
                    return Ok(false);
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            self.settle_all()?;
        }
        Ok(self.overlay.as_deref().expect("held above").stands_at(head))
    }

    fn combat_whole(&mut self) -> Option<Result<(), String>> {
        loop {
            let (engaged, learned) = (self.engaged(), self.combat.learned());
            if !self.combat.ask_step(&self.views, self.applied, self.branch, self.epoch, engaged) {
                return self.combat.open().then_some(Ok(()));
            }
            self.post_makers();
            let joined = self.combat.join(Settling::Whole);
            if joined.made {
                return Some(Ok(()));
            }
            if joined.answered || self.combat.learned() == learned {
                return Some(Err(self.combat.loading().join("; ")));
            }
        }
    }

    pub fn combat_whole_state(&self) -> Option<&Result<(), String>> {
        self.combat_whole.as_ref()
    }

    fn head_products_reached(&self, head: Hour) -> bool {
        (0..self.layers.len()).filter(|&at| self.reads[at].is_ok() && self.ready[at]).all(|at| self.layers[at].made_at(head))
    }

    fn post_makers(&mut self) {
        if self.composing || self.superseded {
            return;
        }
        let Some(hour) = self.applied else { return };
        let (store, held) = (&mut self.views, self.mirrors.as_ref().expect("the mirrors are home while not composing"));
        store.post(held, hour, self.branch.unwrap_or_default(), self.epoch, true);
    }

    fn vision_layer(&self) -> Option<&crate::vision::Vision> {
        self.layer_parts().vision_layer()
    }

    pub fn viewer(&self) -> Option<u32> {
        self.vision_layer()?.ours()
    }

    pub fn take_overlay_withdrawal(&mut self) -> bool {
        if self.composing || self.superseded {
            return false;
        }
        let viewer = self.viewer();
        let refused = self.vision_refused().is_some();
        let other = self.handed_viewer.as_ref().is_some_and(|(choice, ours)| refused || *choice != self.view_shown.perspective || viewer.is_some_and(|viewer| viewer != *ours));
        if other {
            self.handed_viewer = None;
        }
        other
    }

    pub fn vision_refused(&self) -> Option<String> {
        let vision = self.vision_layer()?;
        vision.made().then(|| vision.refused()).flatten()
    }

    pub fn hurrying(&self) -> bool {
        self.switched.is_some_and(|at| at.elapsed() < HURRY)
    }

    pub fn settle_all(&mut self) -> io::Result<()> {
        self.doing.set(Doing::WHOLE);
        self.post_makers();
        self.views.made();
        if let Some(mirrors) = self.mirrors.as_deref().filter(|_| !self.superseded) {
            for at in 0..self.layers.len() {
                if self.reads[at].is_ok() && self.ready[at] {
                    let (before, rest) = self.layers.split_at_mut(at);
                    rest[0].settle_with(mirrors, &Upstream::new(before), Settling::Whole)?;
                    self.standing.settled_whole(at, rest[0].published());
                }
            }
            let applied = self.applied;
            hand_naval(&self.names, &mut self.layers, &self.reads, &self.ready);
            for at in 0..self.layers.len() {
                if self.reads[at].is_ok() && self.ready[at] {
                    self.layers[at].stage()?;
                    if let (true, Some(hour)) = (self.layers[at].timed(), applied) {
                        let composed = self.composed.load(Ordering::Acquire);
                        self.layers[at].timed_at(hour, composed)?;
                    }
                }
            }
        }
        self.doing.set(Doing::IDLE);
        Ok(())
    }

    pub fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        if self.composing || self.superseded {
            return Ok(());
        }
        let _lap = replay_frame::cpu::scope("frame.refresh.uploads", Some("other"));
        self.upload_with(|layer| layer.upload(gpu))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn upload_each(&mut self, each: impl FnMut(&mut dyn Layer) -> io::Result<()>) -> io::Result<()> {
        self.upload_with(each)
    }

    fn upload_with(&mut self, mut each: impl FnMut(&mut dyn Layer) -> io::Result<()>) -> io::Result<()> {
        let mut mark = replay_frame::cpu::thread_cycles();
        for at in 0..self.layers.len() {
            if !(self.reads[at].is_ok() && self.ready[at]) {
                continue;
            }
            let layer = &mut self.layers[at];
            let started = Instant::now();
            let counted = shader_pipeline::gpu::stats::peek();
            each(&mut **layer)?;
            let spent = started.elapsed();
            let now = replay_frame::cpu::thread_cycles();
            let cycles = now.saturating_sub(std::mem::replace(&mut mark, now));
            let written = |count: shader_pipeline::gpu::stats::Count| shader_pipeline::gpu::stats::peek()[count as usize].saturating_sub(counted[count as usize]);
            if let Some(part) = self.last.layers.get_mut(at) {
                part.upload_ns = spent.as_nanos() as u64;
                part.upload_cycles = cycles;
                part.upload_carried = !layer.uploaded();
                (part.uploads, part.upload_bytes) = (written(shader_pipeline::gpu::stats::Count::Uploads), written(shader_pipeline::gpu::stats::Count::UploadBytes));
                (part.write_ns, part.create_ns) = (written(shader_pipeline::gpu::stats::Count::WriteNs), written(shader_pipeline::gpu::stats::Count::CreateNs));
            }
        }
        self.update_shown();
        self.judge_installs();
        let published: Vec<u8> = self
            .layers
            .iter()
            .map(|layer| match layer.published() {
                crate::layers::Published::Current => 0,
                crate::layers::Published::At(_) => 1,
                crate::layers::Published::Cadence(_) => 2,
                crate::layers::Published::Nothing => 3,
            })
            .collect();
        for (((part, shown), fresh), published) in self.last.layers.iter_mut().zip(&self.shown).zip(&self.fresh).zip(published) {
            (part.shown, part.reset_shown, part.published) = (*shown, *fresh, published);
        }
        (self.last.drawn, self.last.oldest, self.last.reset_shown) = (self.drawn, self.oldest, self.reset_shown);
        Ok(())
    }

    fn judge_installs(&mut self) {
        use viewer_runtime::publication_check as check;
        if !check::on() {
            return;
        }
        for at in 0..self.layers.len() {
            if !(self.reads[at].is_ok() && self.ready[at]) {
                continue;
            }
            check::judged();
            if !self.layers[at].uploaded() {
                check::found(self.names[at], "its install was left for a later frame");
            }
        }
        self.judged_shown.resize(self.layers.len(), None);
        let sought = std::mem::replace(&mut self.judged_resets, self.resets_taken) != self.resets_taken;
        for at in 0..self.layers.len() {
            if !matches!(self.layers[at].published(), crate::layers::Published::Current) {
                continue;
            }
            check::judged();
            let shown = self.shown.get(at).copied().flatten();
            let before = self.judged_shown[at];
            if !sought
                && let (Some(before), Some(shown)) = (before, shown)
                && shown < before
            {
                check::backwards(self.names[at], before, shown);
            }
            self.judged_shown[at] = shown.or(before);
        }
    }

    fn take_step(&mut self, step: &Arc<Step>, frame: &mut Frame, compose: &mut [bool]) -> bool {
        let mut reset_tables = BTreeSet::new();
        let mut judged: Vec<(usize, Box<Mirror>)> = Vec::new();
        if piece::on() && !step.reset() {
            for changes in step.tables().iter().filter(|changes| !changes.reset()) {
                if let Some(state) = self.consumer.state(step, changes.table())
                    && let Ok(rebuilt) = state.downcast::<Mirror>()
                {
                    judged.push((changes.table(), rebuilt));
                }
            }
        }
        let mirrors = self.mirrors.as_mut().and_then(Arc::get_mut).expect("the mirrors are home and the step's views made");
        let mut replaced = false;
        for changes in step.tables() {
            if changes.reset()
                && let Some(state) = self.consumer.state(step, changes.table())
                && let Ok(mut rebuilt) = state.downcast::<Mirror>()
            {
                rebuilt.set_capture(self.entry.capture.get().copied());
                if let Some(old) = mirrors.insert(rebuilt) {
                    self.worker.drop_later(old);
                    replaced = true;
                }
                reset_tables.insert(changes.table());
            }
            if let Some(old) = changes.renumber() {
                let moved = old.iter().enumerate().filter(|&(new, &from)| from as usize != new).count();
                frame.renumbers.push((changes.table(), old.len() as u32, moved as u32));
                reset_tables.insert(changes.table());
                replaced = true;
            }
        }
        let crossing = !step.reset() && replaced;
        frame.reset_tables.extend(reset_tables.iter().copied());
        frame.reset_step |= step.reset();
        let (started, cycles) = (Instant::now(), replay_frame::cpu::thread_cycles());
        mirrors.apply_timed(step, &mut frame.mirror_tables, &mut frame.mirror_split, &mut self.views.noting(&self.layers_awake));
        frame.mirror_ns += started.elapsed().as_nanos() as u64;
        for (table, mut rebuilt) in judged {
            piece::judged();
            rebuilt.set_capture(self.entry.capture.get().copied());
            let id = TableId::ALL[table];
            match mirrors.get(id) {
                None => piece::found(id, step.last(), "the crossing left no mirror of it"),
                Some(mirror) => {
                    if let Some(why) = mirror.differs(&rebuilt, self.entry.kinds().get(table).map_or(&[][..], |kinds| &kinds[..])) {
                        piece::found(id, step.last(), &why);
                    }
                }
            }
        }
        frame.mirror_cycles += replay_frame::cpu::thread_cycles().saturating_sub(cycles);
        frame.steps += 1;
        frame.step_ns += step.built().ns;
        frame.step_most_ns = frame.step_most_ns.max(step.built().ns);
        frame.step_cycles += step.built().cycles;
        frame.cells += step.built().cells;
        frame.rows += step.built().rows;
        if step.first() <= step.last() {
            frame.first = frame.first.filter(|first| *first <= step.first()).or_else(|| Some(step.first()));
        }
        frame.applied = Some(step.last());
        self.applied = Some(step.last());
        self.epoch = step.epoch();
        if step.reset() {
            frame.sought = frame.sought.or(step.built().sought);
            self.resets_taken += 1;
            self.vision.forget();
        }
        for (wanted, reads) in compose.iter_mut().zip(&self.reads) {
            *wanted |= reads.as_ref().is_ok_and(|tables| step.reset() || tables.iter().any(|table| reset_tables.contains(table)));
        }
        crossing
    }

    fn compose_here(&mut self, compose: &[bool], frame: &mut Frame) {
        self.compose_ns.resize(self.layers.len(), 0);
        self.fresh.resize(self.layers.len(), false);
        self.offered.resize(self.layers.len(), None);
        let mut spent = vec![0; self.layers.len()];
        for at in 0..self.layers.len() {
            if !compose[at] {
                continue;
            }
            let mirrors = self.mirrors.as_deref().expect("the mirrors are home while not composing");
            let (before, rest) = self.layers.split_at_mut(at);
            let started = Instant::now();
            let result = rest[0].compose_with(mirrors, &Upstream::new(before));
            let ns = started.elapsed().as_nanos() as u64;
            (self.compose_ns[at], spent[at]) = (ns, ns);
            (self.fresh[at], self.offered[at]) = (false, None);
            match result {
                Err(error) => self.reads[at] = Err(error.to_string()),
                Ok(()) => self.ready[at] = true,
            }
        }
        frame.resets += 1;
        frame.swapped = Some(spent);
    }

    fn send_away(&mut self, compose: Vec<bool>) -> io::Result<()> {
        self.composed.fetch_add(1, Ordering::AcqRel);
        self.views.settled();
        let mirrors = self.mirrors.take().expect("the mirrors are home");
        self.sizes = (mirrors.iter().map(|mirror| (mirror.table(), mirror.bytes())).collect(), self.layers.iter().map(|layer| (layer.name(), layer.held())).collect());
        let layers = std::mem::take(&mut self.layers);
        self.compose_ns.resize(layers.len(), 0);
        self.cancel = Arc::default();
        let away = Away { mirrors, layers, compose, upstream: self.upstream.clone(), cost: self.compose_ns.clone(), cancel: Arc::clone(&self.cancel), results: Vec::new() };
        if let Err(error) = self.worker.jobs.send(Job::Compose(Box::new(away))) {
            let Job::Compose(away) = error.0 else { unreachable!() };
            self.mirrors = Some(away.mirrors);
            self.layers = away.layers;
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Map compose worker stopped"));
        }
        self.composing = true;
        self.superseded = false;
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.reset();
        }
        self.combat.reset();
        Ok(())
    }

    fn swap_in(&mut self, away: Away) -> io::Result<()> {
        let Away { mirrors, layers, results, .. } = away;
        self.mirrors = Some(mirrors);
        self.layers = layers;
        self.composing = false;
        self.fresh.resize(self.layers.len(), false);
        self.offered.resize(self.layers.len(), None);
        for (at, (result, ns, composed)) in results.into_iter().enumerate() {
            if composed {
                self.compose_ns[at] = ns;
                self.fresh[at] = false;
                self.offered[at] = None;
            }
            match result {
                Err(error) => self.reads[at] = Err(error.to_string()),
                Ok(()) if composed => self.ready[at] = true,
                Ok(()) => {}
            }
        }
        if let Some(view) = self.view_later.take() {
            self.set_view(view)?;
        }
        Ok(())
    }

    fn take_home(&mut self, away: Away) {
        let Away { mirrors, layers, results, .. } = away;
        self.mirrors = Some(mirrors);
        self.layers = layers;
        self.composing = false;
        self.superseded = true;
        for (at, (result, _, _)) in results.into_iter().enumerate() {
            if let Err(error) = result {
                self.reads[at] = Err(error.to_string());
            }
        }
    }

    pub fn standing(&self) -> serde_json::Value {
        self.standing.report(&self.names)
    }

    pub fn views(&self) -> &views::Store {
        &self.views
    }

    pub fn layer_parts(&self) -> LayerParts<'_> {
        LayerParts {
            names: &self.names,
            reads: &self.reads,
            ready: &self.ready,
            mirrors: self.mirrors.as_deref(),
            layers: &self.layers,
            composing: self.composing,
            superseded: self.superseded,
        }
    }

    pub fn parts(&self) -> Option<(&Mirrors, &[Box<dyn Layer>])> {
        self.layer_parts().parts()
    }

    pub fn layer_retirements(&self) -> Vec<crate::layers::Retirement> {
        self.layers.iter().filter_map(|layer| layer.retirement()).chain(std::iter::once(self.combat.retirement())).collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn layers_mut(&mut self) -> &mut [Box<dyn Layer>] {
        &mut self.layers
    }

    pub fn composing(&self) -> bool {
        self.composing || self.superseded
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn applied(&self) -> Option<Hour> {
        self.applied
    }

    pub fn held(&self) -> Held {
        let (report, _, _) = self.entry.playhead.held();
        let (mirrors, layers) = match &self.mirrors {
            Some(mirrors) => (mirrors.iter().map(|mirror| (mirror.table(), mirror.bytes())).collect(), self.layers.iter().map(|layer| (layer.name(), layer.held())).collect()),
            None => self.sizes.clone(),
        };
        Held { mirrors, views: self.views.held(), layers, pending: self.pending.len(), states: (report.states, report.state_bytes), playhead: report.total }
    }

    pub fn on_screen(&self) -> Option<OnScreen> {
        use crate::layers::Published;
        if self.failure.is_some() {
            return None;
        }
        let drawn = (0..self.layers.len()).filter(|&at| self.reads[at].is_ok() && self.ready[at] && !matches!(self.layers[at].published(), Published::Cadence(_)));
        let whole = |at: usize| self.layers[at].carried().entities == 0 && self.layers[at].uploaded();
        Some(OnScreen {
            head: self.last.head,
            drawn: self.drawn,
            sought: lock(&self.entry.anchors).0,
            composing: self.composing(),
            settled: !self.composing() && drawn.clone().all(whole),
            furthest: drawn.min_by_key(|&at| (self.shown.get(at).copied().flatten(), whole(at))).map(|at| self.names[at]),
            resets: self.resets_shown,
        })
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn fail(&mut self, failure: String) {
        eprintln!("{failure}");
        self.failure = Some(failure);
    }

    pub fn branch(&self) -> Option<replay_data::BranchId> {
        self.branch
    }

    pub fn set_branch(&mut self, branch: replay_data::BranchId) {
        self.branch = Some(branch);
    }

    pub fn mode_on_screen(&self) -> Option<&viewer_map_modes::map_mode::MapMode> {
        self.mode.as_ref()
    }

    pub fn unsettled(&self, view: &View, head: Hour) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if self.settles_held.load(Ordering::Acquire) {
            return true;
        }
        self.view_shown != *view
            || self.mode.as_ref() != Some(&view.map_mode)
            || !self.whole
            || !self.on_screen().is_some_and(|on| on.settled && on.drawn == Some(head))
            || self.overlay.as_deref().is_some_and(|overlay| overlay.offered_stale(self.layer_parts(), self.branch))
            || self.answers_waiting()
    }

    pub fn answers_waiting(&self) -> bool {
        self.views.demand() != self.demand
            || self.epoch != self.entry.playhead.epoch()
            || viewer_runtime::landed::count() != self.landed
            || (self.failure.is_none() && self.entry.playhead.error().is_some())
            || self.overlay.as_deref().is_some_and(Overlaying::needs_reask)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnScreen {
    pub head: Hour,
    pub drawn: Option<Hour>,
    pub sought: Option<Hour>,
    pub composing: bool,
    pub settled: bool,
    pub furthest: Option<&'static str>,
    pub resets: u64,
}

impl<P: PlaybackProducts> Drop for MapSide<P> {
    fn drop(&mut self) {
        self.entry.maps.fetch_sub(1, Ordering::AcqRel);
        self.entry.playhead.retire(self.reader);
    }
}

fn vision_of<'a>(names: &[&'static str], reads: &[Result<BTreeSet<usize>, String>], ready: &[bool], layers: &'a [Box<dyn Layer>], away: bool) -> Option<&'a crate::vision::Vision> {
    if away {
        return None;
    }
    let at = names.iter().position(|name| *name == "vision")?;
    if !(reads.get(at)?.is_ok() && *ready.get(at)?) {
        return None;
    }
    layers.get(at)?.any().downcast_ref()
}

#[derive(Clone, Copy, Debug)]
pub struct CaughtUp {
    pub name: &'static str,
    pub caught: bool,
    pub withheld: bool,
    pub viewer: Option<u32>,
}

fn declare(entry: &Entry, layers: &mut [Box<dyn Layer>], view: &View) -> Declaring {
    let mut union: BTreeMap<usize, Read> = BTreeMap::new();
    let mut reads: Vec<Result<BTreeSet<usize>, String>> = Vec::with_capacity(layers.len());
    let mut upstream = Vec::with_capacity(layers.len());
    let mut each = Vec::with_capacity(layers.len());
    let names: Vec<&'static str> = layers.iter().map(|layer| layer.name()).collect();
    for (at, layer) in layers.iter_mut().enumerate() {
        let mut inputs = Inputs::new(entry.kinds(), layer.name());
        let declared = layer.inputs(view, &mut inputs).and_then(|()| {
            inputs
                .upstream()
                .iter()
                .map(|name| match names[..at].iter().position(|held| held == name) {
                    Some(before) if reads[before].is_ok() => Ok(before),
                    Some(_) => Err(io::Error::other(format!("The {} layer is refused: the {name} layer it reads is refused", names[at]))),
                    None => Err(io::Error::other(format!("The {} layer is refused: it reads the {name} layer, which is not registered before it", names[at]))),
                })
                .collect::<io::Result<Vec<usize>>>()
        });
        upstream.push(declared.as_ref().map_or_else(|_| Vec::new(), Clone::clone));
        match declared {
            Ok(_) => {
                let after = inputs.upstream().to_vec();
                let tables = inputs.into_tables();
                each.push(Some((tables.clone(), after)));
                reads.push(Ok(tables.keys().copied().collect()));
                for (table, read) in tables {
                    let held = union.entry(table).or_default();
                    held.columns.extend(read.columns);
                    held.columns.sort_unstable();
                    held.columns.dedup();
                    held.keyed |= read.keyed;
                    held.hourly |= read.hourly;
                }
            }
            Err(error) => {
                each.push(None);
                reads.push(Err(error.to_string()));
            }
        }
    }
    Declaring { union, reads, upstream, each }
}

type LayerDeclared = Option<(BTreeMap<usize, Read>, Vec<&'static str>)>;

struct Declaring {
    union: BTreeMap<usize, Read>,
    reads: Vec<Result<BTreeSet<usize>, String>>,
    upstream: Vec<Vec<usize>>,
    each: Vec<LayerDeclared>,
}

pub(crate) fn timeline_runs(entry: &Entry) -> Vec<usize> {
    let mut runs = Vec::new();
    for piece in entry.playhead().timeline().pieces() {
        if !runs.contains(&piece.run) {
            runs.push(piece.run);
        }
    }
    runs
}

fn for_the_makers(entry: &Entry) -> BTreeMap<usize, Read> {
    let kinds = entry.kinds();
    let written = entry.pages.written_tables(&timeline_runs(entry), kinds.len());
    (0..kinds.len())
        .filter(|&table| table != script_values::TABLE.index() && matches!(written[table], playhead::pages::TableWritten::Written))
        .map(|table| (table, Read { columns: (0..kinds[table].len()).collect(), keyed: true, hourly: false }))
        .collect()
}

fn hand_naval(names: &[&'static str], layers: &mut [Box<dyn Layer>], reads: &[Result<BTreeSet<usize>, String>], ready: &[bool]) {
    let drawn = |at: usize| reads[at].is_ok() && ready[at];
    let draws = |name: &str| names.iter().enumerate().any(|(at, held)| *held == name && drawn(at));
    if !(draws("naval") && draws("political")) {
        return;
    }
    let Some(naval) = names
        .iter()
        .position(|&name| name == "naval")
        .and_then(|at| layers[at].take_naval_changed())
    else {
        return;
    };
    for (at, layer) in layers.iter_mut().enumerate() {
        if drawn(at) {
            layer.set_naval(&naval);
        }
    }
}

fn awake_mask(declared: &BTreeMap<usize, Read>) -> Vec<bool> {
    let mut mask = vec![false; declared.keys().next_back().map_or(0, |&table| table + 1)];
    for &table in declared.keys() {
        mask[table] = true;
    }
    mask
}

fn merged(layers: &BTreeMap<usize, Read>, makers: &BTreeMap<usize, Read>) -> BTreeMap<usize, Read> {
    let mut union = layers.clone();
    for (&table, read) in makers {
        let held = union.entry(table).or_default();
        held.columns.extend(read.columns.iter().copied());
        held.columns.sort_unstable();
        held.columns.dedup();
        held.keyed |= read.keyed;
        held.hourly |= read.hourly;
    }
    union
}

fn to_declared(declared: &BTreeMap<usize, Read>) -> Vec<Declared> {
    declared.iter().map(|(&table, read)| Declared { table, columns: read.columns.clone(), keyed: read.keyed, passing: false, hourly: read.hourly }).collect()
}
