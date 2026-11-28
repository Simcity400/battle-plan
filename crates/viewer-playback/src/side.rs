use crate::{map_open::{Combat, MapSide}, overlay};
use viewer_layer_core::playhead_host::{Doing, Entry, Frame, Head, LayerParts, OnScreen, Overlaying, Waited, lock, views};
use viewer_layer_core::layers::{Gpu, Installs, Retirement, View, drawn_layer as drawn};
use replay_data::Hour;
use std::{
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};
use viewer_runtime::landed::Landed;

pub const THREAD: &str = "map side";

const IDLE: Duration = Duration::from_millis(2);

const REST: Duration = Duration::from_millis(16);

const UNTIL_DUE: Duration = Duration::from_micros(200);

const LATE: Duration = Duration::from_secs(5);

const GIVEN_UP: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct Ask {
    view: View,
    at: Head,
    overlay: Option<overlay::Ask>,
    combat: Option<viewer_combat::Asking>,
    serial: u64,
}

impl Ask {
    fn same(&self, other: &Self) -> bool {
        self.view == other.view
            && self.at == other.at
            && self.combat.as_ref().map(viewer_combat::Asking::key) == other.combat.as_ref().map(viewer_combat::Asking::key)
            && match (&self.overlay, &other.overlay) {
                (Some(a), Some(b)) => {
                    (a.camera, a.target, a.pixels, a.ui_scale.to_bits()) == (b.camera, b.target, b.pixels, b.ui_scale.to_bits())
                        && a.view == b.view
                        && Arc::ptr_eq(&a.rosters, &b.rosters)
                }
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Debug)]
struct Ready {
    hour: Hour,
    view: View,
    placed: Option<Placed>,
    combat: Option<viewer_combat::ReadKey>,
}

type Placed = (viewer_gpu::camera::Camera, Option<f32>, [u32; 2], f32, crate::map_views::CounterView);

impl Ready {
    fn of(hour: Hour, ask: &Ask) -> Self {
        Self { hour, view: ask.view.clone(), placed: ask.overlay.as_ref().map(Self::placed), combat: ask.combat.as_ref().map(viewer_combat::Asking::key) }
    }

    fn placed(overlay: &overlay::Ask) -> Placed {
        (overlay.camera, overlay.target, overlay.pixels, overlay.ui_scale, overlay.view.clone())
    }

    fn answers(&self, hour: Hour, ask: &Ask) -> bool {
        self.hour == hour
            && self.view == ask.view
            && self.combat == ask.combat.as_ref().map(viewer_combat::Asking::key)
            && match (&self.placed, &ask.overlay) {
                (Some((camera, target, pixels, scale, view)), Some(overlay)) => {
                    (*camera, *target, *pixels, *scale) == (overlay.camera, overlay.target, overlay.pixels, overlay.ui_scale) && *view == overlay.view
                }
                (None, None) => true,
                _ => false,
            }
    }
}

const PICTURES_HELD: usize = 8;

struct Turn {
    side: MapSide,
    framed: bool,
    answered: Option<(u64, Hour)>,
    ready: Option<Ready>,
    published: Option<Instant>,
    reached: Option<Duration>,
    looks: u64,
    stop: bool,
    refused: Option<(u64, Hour, String)>,
}

struct Shared {
    held: Mutex<Turn>,
    ask: Mutex<Option<Ask>>,
    signal: Condvar,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn asked(&self) -> Option<Ask> {
        lock(&self.ask).clone()
    }

    fn ready(&self, turn: &Turn, hour: Hour) -> bool {
        turn.ready.as_ref().is_some_and(|ready| lock(&self.ask).as_ref().is_some_and(|ask| ready.answers(hour, ask)))
    }
}

pub struct SideThread {
    shared: Arc<Shared>,
    entry: Arc<Entry>,
    sight: Option<Arc<crate::document::Content>>,
    views: views::Views,
    products: Products,
    installs: Vec<Box<dyn Installs>>,
    install_failure: Option<String>,
    doing: Arc<Doing>,
    composed: Arc<std::sync::atomic::AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    retirements: Vec<Retirement>,
    refused: Option<String>,
}

impl SideThread {
    pub fn new(mut side: MapSide, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let (entry, sight, views) = (Arc::clone(side.entry()), side.sight().cloned(), side.views().handle());
        let doing = Arc::clone(side.doing());
        let composed = Arc::clone(side.composed());
        let retirements = side.layer_retirements();
        let installs = side.take_installs();
        let products = Products { branch: side.branch(), ..Products::default() };
        let tables = sight.as_ref().and_then(|content| content.tables);
        let shared = Arc::new(Shared {
            held: Mutex::new(Turn { side, framed: true, answered: None, ready: None, published: None, reached: None, looks: 0, stop: false, refused: None }),
            ask: Mutex::new(None),
            signal: Condvar::new(),
            wake,
        });
        let thread = std::thread::Builder::new().name(THREAD.into()).spawn({
            let shared = Arc::clone(&shared);
            move || {
                viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
                if tables.is_some() {
                    engine_executable::executable_tables::enter(tables);
                }
                advance(&shared);
            }
        })?;
        Ok(Self { shared, entry, sight, views, products, installs, install_failure: None, doing, composed, thread: Some(thread), retirements, refused: None })
    }

