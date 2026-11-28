use crate::playhead_host::mirror::Mirrors;
use eframe::wgpu;
use playhead::Step;
use replay_columnar::state::Kind;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use viewer_engagements::engagement_history::History;
use viewer_gpu::{camera::MapSize, province_admission::Admission};
use viewer_foundation::perspective;
use viewer_map_modes::map_mode::MapMode;

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub map_mode: MapMode,
    pub perspective: perspective::Choice,
    pub camera_height: f32,
    pub camera: Option<viewer_gpu::camera::Placement>,
    pub target: Option<viewer_gpu::camera::Placement>,
    pub fog: bool,
    pub viewport: [u32; 2],
    pub graphics: dashboard_core::protocol::MapGraphics,
    pub sound: bool,
    pub audio: dashboard_core::protocol::MapAudio,
    pub overlay: crate::scene_content::OverlayLayers,
    pub counters: dashboard_core::protocol::CounterFilter,
    pub player_counters: bool,
    pub selected: Option<u32>,
}

impl View {
    pub fn same_declaration(&self, other: &Self) -> bool {
        let unplaced = |view: &Self| Self { camera_height: 0.0, camera: None, target: None, viewport: [0; 2], ..view.clone() };
        unplaced(self) == unplaced(other)
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            map_mode: MapMode::default(),
            perspective: perspective::Choice::default(),
            camera_height: 0.0,
            camera: None,
            target: None,
            viewport: [0, 0],
            fog: true,
            graphics: Default::default(),
            sound: true,
            audio: Default::default(),
            overlay: Default::default(),
            counters: Default::default(),
            player_counters: false,
            selected: None,
        }
    }
}

pub struct Gpu<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub scene: &'a mut viewer_world::scene::Scene,
    pub content: &'a crate::scene_content::SceneContent,
    pub counter_gui: &'a viewer_native_gui::native_resources::GuiResources,
    pub whole: bool,
}

pub trait Installs: Send {
    fn name(&self) -> &'static str;

    fn install(&mut self, gpu: &mut Gpu<'_>, through: Option<Hour>, composed: u64) -> io::Result<()>;

    fn shown(&self) -> Option<Hour>;

    fn owes(&self) -> bool;

    fn wrote(&self) -> Wrote;

    fn reassert(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        let _ = gpu;
        Ok(())
    }

    fn plans_shown(&self) -> Option<Hour> {
        None
    }
}

#[derive(Clone, Copy, Default)]
pub struct Wrote {
    pub ns: u64,
    pub bytes: u64,
    pub writes: [u64; 3],
    pub phases: [u64; 4],
}

pub struct Shown(std::sync::atomic::AtomicI64);

impl Shown {
    const NONE: i64 = i64::MIN;

    pub fn set(&self, hour: Hour) {
        self.0.store(i64::from(hour), std::sync::atomic::Ordering::Release);
    }

    pub fn get(&self) -> Option<Hour> {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            Self::NONE => None,
            hour => Some(hour as Hour),
        }
    }
}

impl Default for Shown {
    fn default() -> Self {
        Self(std::sync::atomic::AtomicI64::new(Self::NONE))
    }
}

pub const BOUND: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate(u32);

impl Rate {
    pub const fn a_second(makings: u32) -> Self {
        Self(if makings == 0 { 1 } else { makings })
    }

    pub fn a_publication() -> Self {
        Self::a_second(viewer_runtime::simulation_rate::rate().round().max(1.0) as u32)
    }

    pub fn makings_a_second(self) -> u32 {
        self.0
    }

    pub fn interval(self) -> std::time::Duration {
        std::time::Duration::from_secs(1) / self.0
    }