    pub fn ask(&self, view: View, at: Head, overlay: Option<overlay::Ask>, combat: Option<viewer_combat::Asking>) {
        let mut ask = lock(&self.shared.ask);
        let next = Ask { view, at, overlay, combat, serial: 0 };
        if !matches!(next.at, Head::Stepped(_)) && ask.as_ref().is_some_and(|held| held.same(&next)) {
            return;
        }
        let serial = ask.as_ref().map_or(0, |held| held.serial + 1);
        *ask = Some(Ask { serial, ..next });
        drop(ask);
        self.shared.signal.notify_all();
    }

    pub fn installed_through(&self) -> Option<Hour> {
        self.installs.iter().filter_map(|installs| installs.shown()).min()
    }

    pub fn reassert(&mut self, gpu: &mut Gpu<'_>) {
        for installs in &mut self.installs {
            if let Err(error) = installs.reassert(gpu) {
                self.install_failure = Some(format!("The map's layer {} stopped writing its borders again: {error}", installs.name()));
            }
        }
    }

    pub fn plans_shown(&self) -> Option<Hour> {
        self.installs.iter().filter_map(|installs| installs.plans_shown()).max()
    }

    pub fn install(&mut self, gpu: &mut Gpu<'_>, through: Option<Hour>) {
        if self.installs.is_empty() {
            return;
        }
        let _lap = replay_frame::cpu::scope("frame.refresh.installs", Some("other"));
        let composed = self.composed.load(std::sync::atomic::Ordering::Acquire);
        for installs in &mut self.installs {
            if let Err(error) = installs.install(gpu, through, composed) {
                self.install_failure = Some(format!("The map's layer {} stopped at an install: {error}", installs.name()));
            }
            if viewer_runtime::publication_check::on() {
                viewer_runtime::publication_check::judged();
                if installs.owes() {
                    viewer_runtime::publication_check::found(installs.name(), "a change set whose hour this frame had reached was left unwritten");
                }
            }
        }
        let timed = self.installs.iter().map(|installs| installs.shown()).min().expect("an installer");
        if timed.is_none() {
            return;
        }
        let (untimed, untimed_oldest) = self.products.untimed;
        let hours = |held: Option<Hour>| match (held, timed) {
            (Some(held), Some(timed)) => Some(held.min(timed)),
            _ => None,
        };
        if let Some(on) = self.products.on_screen.as_mut() {
            on.drawn = hours(untimed);
        }
        (self.products.frame.drawn, self.products.frame.oldest) = (hours(untimed), hours(untimed_oldest));
    }

    pub fn installed(&self) -> impl Iterator<Item = (&'static str, Option<Hour>, viewer_layer_core::layers::Wrote)> {
        self.installs.iter().map(|installs| (installs.name(), installs.shown(), installs.wrote()))
    }

    pub fn window(&mut self) -> Option<Window<'_>> {
        let turn = match self.shared.held.try_lock() {
            Ok(turn) => turn,
            Err(TryLockError::Poisoned(held)) => held.into_inner(),
            Err(TryLockError::WouldBlock) => {
                viewer_runtime::view_record::add("layers", "frames_without_the_side", 1);
                self.products.missed = Some(self.doing.name());
                return None;
            }
        };
        self.products.missed = None;
        Some(Window { turn, signal: &self.shared.signal, products: &mut self.products })
    }