    pub fn behind(self) -> u32 {
        let publications = viewer_runtime::simulation_rate::rate();
        (publications / f64::from(self.0)).ceil() as u32 + BOUND
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settling {
    Live,
    Still,
    Whole,
}

impl Settling {
    pub fn step() -> Self {
        Self::Live
    }

    pub fn still() -> Self {
        Self::Still
    }

    pub fn waits(self) -> bool {
        self != Self::Live
    }

    pub fn waiting(self, whole: std::time::Duration) -> std::time::Duration {
        if self.waits() { whole } else { std::time::Duration::ZERO }
    }

    pub fn strict(self) -> bool {
        self == Self::Whole
    }
}

pub fn drawn_layer<'a, T: 'static>(side: &'a crate::playhead_host::MapSide<impl crate::playhead_host::PlaybackProducts>, name: &str) -> Option<&'a T> {
    side.layer_parts().drawn(name)
}

pub type Retirement = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
pub struct RetirementFlag(Arc<AtomicBool>);

impl RetirementFlag {
    pub const POLL: std::time::Duration = std::time::Duration::from_millis(50);

    pub fn handle(&self) -> Retirement {
        let requested = Arc::clone(&self.0);
        Box::new(move || requested.store(true, Ordering::Release))
    }

    pub fn requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn check(&self) -> io::Result<()> {
        if self.requested() { Err(io::Error::from(io::ErrorKind::Interrupted)) } else { Ok(()) }
    }

    pub fn receive<T>(&self, receiver: &viewer_runtime::landed::Receiver<T>) -> io::Result<Option<viewer_runtime::landed::Landed<T>>> {
        loop {
            self.check()?;
            match receiver.recv_timeout(Self::POLL) {
                Ok(value) => return Ok(Some(value)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

pub trait Layer: Send {
    fn name(&self) -> &'static str;

    fn retirement(&self) -> Option<Retirement> {
        None
    }

    fn placed(&mut self, view: &View) -> Option<u64> {
        let _ = view;
        None
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()>;

    fn views(&mut self, _store: &crate::playhead_host::views::Store) -> io::Result<()> {
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()>;

    fn apply(&mut self, step: &Step, mirrors: &Mirrors);

    fn moved_at(&self, step: &Step, mirrors: &Mirrors, into: &mut viewer_unit_reads::counter_move::MovedHours) {
        let _ = (step, mirrors, into);
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()>;

    fn stage(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        let _ = gpu;
        Ok(())
    }

    fn timed(&self) -> bool {
        false
    }

    fn timed_at(&mut self, hour: Hour, composed: u64) -> io::Result<()> {
        let _ = (hour, composed);
        Ok(())
    }

    fn installs(&mut self) -> Option<Box<dyn Installs>> {
        None
    }

    fn shown_through(&self) -> Option<Hour> {
        None
    }

    fn carried(&self) -> Carried;

    fn published(&self) -> Published {
        Published::Current
    }

    fn made_at(&self, head: Hour) -> bool {
        let _ = head;
        true
    }

    fn behind_bound(&self) -> u32 {
        BOUND
    }

    fn wants_ahead(&self) -> bool {
        false
    }

    fn ahead(&mut self, steps: &[Arc<Step>]) {
        let _ = steps;
    }

    fn void_ahead(&mut self, mirrors: &Mirrors, kept: Option<replay_data::Hour>) {
        let _ = (mirrors, kept);
    }

    fn held(&self) -> usize;

    fn any(&self) -> &dyn std::any::Any;

    fn scope(&mut self, _sight: &Sight<'_>) -> Option<Scope> {
        None
    }

    fn uploaded(&self) -> bool {
        true
    }

    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    fn held_viewer(&self) -> Option<u32> {
        None
    }

    fn take_naval_changed(&mut self) -> Option<Arc<BTreeMap<u32, map_display_data::landscape::Attributes>>> {
        None
    }

    fn set_naval(&mut self, _attributes: &BTreeMap<u32, map_display_data::landscape::Attributes>) {}

    fn viewed(&mut self, view: &View, mirrors: &Mirrors) {
        let _ = (view, mirrors);
    }

    fn changed(&self) -> Option<&Changed> {
        None
    }

    fn clear_changed(&mut self) {}

    fn compose_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<()> {
        let _ = upstream;
        self.compose(mirrors)
    }

    fn apply_with(&mut self, step: &Step, mirrors: &Mirrors, upstream: &Upstream<'_>) {
        let _ = upstream;
        self.apply(step, mirrors);
    }

    fn settle_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        let _ = upstream;
        self.settle(mirrors, settling)
    }

    fn ask(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<bool> {
        let _ = (mirrors, upstream);
        Ok(false)
    }

    fn join(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        self.settle_with(mirrors, upstream, settling)
    }
}

pub struct Upstream<'a> {
    layers: &'a [Box<dyn Layer>],
}

impl<'a> Upstream<'a> {
    pub fn new(layers: &'a [Box<dyn Layer>]) -> Self {
        Self { layers }
    }

    pub fn get<T: 'static>(&self) -> Option<&'a T> {
        self.layers.iter().find_map(|layer| layer.any().downcast_ref::<T>())
    }

    pub fn changed<T: 'static>(&self) -> Change<'a> {
        static NONE: Changed = Changed { kinds: Vec::new(), everything: false };
        self.layers.iter().find(|layer| layer.any().is::<T>()).and_then(|layer| layer.changed()).unwrap_or(&NONE).read()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Changed {
    kinds: Vec<(&'static str, Vec<u64>)>,
    everything: bool,
}

impl Changed {
    pub fn mark(&mut self, kind: &'static str, id: u64) {
        match self.kinds.iter_mut().find(|(held, _)| *held == kind) {
            Some((_, ids)) => ids.push(id),
            None => self.kinds.push((kind, vec![id])),
        }
    }

    pub fn everything(&mut self) {
        self.everything = true;
    }

    pub fn read(&self) -> Change<'_> {
        if self.everything { Change::Whole } else { Change::Marked(Marked(self)) }
    }

    pub fn clear(&mut self) {
        self.kinds.iter_mut().for_each(|(_, ids)| ids.clear());
        self.everything = false;
    }

    pub fn bytes(&self) -> usize {
        self.kinds.iter().map(|(_, ids)| ids.len() * 8).sum()
    }
}

#[derive(Clone, Copy)]
pub enum Change<'a> {
    Whole,
    Marked(Marked<'a>),
}

#[derive(Clone, Copy)]
pub struct Marked<'a>(&'a Changed);

impl<'a> Marked<'a> {
    pub fn of(&self, kind: &str) -> &'a [u64] {
        self.0.kinds.iter().find(|(held, _)| *held == kind).map_or(&[], |(_, ids)| ids.as_slice())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Published {
    Current,
    At(replay_data::Hour),
    Nothing,
    Cadence(replay_data::Hour),
}

pub struct Fed<P> {
    handed: Option<Hour>,
    applied: Option<Hour>,
    composed: Option<Hour>,
    early: VecDeque<(Option<Hour>, viewer_runtime::landed::Landed<P>)>,
    taken: bool,
    stamp: Option<Hour>,
    due: Due,
}

impl<P> Default for Fed<P> {
    fn default() -> Self {
        Self { handed: None, applied: None, composed: None, early: VecDeque::new(), taken: false, stamp: None, due: Due::default() }
    }
}

impl<P> Fed<P> {
    pub fn due(&self) -> Due {
        self.due.clone()
    }

    pub fn compose(&mut self, hour: Option<Hour>) {
        (self.handed, self.applied, self.composed, self.taken, self.stamp) = (None, None, hour, false, None);
        self.early.clear();
        self.due.set(None);
    }

    pub fn hand<'a>(&mut self, steps: &'a [Arc<Step>]) -> Vec<&'a Arc<Step>> {
        let mut out = Vec::with_capacity(steps.len());
        for step in steps {
            if step.first() > step.last() {
                out.push(step);
            } else if self.handed.is_none_or(|handed| step.last() > handed) {
                self.handed = Some(step.last());
                out.push(step);
            }
        }
        out
    }

    pub fn applied(&mut self, step: &Step) {
        self.applied_through(step.last());
    }

    pub fn applied_hour(&self) -> Option<Hour> {
        self.applied
    }

    pub fn through(&self) -> Option<Hour> {
        self.applied.or(self.composed)
    }

    pub fn restart(&mut self) {
        self.early.clear();
    }

    pub fn void(&mut self) {
        let through = self.through();
        self.early.retain(|(stamp, _)| *stamp <= through);
        self.handed = through;
    }

    pub fn applied_unpublished(&mut self, step: &Step) {
        self.applied = Some(step.last());
    }

    pub fn publish(&self) {
        self.due.set(self.applied);
    }

    pub fn applied_through(&mut self, hour: Hour) {
        self.applied = Some(hour);
        self.due.set(self.applied);
    }

    pub fn receive(&mut self, stamp: Option<Hour>, product: viewer_runtime::landed::Landed<P>) {
        self.early.push_back((stamp, product));
    }

    pub fn take(&mut self, mut each: impl FnMut(viewer_runtime::landed::Landed<P>)) {
        while self.early.front().is_some_and(|(stamp, _)| *stamp <= self.applied) {
            let (stamp, product) = self.early.pop_front().expect("held");
            (self.taken, self.stamp) = (true, self.stamp.max(stamp));
            each(product);
        }
    }

    pub fn arrived(&self) -> bool {
        let newest = self.early.back().map(|(stamp, _)| *stamp).or(self.taken.then_some(self.stamp));
        newest.is_some_and(|stamp| stamp >= self.applied)
    }

    pub fn published(&self) -> Published {
        if !self.taken {
            return Published::Nothing;
        }
        if self.stamp == self.applied {
            return Published::Current;
        }
        self.stamp.or(self.composed).map_or(Published::Nothing, Published::At)
    }

    pub fn made_through(&self) -> Option<Hour> {
        self.early.back().map(|(stamp, _)| *stamp).or(self.taken.then_some(self.stamp)).flatten()
    }

    pub fn shown(&self) -> Option<Hour> {
        if self.taken { self.stamp.or(self.composed) } else { None }
    }
}

#[derive(Clone)]
pub struct Due(Arc<AtomicI64>);

impl Default for Due {
    fn default() -> Self {
        Self(Arc::new(AtomicI64::new(i64::MIN)))
    }
}

impl Due {
    fn set(&self, hour: Option<Hour>) {
        self.0.store(hour.map_or(i64::MIN, i64::from), Ordering::Release);
    }

    pub fn reached(&self, last: Hour) -> bool {
        self.0.load(Ordering::Acquire) >= i64::from(last)
    }

    pub fn hour(&self) -> Option<Hour> {
        let hour = self.0.load(Ordering::Acquire);
        (hour != i64::MIN).then_some(hour as Hour)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Carried {
    pub entities: usize,
}

static IN_VIEW: AtomicBool = AtomicBool::new(false);

pub fn count_in_view(on: bool) {
    IN_VIEW.store(on, Ordering::Relaxed);
}

fn counting_in_view() -> bool {
    IN_VIEW.load(Ordering::Relaxed)
}

#[derive(Clone, Copy)]
pub struct SightInputs<'a> {
    pub scene: &'a crate::scene_content::SceneContent,
    pub state_provinces: &'a BTreeMap<u32, Vec<u32>>,
}

pub struct Sight<'a> {
    admission: Option<Admission>,
    content: Option<SightInputs<'a>>,
    eye: Option<[f32; 3]>,
}

impl<'a> Sight<'a> {
    pub fn none() -> Self {
        Self { admission: None, content: None, eye: None }
    }

    pub fn of(content: Option<SightInputs<'a>>, view: &View) -> Self {
        let (Some(content), Some(placement)) = (content, view.camera) else { return Self::none() };
        if view.viewport == [0, 0] || !counting_in_view() {
            return Self::none();
        }
        let camera = placement.on(MapSize::of(&content.scene.map), content.scene.map_defines);
        let eye = camera.eye();
        Self { admission: Some(Admission::new(camera, view.viewport)), content: Some(content), eye: Some(eye) }
    }

    pub fn province(&self, province: u32) -> Option<bool> {
        Some(self.admission.as_ref()?.admits_province(&self.content?.scene.counters.bounds, province))
    }

    pub fn state(&self, state: u32) -> Option<bool> {
        let provinces = self.content?.state_provinces.get(&state);
        self.admission.as_ref()?;
        Some(provinces.is_some_and(|provinces| provinces.iter().any(|&province| self.province(province) == Some(true))))
    }

    pub fn heard(&self, at: [f32; 2], reach: f32, hearing: Option<f32>) -> Option<bool> {
        let (eye, hearing) = (self.eye?, hearing?);
        let [dx, dz] = [eye[0] - at[0], eye[2] - at[1]];
        Some(dx * dx + dz * dz <= (hearing + reach) * (hearing + reach))
    }

    pub fn province_heard(&self, province: u32, hearing: Option<f32>) -> Option<bool> {
        let (eye, hearing) = (self.eye?, hearing?);
        let Some(&[min_x, min_z, max_x, max_z]) = self.content?.scene.counters.bounds.get(&province) else { return Some(false) };
        let [dx, dz] = [eye[0] - eye[0].clamp(min_x, max_x), eye[2] - eye[2].clamp(min_z, max_z)];
        Some(dx * dx + dz * dz <= hearing * hearing)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scope {
    pub settled: u64,
    pub in_view: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Scoped {
    settled: u64,
    provinces: Vec<u32>,
    states: Vec<u32>,
    heard: Vec<([f32; 2], f32)>,
    heard_provinces: Vec<u32>,
}

impl Scoped {
    pub fn province(&mut self, province: u32) {
        self.settled += 1;
        if counting_in_view() {
            self.provinces.push(province);
        }
    }

    pub fn state(&mut self, state: u32) {
        self.settled += 1;
        if counting_in_view() {
            self.states.push(state);
        }
    }

    pub fn heard(&mut self, at: [f32; 2], reach: f32) {
        self.settled += 1;
        if counting_in_view() {
            self.heard.push((at, reach));
        }
    }

    pub fn heard_province(&mut self, province: u32) {
        self.settled += 1;
        if counting_in_view() {
            self.heard_provinces.push(province);
        }
    }

    pub fn unplaced(&mut self) {
        self.settled += 1;
    }

    pub fn take(&mut self, sight: &Sight<'_>, hearing: Option<f32>) -> Scope {
        let counted = sight.admission.is_some();
        let seen = self
            .provinces
            .iter()
            .map(|&province| sight.province(province))
            .chain(self.states.iter().map(|&state| sight.state(state)))
            .chain(self.heard.iter().map(|&(at, reach)| sight.heard(at, reach, hearing)))
            .chain(self.heard_provinces.iter().map(|&province| sight.province_heard(province, hearing)))
            .try_fold(0u64, |count, seen| seen.map(|seen| count + u64::from(seen)));
        let scope = Scope { settled: self.settled, in_view: seen.filter(|_| counted) };
        self.settled = 0;
        self.provinces.clear();
        self.states.clear();
        self.heard.clear();
        self.heard_provinces.clear();
        scope
    }

    pub fn bytes(&self) -> usize {
        (self.provinces.capacity() + self.states.capacity() + self.heard_provinces.capacity()) * 4 + self.heard.capacity() * 12
    }

    pub fn extend(&mut self, other: Self) {
        self.settled += other.settled;
        self.provinces.extend(other.provinces);
        self.states.extend(other.states);
        self.heard.extend(other.heard);
        self.heard_provinces.extend(other.heard_provinces);
    }
}

#[derive(Debug, Default)]
pub struct Dirty {
    marked: Vec<u64>,
    list: Vec<(u32, u64)>,
}

impl Dirty {
    pub fn mark(&mut self, entity: u32, frame: u64) -> bool {
        let word = entity as usize / 64;
        if word >= self.marked.len() {
            self.marked.resize(word + 1, 0);
        }
        let bit = 1 << (entity % 64);
        if self.marked[word] & bit != 0 {
            return false;
        }
        self.marked[word] |= bit;
        self.list.push((entity, frame));
        true
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn since(&self) -> Option<u64> {
        self.list.first().map(|&(_, frame)| frame)
    }

    pub fn settle(&mut self, mut each: impl FnMut(u32) -> io::Result<()>) -> io::Result<()> {
        let mut done = 0;
        while done < self.list.len() {
            let (entity, _) = self.list[done];
            self.marked[entity as usize / 64] &= !(1 << (entity % 64));
            done += 1;
            each(entity)?;
        }
        self.list.drain(..done);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.list.iter().for_each(|&(entity, _)| self.marked[entity as usize / 64] = 0);
        self.list.clear();
    }

    pub fn bytes(&self) -> usize {
        self.marked.capacity() * 8 + self.list.capacity() * 12
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeptAt {
    Unread,
    Unindexed,
    At(Hour, viewer_engagements::engagement_history::Provenance),
}

impl KeptAt {
    pub fn of(index: Option<(&Arc<History>, replay_data::BranchId)>, head: Option<Hour>, viewer: u32) -> Self {
        match (index, head) {
            (Some((history, branch)), Some(head)) => Self::At(head, history.provenance(branch, head, viewer)),
            _ => Self::Unindexed,
        }
    }

    pub fn advance(self, now: Self, index: Option<(&Arc<History>, replay_data::BranchId)>, kind: usize, viewer: u32) -> Option<Vec<u64>> {
        match (self, now) {
            (Self::Unindexed, Self::Unindexed) => Some(Vec::new()),
            (Self::At(from, held), Self::At(head, found)) if held == found => match found {
                viewer_engagements::engagement_history::Provenance::Indexed => {
                    let (history, branch) = index?;
                    history.icons_changed(kind, branch, from, head, viewer)
                }
                _ => Some(Vec::new()),
            },
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Keys {
    list: Vec<(u64, u64)>,
    held: std::collections::HashSet<u64>,
}

impl Keys {
    pub fn mark(&mut self, key: u64, frame: u64) -> bool {
        if !self.held.insert(key) {
            return false;
        }
        self.list.push((key, frame));
        true
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn take(&mut self) -> Vec<u64> {
        let taken: Vec<u64> = self.list.drain(..).map(|(key, _)| key).collect();
        taken.iter().for_each(|key| {
            self.held.remove(key);
        });
        taken
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.held.clear();
    }

    pub fn bytes(&self) -> usize {
        self.list.capacity() * 16 + self.held.capacity() * 16
    }
}

#[derive(Debug)]
pub struct Num<T> {
    pub table: TableId,
    pub column: usize,
    kind: PhantomData<fn() -> T>,
}

#[cfg(any(test, feature = "test-support"))]
impl<T> Num<T> {
    pub fn of(table: TableId, column: usize) -> Self {
        Self { table, column, kind: PhantomData }
    }
}

impl<T> Clone for Num<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Num<T> {}

#[derive(Clone, Copy, Debug)]
pub struct Text {
    pub table: TableId,
    pub column: usize,
}

pub fn moved_provinces(
    step: &Step,
    mirrors: &Mirrors,
    table: TableId,
    province: usize,
    unit: impl Fn(u64) -> viewer_runtime::unit_handle::Unit,
    into: &mut viewer_unit_reads::counter_move::MovedHours,
) {
    let hours = step.hours_of(table.index());
    if hours.is_empty() {
        return;
    }
    let Some(mirror) = mirrors.get(table) else { return };
    let lived: std::collections::BTreeSet<u128> = step
        .table(table.index())
        .into_iter()
        .flat_map(|changes| {
            changes
                .deleted()
                .keys()
                .unwrap_or_default()
                .iter()
                .chain(changes.inserted().keys().unwrap_or_default())
                .copied()
        })
        .collect();
    for set in hours {
        let Some(hour) = set.head() else { continue };
        for held in set.columns().iter().filter(|held| held.column() == province) {
            held.each(|slot, value| {
                if let (Some(key), replay_columnar::store::Value::Number(bits)) = (mirror.key(slot).filter(|_| mirror.live(slot)), value)
                    && !lived.contains(&key)
                {
                    into.note(unit(key as u64), hour, bits as u32);
                }
            });
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct List {
    pub table: TableId,
    pub column: usize,
}

pub trait Number: Copy {
    const KINDS: &'static [Kind];
    fn from_raw(raw: u64) -> Self;
}

macro_rules! number {
    ($($type:ty => [$($kind:ident),+], |$raw:ident| $from:expr;)+) => {
        $(impl Number for $type {
            const KINDS: &'static [Kind] = &[$(Kind::$kind),+];
            #[inline]
            fn from_raw($raw: u64) -> Self {
                $from
            }
        })+
    };
}

number! {
    bool => [Bool], |raw| raw != 0;
    u8 => [U8], |raw| raw as u8;
    i8 => [I8], |raw| raw as u8 as i8;
    u16 => [U16], |raw| raw as u16;
    i16 => [I16], |raw| raw as u16 as i16;
    u32 => [U32, OptionalU32], |raw| raw as u32;
    i32 => [I32, OptionalI32], |raw| raw as u32 as i32;
    u64 => [U64, OptionalU64], |raw| raw;
    i64 => [I64, OptionalI64], |raw| raw as i64;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    pub columns: Vec<usize>,
    pub keyed: bool,
    pub hourly: bool,
}

pub struct Inputs<'a> {
    kinds: &'a [Vec<Kind>],
    layer: &'static str,
    tables: BTreeMap<usize, Read>,
    after: Vec<&'static str>,
}

impl<'a> Inputs<'a> {
    pub fn new(kinds: &'a [Vec<Kind>], layer: &'static str) -> Self {
        Self { kinds, layer, tables: BTreeMap::new(), after: Vec::new() }
    }

    pub fn table(&mut self, table: TableId, keyed: bool) -> io::Result<()> {
        self.kinds_of(table)?;
        let entry = self.tables.entry(table.index()).or_default();
        entry.keyed |= keyed;
        Ok(())
    }

    pub fn hourly(&mut self, table: TableId) -> io::Result<()> {
        self.kinds_of(table)?;
        self.tables.entry(table.index()).or_default().hourly = true;
        Ok(())
    }

    pub fn after(&mut self, name: &'static str) {
        if !self.after.contains(&name) {
            self.after.push(name);
        }
    }

    pub fn upstream(&self) -> &[&'static str] {
        &self.after
    }

    pub fn number<T: Number>(&mut self, table: TableId, field: &str) -> io::Result<Num<T>> {
        let (column, kind) = self.column(table, field)?;
        if !T::KINDS.contains(&kind) {
            return Err(self.refuse(format!("{table:?}.{field} is a {kind:?} column, not a {}", std::any::type_name::<T>())));
        }
        Ok(Num { table, column, kind: PhantomData })
    }

    pub fn text(&mut self, table: TableId, field: &str) -> io::Result<Text> {
        let (column, kind) = self.column(table, field)?;
        if kind != Kind::Bytes {
            return Err(self.refuse(format!("{table:?}.{field} is a {kind:?} column, not text")));
        }
        Ok(Text { table, column })
    }

    pub fn list(&mut self, table: TableId, field: &str) -> io::Result<List> {
        let (column, kind) = self.column(table, field)?;
        if !matches!(kind, Kind::U32List | Kind::U64List | Kind::Opaque) {
            return Err(self.refuse(format!("{table:?}.{field} is a {kind:?} column, not a list")));
        }
        Ok(List { table, column })
    }

    pub fn index(&mut self, table: TableId, column: usize) -> io::Result<()> {
        let kinds = self.kinds_of(table)?;
        if column >= kinds.len() {
            return Err(self.refuse(format!("{table:?} holds no column {column}")));
        }
        self.select(table, column);
        Ok(())
    }

    fn column(&mut self, table: TableId, field: &str) -> io::Result<(usize, Kind)> {
        let kinds = self.kinds_of(table)?;
        let column = (0..kinds.len())
            .find(|&column| table.field_name(column) == Some(field))
            .ok_or_else(|| self.refuse(format!("it reads {table:?}.{field}, which this recording's schema does not hold")))?;
        let kind = kinds[column];
        self.select(table, column);
        Ok((column, kind))
    }

    fn select(&mut self, table: TableId, column: usize) {
        let entry = self.tables.entry(table.index()).or_default();
        if let Err(at) = entry.columns.binary_search(&column) {
            entry.columns.insert(at, column);
        }
    }

    fn kinds_of(&self, table: TableId) -> io::Result<&'a [Kind]> {
        let kinds = self.kinds;
        kinds.get(table.index()).filter(|kinds| !kinds.is_empty()).map(Vec::as_slice).ok_or_else(|| self.refuse(format!("it reads {table:?}, which this recording's schema does not hold")))
    }

    fn refuse(&self, why: String) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, format!("The {} layer is refused: {why}", self.layer))
    }

    pub fn into_tables(self) -> BTreeMap<usize, Read> {
        self.tables
    }

    pub fn selected(&self) -> BTreeMap<usize, Vec<usize>> {
        self.tables.iter().map(|(&table, read)| (table, read.columns.clone())).collect()
    }
}