    pub fn stepped_window(&mut self, hour: Hour) -> Option<Window<'_>> {
        self.whole_at(hour, LATE, GIVEN_UP)
    }

    pub(super) fn whole_at(&mut self, hour: Hour, late: Duration, given_up: Duration) -> Option<Window<'_>> {
        let started = Instant::now();
        let mut said = 0;
        let mut turn = lock(&self.shared.held);
        let serial = || lock(&self.shared.ask).as_ref().map_or(0, |ask| ask.serial);
        let refused = |turn: &Turn| turn.refused.as_ref().is_some_and(|(asked, at, _)| *at == hour && *asked == serial());
        while !self.shared.ready(&turn, hour) && turn.side.failure().is_none() && !refused(&turn) {
            turn = self.shared.signal.wait_timeout(turn, late).unwrap_or_else(std::sync::PoisonError::into_inner).0;
            if self.shared.ready(&turn, hour) || turn.side.failure().is_some() || refused(&turn) {
                break;
            }
            let waited = started.elapsed();
            if waited >= given_up {
                turn.refused = Some((serial(), hour, waiting_for(&self.shared, &turn, hour, waited)));
                break;
            }
            let times = waited.as_nanos() / late.as_nanos().max(1);
            if times > said {
                said = times;
                eprintln!("{}", waiting_for(&self.shared, &turn, hour, waited));
            }
        }
        if let Some(failure) = turn.side.failure() {
            self.products.failure = Some(failure.to_owned());
            return None;
        }
        if refused(&turn) {
            self.refused = turn.refused.as_ref().map(|(_, _, why)| why.clone());
            return None;
        }
        Some(Window { turn, signal: &self.shared.signal, products: &mut self.products })
    }

    pub fn take_refusal(&mut self) -> Option<String> {
        self.refused.take()
    }

    pub fn lay_overlay_again(&mut self) -> Result<Landed<overlay::Made>, String> {
        let mut turn = lock(&self.shared.held);
        if let Some(failure) = turn.side.failure() {
            return Err(failure.to_owned());
        }
        turn.side.lay_overlay_again()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn wait_window(&mut self, mut done: impl FnMut(&mut Self) -> bool) -> Result<(), String> {
        loop {
            if done(self) {
                return Ok(());
            }
            let mut turn = lock(&self.shared.held);
            while turn.framed && turn.side.failure().is_none() && !turn.stop {
                turn = self.shared.signal.wait(turn).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if let Some(failure) = turn.side.failure() {
                let failure = failure.to_owned();
                self.products.failure = Some(failure.clone());
                return Err(failure);
            }
            if turn.stop {
                return Err("the map's side thread stopped".to_owned());
            }
            self.products.missed = None;
            let mut window = Window { turn, signal: &self.shared.signal, products: &mut self.products };
            window.take(Default::default());
            drop(window);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn driven<T>(&mut self, drive: impl FnOnce(&mut MapSide) -> T) -> T {
        let turn = lock(&self.shared.held);
        self.products.missed = None;
        let mut window = Window { turn, signal: &self.shared.signal, products: &mut self.products };
        let made = drive(window.side_mut());
        window.take(Wants { warnings: true, highlight: false });
        drop(window.take_overlay());
        drop(window.take_combat());
        made
    }

    pub fn products(&self) -> &Products {
        &self.products
    }

    pub fn on_screen(&self) -> Option<OnScreen> {
        self.products.on_screen
    }

    pub fn failure(&self) -> Option<&str> {
        self.products.failure.as_deref().or(self.install_failure.as_deref())
    }

    pub fn branch(&self) -> Option<replay_data::BranchId> {
        self.products.branch
    }

    pub fn draws(&self, name: &str) -> bool {
        self.products.draws(name)
    }

    pub fn composing(&self) -> bool {
        self.products.composing
    }

    pub fn applied(&self) -> Option<Hour> {
        self.products.applied
    }

    pub fn mode_on_screen(&self) -> Option<&viewer_map_modes::map_mode::MapMode> {
        self.products.mode.as_ref()
    }

    pub fn last_frame(&self) -> &Frame {
        &self.products.frame
    }

    pub fn view(&self) -> &View {
        &self.products.view
    }

    pub fn names(&self) -> &[&'static str] {
        &self.products.names
    }

    pub fn with_side<T>(&self, read: impl FnOnce(&MapSide) -> T) -> T {
        read(&lock(&self.shared.held).side)
    }

    pub fn try_with_side<T>(&self, read: impl FnOnce(&MapSide) -> T) -> Option<T> {
        match self.shared.held.try_lock() {
            Ok(turn) => Some(read(&turn.side)),
            Err(TryLockError::Poisoned(held)) => Some(read(&held.into_inner().side)),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_side_mut<T>(&self, change: impl FnOnce(&mut MapSide) -> T) -> T {
        change(&mut lock(&self.shared.held).side)
    }

    pub fn views(&self) -> &views::Views {
        &self.views
    }

    pub fn entry(&self) -> &Arc<Entry> {
        &self.entry
    }


    pub fn head(&self, now: Instant) -> Hour {
        self.entry.playhead().head(now)
    }

    pub fn anchor(&self, hour: Hour, playing: bool, speed: f64, sought: bool) {
        self.entry.anchor(hour, playing, speed, sought);
    }

    pub fn made_over(&self, content: *const crate::document::Content) -> bool {
        self.sight.as_ref().is_some_and(|sight| std::ptr::eq(Arc::as_ptr(sight), content))
    }
}

impl Drop for SideThread {
    fn drop(&mut self) {
        for stop in self.retirements.drain(..) {
            stop();
        }
        lock(&self.shared.held).stop = true;
        self.shared.signal.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn waiting_for(shared: &Shared, turn: &Turn, hour: Hour, waited: Duration) -> String {
    format!(
        "The saved frame at hour {hour} waited {:.1} s for the map's side: asked {:?}, whole at {:?}, framed {}, looks {}, stopping {}; {}",
        waited.as_secs_f64(),
        shared.asked().map(|ask| ask.at),
        turn.ready.as_ref().map(|ready| ready.hour),
        turn.framed,
        turn.looks,
        turn.stop,
        turn.side.shown_states(),
    )
}

pub struct Window<'a> {
    turn: MutexGuard<'a, Turn>,
    signal: &'a Condvar,
    products: &'a mut Products,
}

impl Window<'_> {
    pub fn side(&self) -> &MapSide {
        &self.turn.side
    }

    pub fn side_mut(&mut self) -> &mut MapSide {
        self.products.movement_of = None;
        &mut self.turn.side
    }

    pub fn upload(&mut self, gpu: &mut Gpu<'_>) {
        let side = &mut self.turn.side;
        if side.failure().is_some() {
            return;
        }
        if let Err(error) = side.upload(gpu) {
            side.fail(format!("The map's playhead stopped at an upload: {error}"));
        }
    }

    pub fn take_overlay(&mut self) -> Option<Landed<overlay::Made>> {
        self.products.overlay.take()
    }

    pub fn take_combat(&mut self) -> Option<Landed<viewer_combat::Product>> {
        self.turn.side.take_combat()
    }

    pub fn take_overlay_withdrawal(&mut self) -> bool {
        std::mem::take(&mut self.products.overlay_withdrawn)
    }

    pub fn take(&mut self, wants: Wants) {
        let _lap = replay_frame::cpu::scope("frame.refresh.products", Some("other"));
        let reached = self.turn.published.take().map(|published| published.elapsed());
        self.turn.reached = reached.or(self.turn.reached);
        self.turn.side.reached(reached);
        self.products.take(&mut self.turn.side, wants);
    }
}

impl Drop for Window<'_> {
    fn drop(&mut self) {
        self.turn.framed = true;
        self.signal.notify_all();
    }
}

fn rests(turn: &Turn, ask: &Ask, head: Hour, moving: bool, viewpoint: bool) -> bool {
    !moving
        && !viewpoint
        && !turn.side.hurrying()
        && turn.answered == Some((ask.serial, head))
        && !turn.side.unsettled(&ask.view, head)
}

fn advance(shared: &Shared) {
    use viewer_runtime::simulation_rate as rate;
    let (mut pace, mut reach) = (rate::Pace::new(Instant::now()), rate::Reach::default());
    let mut waited = Waited::default();
    let mut turn = lock(&shared.held);
    loop {
        turn.looks += 1;
        if turn.stop {
            return;
        }
        let Some(ask) = shared.asked().filter(|_| turn.side.failure().is_none()) else {
            turn = wait(shared, turn);
            continue;
        };
        if turn.refused.as_ref().is_some_and(|(asked, at, _)| *asked != ask.serial || ask.at != Head::Stepped(*at)) {
            turn.refused = None;
        }
        match ask.at {
            Head::Stepped(hour) if turn.ready.as_ref().is_some_and(|ready| ready.answers(hour, &ask)) || turn.refused.is_some() => turn = wait(shared, turn),
            Head::Stepped(hour) => {
                turn.ready = None;
                match toward(&mut turn.side, &ask, hour, Made::Stepped) {
                    Ok(()) => turn.ready = Some(Ready::of(hour, &ask)),
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => turn.refused = Some((ask.serial, hour, error.to_string())),
                    Err(error) => turn.side.fail(format!("The map's playhead stopped: {error}")),
                }
                shared.signal.notify_all();
            }
            _ if !turn.framed => {
                let since = Instant::now();
                let due = pace.late(since).is_some();
                turn = wait(shared, turn);
                if due {
                    waited.for_the_frame_ns += since.elapsed().as_nanos() as u64;
                }
            }
            at => {
                let (now, interval) = (Instant::now(), rate::interval());
                let viewpoint = ask.view.perspective != turn.side.view().perspective;
                let asked = match at {
                    Head::Clock => turn.side.head(now),
                    Head::Paced(hour) | Head::Stepped(hour) => hour,
                };
                let ahead = reach.ahead(interval) + interval;
                let moving = turn.side.head_from(asked, ahead);
                let head = moving.unwrap_or(asked);
                if rests(&turn, &ask, head, moving.is_some(), viewpoint) {
                    turn = waiting(shared, turn, REST);
                    continue;
                }
                let urgent = viewpoint || turn.side.hurrying() || turn.answered != Some((ask.serial, head)) || turn.side.answers_waiting();
                let Some(late) = pace.late(now).or(urgent.then_some(std::time::Duration::ZERO)) else {
                    turn = waiting(shared, turn, pace.until(now).max(UNTIL_DUE).min(IDLE));
                    continue;
                };
                if let Some(reached) = turn.reached.take() {
                    reach.took(reached, interval);
                }
                if moving.is_some() && late < interval && !viewpoint && !turn.side.reaches(head) {
                    let since = Instant::now();
                    turn = waiting(shared, turn, UNTIL_DUE);
                    waited.for_an_hour_ns += since.elapsed().as_nanos() as u64;
                    continue;
                }
                pace.take(now, interval);
                waited.late_ns = late.as_nanos() as u64;
                turn.side.waited(std::mem::take(&mut waited));
                turn.framed = false;
                turn.ready = None;
                turn.published = Some(now);
                let made = if moving.is_some() || viewpoint || turn.side.hurrying() { Made::Playing } else { Made::Still };
                if let Err(error) = toward(&mut turn.side, &ask, head, made) {
                    turn.side.fail(format!("The map's playhead stopped: {error}"));
                }
                turn.answered = Some((ask.serial, head));
                drop(turn);
                (shared.wake)();
                shared.signal.notify_all();
                turn = lock(&shared.held);
            }
        }
    }
}

fn wait<'a>(shared: &'a Shared, turn: MutexGuard<'a, Turn>) -> MutexGuard<'a, Turn> {
    waiting(shared, turn, IDLE)
}

fn waiting<'a>(shared: &'a Shared, turn: MutexGuard<'a, Turn>, most: Duration) -> MutexGuard<'a, Turn> {
    shared.signal.wait_timeout(turn, most).unwrap_or_else(std::sync::PoisonError::into_inner).0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Made {
    Playing,
    Still,
    Stepped,
}

fn toward(side: &mut MapSide, ask: &Ask, head: Hour, made: Made) -> io::Result<()> {
    if let Some(error) = side.entry().playhead().error() {
        return Err(io::Error::other(format!("The playhead stopped: {error}")));
    }
    if side.view() != &ask.view {
        side.set_view(ask.view.clone())?;
    }
    if let Some(overlay) = &ask.overlay {
        side.ask_overlay(overlay.clone());
    }
    side.ask_combat(ask.combat.clone().map(Combat));
    match made {
        Made::Playing => side.frame(head).map(drop),
        Made::Still => side.frame_still(head).map(drop),
        Made::Stepped => side.catch_up(head),
    }
}

#[derive(Default)]
pub struct LayIns {
    pub labels: Option<(Arc<map_display_data::landscape::Labels>, u32)>,
    pub intel: Option<Landed<Arc<viewer_raids::map::intel_icons::IntelIcons>>>,
    pub raids: Option<Landed<Arc<viewer_raids::map::raids::RaidIcons>>>,
    resources: Option<(viewer_map_icons::resource_icons::HourIcons, u64)>,
    occupation: Option<(viewer_map_icons::occupation_icons::Marks, u64)>,
}

impl LayIns {
    pub fn of(side: LayerParts<'_>, branch: Option<replay_data::BranchId>) -> Self {
        let resources = viewer_layers_political::resource_icons::drawn(side);
        let occupation = viewer_layers_political::occupation_marks::drawn(side);
        let ours = viewer_layer_core::vision::drawn(side).and_then(viewer_layer_core::vision::Vision::ours);
        Self {
            labels: viewer_layers_political::labels::drawn(side),
            intel: viewer_layers_forces::intel_icons::drawn(side),
            raids: branch.zip(ours).and_then(|(branch, ours)| viewer_layers_political::scripts::raids(side, branch, ours)),
            resources: resources.map(|layer| (layer.icons(), layer.version())),
            occupation: occupation.map(|layer| (layer.marks(), layer.version())),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_icons(
        resources: Option<(viewer_map_icons::resource_icons::HourIcons, u64)>,
        occupation: Option<(viewer_map_icons::occupation_icons::Marks, u64)>,
    ) -> Self {
        Self {
            resources,
            occupation,
            ..Self::default()
        }
    }

    pub fn set(&self, tables: &mut crate::document::OverlayTables) {
        if let Some((labels, ours)) = &self.labels {
            tables.set_labels(labels, *ours);
        }
        if let Some(intel) = &self.intel {
            tables.set_intel(intel);
        }
    }

    pub fn lay(&self, prepared: &mut crate::document::PreparedOverlay, hour: &impl crate::document::IconHost, camera: viewer_gpu::camera::Camera, pixels: [u32; 2], ui_scale: f32, ours: u32) -> bool {
        let view = prepared.counter_view().clone();
        self.lay_icons(&mut prepared.laid_icons, &view, hour, camera, pixels, ui_scale, ours)
    }

    #[expect(clippy::too_many_arguments, reason = "the icon state, view, host, camera, viewport, scale and viewer country")]
    pub fn lay_icons(
        &self,
        laid_icons: &mut crate::document::LaidIcons,
        view: &crate::map_views::CounterView,
        hour: &impl crate::document::IconHost,
        camera: viewer_gpu::camera::Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        ours: u32,
    ) -> bool {
        use crate::document::MadeBy;
        let mut laid = false;
        if let Some((icons, version)) = &self.resources
            && laid_icons.resource_icons_by() != MadeBy::Layer(*version)
        {
            laid_icons.replace_resource_icons(hour, camera, pixels, ui_scale, view.visibility, icons, *version);
            laid = true;
        }
        if let Some((marks, version)) = &self.occupation
            && laid_icons.occupation_icons_by() != MadeBy::Layer(*version)
        {
            laid_icons.replace_occupation_icons(hour, camera, pixels, ui_scale, view.occupation, ours, marks, *version);
            laid = true;
        }
        laid
    }

}

#[derive(Clone, Copy, Default)]
pub struct Wants {
    pub warnings: bool,
    pub highlight: bool,
}

pub struct Products {
    windows: u64,
    pub missed: Option<&'static str>,
    pub names: Vec<&'static str>,
    drawn: Vec<bool>,
    pub on_screen: Option<OnScreen>,
    untimed: (Option<Hour>, Option<Hour>),
    pub failure: Option<String>,
    pub view: View,
    pub composing: bool,
    pub applied: Option<Hour>,
    pub mode: Option<viewer_map_modes::map_mode::MapMode>,
    pub frame: Frame,
    pub branch: Option<replay_data::BranchId>,
    pub battles: Option<Landed<Option<Arc<viewer_audio::battle::Battles>>>>,
    pub radar: Option<Arc<viewer_aircraft::radar_symbols::Field>>,
    pub plan: Option<Landed<(Hour, Arc<viewer_gpu::ribbon_content::OrderField>)>>,
    pub routes: Option<Landed<(u64, Arc<viewer_unit_models::trade_routes::TradeRoutes>)>>,
    pub labels: Option<viewer_layers_political::labels::Stamped>,
    pub left: viewer_layers_political::left_to_layers::LeftToLayers,
    pub placements: Option<Landed<Arc<viewer_runtime::name_draw::Placements>>>,
    pub counters: viewer_counters::counters::layered::Layered,
    pub counter_kinds: viewer_counter_kit::Kinds,
    pub units: Option<Landed<Arc<viewer_unit_models::unit_models::PreparedModels>>>,
    pub movement: Vec<(viewer_runtime::unit_handle::Unit, viewer_unit_reads::counter_move::Position, f32, f32)>,
    movement_of: Option<u64>,
    pub pick_divisions: std::collections::HashMap<u32, Vec<viewer_layers_forces::units::PickDivision>>,
    pub ours: Option<u32>,
    pub engaged: Option<Arc<viewer_engagements::engagement_history::History>>,
    pub highlight: Option<(u64, crate::highlight::Over<Arc<crate::highlight::Picture>>)>,
    pub highlight_hour: Option<Hour>,
    pictures: std::collections::VecDeque<(Hour, u64, Arc<crate::highlight::Picture>)>,
    pub snow: Option<Landed<map_display_data::Summary>>,
    pub music: Option<Landed<Arc<viewer_audio::Chances>>>,
    pub lines: Option<Lines>,
    warning_facts: crate::limitations::FactsCache,
    pub overlay: Option<Landed<overlay::Made>>,
    overlay_withdrawn: bool,
    pub overlay_refused: bool,
    pub vision_refused: Option<String>,
    pub overlay_generation: Option<u64>,
    pub overlay_hour: Option<Hour>,
    pub overlay_loading: Vec<String>,
    pub overlay_missed: Vec<replay_data::rows::TableId>,
    pub vision: Result<Landed<Arc<viewer_foundation::perspective::Vision>>, String>,
    pub combat_loading: Vec<String>,
    pub combat_whole: Option<Result<(), String>>,
}

impl Default for Products {
    fn default() -> Self {
        Self {
            windows: 0,
            missed: None,
            names: Vec::new(),
            drawn: Vec::new(),
            on_screen: None,
            untimed: (None, None),
            failure: None,
            view: View::default(),
            composing: false,
            applied: None,
            mode: None,
            frame: Frame::default(),
            branch: None,
            battles: None,
            radar: None,
            plan: None,
            routes: None,
            labels: None,
            left: Default::default(),
            placements: None,
            counters: Default::default(),
            counter_kinds: Default::default(),
            units: None,
            movement: Vec::new(),
            movement_of: None,
            pictures: std::collections::VecDeque::new(),
            pick_divisions: Default::default(),
            highlight_hour: None,
            ours: None,
            engaged: None,
            highlight: None,
            snow: None,
            music: None,
            lines: None,
            warning_facts: Default::default(),
            overlay: None,
            overlay_withdrawn: false,
            overlay_refused: false,
            vision_refused: None,
            overlay_generation: None,
            overlay_hour: None,
            overlay_loading: Vec::new(),
            overlay_missed: Vec::new(),
            vision: Err("the playhead's map is not open".to_owned()),
            combat_loading: Vec::new(),
            combat_whole: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayInKeys {
    labels: Option<(usize, u32)>,
    intel: Option<usize>,
    resources: Option<u64>,
    occupation: Option<u64>,
}

impl LayInKeys {
    pub fn of(side: LayerParts<'_>) -> Self {
        Self {
            labels: viewer_layers_political::labels::drawn(side).map(|(labels, ours)| (Arc::as_ptr(&labels) as usize, ours)),
            intel: viewer_layers_forces::intel_icons::drawn(side).map(|icons| Arc::as_ptr(&*icons) as usize),
            resources: viewer_layers_political::resource_icons::drawn(side).map(viewer_layers_political::resource_icons::ResourceIcons::version),
            occupation: viewer_layers_political::occupation_marks::drawn(side).map(viewer_layers_political::occupation_marks::OccupationMarks::version),
        }
    }
}

pub struct Lines {
    pub political: Vec<String>,
    pub limitations: Vec<String>,
    pub reports: Vec<String>,
}

impl Products {
    pub fn draws(&self, name: &str) -> bool {
        self.names.iter().zip(&self.drawn).any(|(held, &drawn)| *held == name && drawn)
    }

    pub fn overlay_coming(&self) -> bool {
        self.vision_refused.is_none() && self.failure.is_none() && !self.overlay_refused
    }

    fn take(&mut self, side: &mut MapSide, wants: Wants) {
        if side.take_overlay_withdrawal() {
            (self.overlay, self.overlay_withdrawn) = (None, true);
        }
        let made = side.take_overlay();
        let overlay_refused = side.overlay().is_some_and(Overlaying::refused);
        if overlay_refused && !self.overlay_refused {
            (self.overlay, self.overlay_withdrawn) = (None, true);
        }
        self.overlay_refused = overlay_refused;
        if let Some(made) = made {
            self.overlay = Some(made);
        }
        if self.combat_loading != side.combat_loading() {
            self.combat_loading = side.combat_loading().to_vec();
        }
        if self.combat_whole.as_ref() != side.combat_whole_state() {
            self.combat_whole = side.combat_whole_state().cloned();
        }
        self.vision = side.vision();
        if let Some(overlay) = side.overlay() {
            self.overlay_generation = Some(overlay.generation());
            self.overlay_hour = overlay.hour();
            if self.overlay_loading != overlay.loading() {
                self.overlay_loading = overlay.loading().to_vec();
            }
            if self.overlay_missed != overlay.missed() {
                self.overlay_missed = overlay.missed().to_vec();
            }
        }
        let side = &*side;
        self.labels = viewer_layers_political::labels::stamped(side);
        self.windows += 1;
        if self.names != side.names() {
            self.names = side.names().to_vec();
        }
        self.drawn.clear();
        self.drawn.extend(side.names().iter().map(|name| side.draws(name)));
        self.on_screen = side.on_screen();
        self.untimed = side.untimed_hours();
        self.failure = side.failure().map(str::to_owned);
        if self.view != *side.view_shown() {
            self.view.clone_from(side.view_shown());
        }
        if !side.composing() {
            self.vision_refused = side.vision_refused();
        }
        self.composing = side.composing();
        self.applied = side.applied();
        if self.mode.as_ref() != side.mode_on_screen() {
            self.mode = side.mode_on_screen().cloned();
        }
        self.frame.clone_from(side.last_frame());
        self.branch = side.branch();
        self.battles = viewer_layers_infrastructure::sounds::drawn(side);
        self.radar = drawn::<viewer_layers_infrastructure::radar::Radar>(side, "radar").and_then(viewer_layers_infrastructure::radar::Radar::field);
        self.plan = drawn::<viewer_layers_forces::orders::Orders>(side, "orders").and_then(viewer_layers_forces::orders::Orders::landed_plan);
        self.routes = side.draws("units").then(|| drawn::<viewer_layers_forces::convoys::Convoys>(side, "convoys")).flatten().and_then(viewer_layers_forces::convoys::Convoys::shown);
        self.left = viewer_layers_political::left_to_layers::LeftToLayers::of(Some(side.layer_parts()));
        self.placements = viewer_layers_political::names::drawn(side);
        self.counters = viewer_layers_counters::counter_kinds::published(side.layer_parts());
        self.counter_kinds = viewer_layers_counters::counter_kinds::draws(side.layer_parts());
        self.units = viewer_layers_forces::units::prepared(side).cloned();
        let made = side.last_frame().made;
        if self.movement_of != Some(made) {
            self.movement_of = Some(made);
            self.movement.clear();
            self.pick_divisions.clear();
            if let Some(units) = viewer_layers_forces::units::of(side) {
                self.movement.extend(units.movement_positions().map(|(unit, place, scale, fade)| (unit, place.clone(), scale, fade)));
                self.pick_divisions = units.pick_divisions();
            }
        }
        self.ours = viewer_layer_core::vision::drawn(side.layer_parts()).and_then(viewer_layer_core::vision::Vision::ours);
        self.engaged = drawn::<viewer_layer_core::engagements::Engagements>(side, "engagements").and_then(viewer_layer_core::engagements::Engagements::history).map(Arc::clone);
        self.take_highlight(side, wants.highlight);
        self.snow = drawn::<viewer_layers_infrastructure::snow::Snow>(side, "snow").and_then(viewer_layers_infrastructure::snow::Snow::landed_summary);
        self.music = viewer_layers_political::scripts::music(side);
        self.lines = wants.warnings.then(|| {
            let [limitations, reports] = crate::presentation::cached_map_lines(side, &mut self.warning_facts);
            Lines { political: viewer_layers_political::political_lines::political_lines(side), limitations, reports }
        });
    }

    fn take_highlight(&mut self, side: &MapSide, wanted: bool) {
        use crate::highlight::{Over, Picture};
        if !side.draws("political") {
            self.highlight = None;
            return;
        }
        if let Some(fills) = drawn::<viewer_layers_political::mode_fills::ModeFills>(side, "mode_fills").filter(|fills| fills.drawn()) {
            self.highlight = viewer_layers_political::mode_fills::picture(fills).map(|(regions, zone, hour)| (1 << 62 | u64::from(hour as u32), Over::Mode(regions, zone)));
            self.highlight_hour = None;
            return;
        }
        let Some(political) = drawn::<viewer_layers_political::political::Political>(side, "political") else {
            self.highlight = None;
            return;
        };
        if !wanted {
            self.pictures.clear();
            return;
        }
        let key = 1 << 63 | (political.changed() & !(1 << 63));
        if let Some(applied) = side.applied()
            && self.pictures.back().is_none_or(|&(_, held, _)| held != key)
        {
            if self.pictures.len() == PICTURES_HELD {
                self.pictures.pop_front();
            }
            self.pictures.push_back((applied, key, Arc::new(Picture::of(political))));
        }
        let (drawn, plans) = (side.on_screen().and_then(|on| on.drawn), political.plans_shown());
        let fits = |hour: Hour| drawn.is_some_and(|drawn| hour <= drawn) && plans.is_none_or(|plans| hour >= plans);
        let Some((_, key, picture)) = self.pictures.iter().rev().find(|&&(hour, ..)| fits(hour)) else {
            self.highlight = None;
            return;
        };
        if self.highlight.as_ref().is_none_or(|(at, _)| at != key) {
            self.highlight = Some((*key, Over::Layer(Arc::clone(picture))));
        }
        self.highlight_hour = self.pictures.iter().find(|&&(_, held, _)| held == *key).map(|&(hour, ..)| hour);
        let key = *key;
        while self.pictures.front().is_some_and(|&(_, held, _)| held != key) {
            self.pictures.pop_front();
        }
    }
}

#[cfg(test)]
mod tests;
