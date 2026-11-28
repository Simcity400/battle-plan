use viewer_layer_core::playhead_host::{lock, views::{Need as RowNeed, self}, timeline::{self, Mark}};
use crate::{document::{Content, OverlayRows, OverlayTables, PreparedOverlay}, map_open::Products, side::{LayInKeys, LayIns}, map_views::{CounterView, Presentation}};
use viewer_layer_core::{playhead_host::{LayerParts, OverlayStep, Overlaying}, layers::{Layer, Settling}};
use viewer_layers_counters::counter_kinds;
use viewer_layers_political::left_to_layers::LeftToLayers;
use replay_data::{BranchId, Hour, rows::TableId};
use replay_frame::frame::DecodedFrame;
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_counter_kit::Kinds;
use viewer_counters::counters::{Need, Shown, layered::Layered};
use viewer_engagements::engagement_history::History;
use viewer_unit_reads::counter_move::MovedHours;
use viewer_gpu::camera::Camera;
use viewer_foundation::perspective::{Choice, Vision};
use viewer_runtime::landed::{Landed, Published, PublishedKeyed};

const WHOLE: Duration = Duration::from_secs(120);

const LOOK: Duration = Duration::from_millis(2);

const UNIT_COUNTERS: &[TableId] = &[
    TableId::DivisionStatus,
    TableId::DivisionPanelStats,
    TableId::DivisionModelInputs,
    TableId::DivisionTemplateIcons,
    TableId::DivisionTransportSpeeds,
    TableId::ArmyIconInputs,
    TableId::ActiveAbilities,
    TableId::UnitLeaders,
    TableId::LeaderPortraits,
    TableId::LeaderPortraitPaths,
    TableId::CharacterPortraits,
    TableId::OrdersGroups,
    TableId::OrdersGroupPlans,
    TableId::OrdersGroupCounterInputs,
    TableId::ArmyOrderPlans,
    TableId::ArmyOrders,
    TableId::RailCooldowns,
    TableId::RailwayGunCounterInputs,
    TableId::ProvinceUnitOrder,
    TableId::CountryUnitSpeedEntries,
    TableId::MovementStateTerms,
    TableId::CoreSupplyAccess,
    TableId::RailEdges,
    TableId::RiverRelations,
];

const LAND_COMBATS: &[TableId] = &[TableId::LandCombatDivisions, TableId::LandCombatSides, TableId::LandCombatDetails];

const NAVAL_LOOKUPS: &[TableId] = &[TableId::NavalMissionCommanders, TableId::NavalMissionCountries, TableId::ModifierDefinitions];
const AIR_LOOKUPS: &[TableId] = &[TableId::AirCountryRelations];

const NAVAL_MISSIONS: &[TableId] = &[
    TableId::NavalMissionLedgers,
    TableId::NavalMissionTaskForces,
    TableId::NavalDominanceTaskForceRanges,
    TableId::SliceReads,
    TableId::WeatherProvinces,
];

const BASE_ICONS: &[TableId] = &[
    TableId::AirBases,
    TableId::AirBaseRegistrations,
    TableId::AirWings,
    TableId::AirReferenceWitnesses,
    TableId::AirMissionInputs,
    TableId::AirGroundCrews,
    TableId::AirRegionCombat,
    TableId::WingEquipment,
    TableId::FacilityMapFlags,
    TableId::AirSortieBaseCountries,
    TableId::AirSortieModifierMetadata,
    TableId::AirSortieModifiers,
    TableId::AirSortieSpecialBases,
];

const OVERLAY: &[TableId] = &[
    TableId::LandCombats,
    TableId::AceRangeInputs,
    TableId::Ships,
    TableId::ShipModels,
    TableId::TaskForces,
    TableId::TaskForceCounterInputs,
    TableId::TaskForceMissions,
    TableId::Fleets,
    TableId::FleetCounterInputs,
    TableId::NavalBases,
    TableId::NavalCombats,
    TableId::NavalCombatDetails,
    TableId::NavalCombatShips,
    TableId::NavalCombatTransfers,
    TableId::NavalProvinceCombats,
    TableId::NavalResults,
    TableId::NavalResultMembers,
    TableId::NavalResultCountries,
    TableId::NavalTransferUnits,
    TableId::NavalAccidentReports,
    TableId::Capitulation,
    TableId::CountryGameScoreInputs,
    TableId::AirRegionalMissions,
    TableId::VariantDefinitions,
    TableId::RaidAirSites,
    TableId::RaidAirScores,
    TableId::RaidCountryInputs,
    TableId::VolunteerHosts,
    TableId::SentVolunteers,
    TableId::ProvinceBuildings,
    TableId::StateCores,
    TableId::StateFlags,
    TableId::FactionTheatres,
    TableId::WeatherRegions,
    TableId::RadarCoverage,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::CountryCounterInputs,
    TableId::FactionMembers,
    TableId::LexicalTokens,
    TableId::Politics,
    TableId::PoliticalParties,
    TableId::ReservedCountryNaming,
    TableId::Subjects,
    TableId::WarRelations,
    TableId::SimulationFeatures,
    TableId::Provinces,
    TableId::States,
    TableId::StateResistance,
    TableId::Autonomy,
    TableId::Raids,
    TableId::RaidTargets,
    TableId::RaidEquipment,
    TableId::RaidSpeedInputs,
    TableId::AvailableEquipmentVariants,
    TableId::CountryCommandPower,
    TableId::IntelligenceAgencies,
    TableId::IntelNetworks,
    TableId::IntelNetworkAuxiliaries,
    TableId::Operations,
    TableId::OperativeMissions,
    TableId::StrategicOperatives,
    TableId::CharacterOperatives,
    TableId::AgencyOperationBuffers,
    TableId::RaidIntelInputs,
    TableId::CountryStabilityInputs,
    TableId::Characters,
    TableId::GlobalSavedEventTargets,
    TableId::NavalRepairBases,
    TableId::NavalRepairQueue,
    TableId::ScriptVariables,
    TableId::ScriptVariableNames,
    TableId::TaskForceMapMotion,
];

fn overlay_needs() -> Vec<RowNeed> {
    overlay_tables().into_iter().map(RowNeed::every).collect()
}

pub fn overlay_tables() -> Vec<TableId> {
    tables_shown(Shown::EVERY)
}

pub fn tables_shown(shown: Shown) -> Vec<TableId> {
    let family = |held: bool, tables: &'static [TableId]| held.then_some(tables).unwrap_or_default();
    let mut tables: Vec<TableId> = OVERLAY
        .iter()
        .chain(viewer_foundation::perspective::VISION_TABLES)
        .chain(family(shown.units, UNIT_COUNTERS))
        .chain(family(shown.land_combats, LAND_COMBATS))
        .chain(family(shown.naval_missions, NAVAL_MISSIONS))
        .chain(family(shown.naval_missions, NAVAL_LOOKUPS))
        .chain(family(shown.bases, BASE_ICONS))
        .chain(family(shown.bases, AIR_LOOKUPS))
        .copied()
        .collect();
    tables.sort_unstable_by_key(|table| table.index());
    tables.dedup();
    tables
}

#[derive(Clone)]
pub struct Ask {
    pub camera: Camera,
    pub target: Option<f32>,
    pub pixels: [u32; 2],
    pub ui_scale: f32,
    pub view: CounterView,
    pub rosters: Arc<crate::playback::AirRosters>,
}

impl Ask {
    fn need(&self, defines: &viewer_counter_kit::CounterDefines) -> Need {
        Need { air_missions: false, by_key: self.camera.free().is_none(), shown: self.shown(defines) }
    }

    pub fn shown(&self, defines: &viewer_counter_kit::CounterDefines) -> Shown {
        shown(&self.camera, self.target, &self.view, defines)
    }
}

pub fn shown(camera: &Camera, target: Option<f32>, view: &CounterView, defines: &viewer_counter_kit::CounterDefines) -> Shown {
    let height = camera.free().is_none().then_some(camera.height);
    Shown::at(height, target, &view.visibility, defines)
}

pub struct Made {
    pub generation: u64,
    pub hour: Hour,
    pub branch: BranchId,
    pub prepared: PreparedOverlay,
    pub tables: Arc<OverlayTables>,
    pub rows: Arc<DecodedFrame>,
    pub placed_at: Camera,
    pub placed_pixels: [u32; 2],
    pub placed_ui_scale: f32,
    pub on: Arc<Presentation>,
    pub published: Layered,
    pub draws: Kinds,
    pub view: CounterView,
    pub left: LeftToLayers,
    pub lay_ins: Arc<LayIns>,
    pub moved: Arc<MovedHours>,
}

#[derive(Clone, Debug, PartialEq)]
struct Tabled {
    branch: BranchId,
    hour: Hour,
    rows: usize,
    perspective: Choice,
    vision: usize,
    engaged: bool,
    left_out: Kinds,
    labels: bool,
    need: Need,
    raids: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Laid {
    tables: u64,
    view: CounterView,
    pixels: [u32; 2],
    ui_scale: u32,
    layered: [usize; 6],
    lay_ins: LayInKeys,
    left: LeftToLayers,
    art: u64,
}

fn next_ask<T>(asked: &mut Option<T>, published: bool) -> Option<T> {
    if published { None } else { asked.take() }
}

fn shows(built: (u64, u64), generation: u64, taken: u64) -> bool {
    built.0 == generation && built.1 > taken
}

fn installs(laid: (u64, Hour), generation: u64, held: Option<Hour>) -> bool {
    laid.0 == generation && held.is_none_or(|held| held <= laid.1)
}

fn joins_on(whole: bool, answered: bool, out: bool, rows_ready: impl FnOnce() -> bool) -> bool {
    whole && !answered && !out && rows_ready()
}

fn stands(hour: Option<Hour>, missed: usize, head: Hour) -> bool {
    hour.is_some_and(|hour| hour >= head) && missed == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waits {
    Rows,
    Build,
    Give,
}

fn waits(exact: bool, superseded: bool, rows: Option<Hour>, tabled: Option<Hour>) -> Waits {
    if !superseded {
        return Waits::Rows;
    }
    match rows {
        Some(rows) if !exact && tabled.is_none_or(|tabled| tabled < rows) => Waits::Build,
        _ => Waits::Give,
    }
}

fn lays(lay: bool, whole: bool, untaken: bool) -> bool {
    lay && (whole || !untaken)
}

fn note_us(name: &'static str, started: Instant) {
    viewer_runtime::redraws::note(name, u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
}

struct Request<P> {
    generation: u64,
    stamp: u64,
    hour: Hour,
    moved: Arc<MovedHours>,
    whole: bool,
    lay: bool,
    asking: P,
}

impl<P> std::ops::Deref for Request<P> {
    type Target = P;
    fn deref(&self) -> &P {
        &self.asking
    }
}

type Wanted = Request<Asking>;

struct Asking {
    ask: Ask,
    perspective: Choice,
    vision: Arc<Vision>,
    engaged: Option<Arc<History>>,
    labels: bool,
    lay_ins: Arc<LayIns>,
    keys: LayInKeys,
    published: Layered,
    draws: Kinds,
    left: LeftToLayers,
}

fn post<P>(slot: &mut Option<Request<P>>, mut want: Request<P>) {
    if let Some(old) = slot.take() {
        let mut carried = Carried { generation: old.generation, moved: old.moved };
        carried.join(want.generation, &want.moved);
        want.moved = carried.moved;
    }
    *slot = Some(want);
}

#[derive(Default)]
struct Carried {
    generation: u64,
    moved: Arc<MovedHours>,
}

impl Carried {
    fn join(&mut self, generation: u64, moved: &MovedHours) -> bool {
        let fresh = generation != self.generation;
        if fresh {
            *self = Self { generation, moved: Arc::default() };
        }
        if moved.counts().0 > 0 {
            Arc::make_mut(&mut self.moved).extend(moved);
        }
        fresh
    }

    fn take<P>(&mut self, want: &mut Request<P>) -> bool {
        let fresh = self.join(want.generation, &want.moved);
        want.moved = Arc::clone(&self.moved);
        fresh
    }

    fn ended(&mut self, stands: Option<Hour>) {
        if let Some(stands) = stands {
            self.moved = Arc::new(self.moved.after(stands));
        }
    }
}

#[derive(Default)]
struct Owed(Option<u64>);

impl Owed {
    fn lays(&mut self, lay: bool, whole: bool, made: bool, held: impl FnOnce() -> (u64, bool)) -> bool {
        if let Some(owed) = self.0 {
            let (answered, published) = held();
            if answered >= owed && !published && !made {
                self.0 = None;
            }
        }
        lays(lay, whole, made || self.0.is_some())
    }

    fn asked(&mut self, lay: bool, whole: bool, stamp: u64) {
        if lay && !whole {
            self.0 = Some(stamp);
        }
    }
}

fn fails(strict: bool, answered: u64, stamp: u64, failed: bool) -> bool {
    (answered >= stamp && failed) || (strict && answered < stamp)
}

#[derive(Clone, Copy)]
struct Asked {
    stamp: u64,
    hour: Hour,
    lay: bool,
}

struct Built {
    stamp: u64,
    generation: u64,
    hour: Hour,
    tables: Arc<OverlayTables>,
    perspective: Choice,
}

#[derive(Clone, PartialEq, Eq)]
enum OutcomeKey {
    Laid(Hour, bool),
    TablesOnly,
    MissingTables(Vec<TableId>, Vec<String>),
    Refused(Vec<String>),
    Superseded(Vec<String>),
}

enum Outcome {
    Laid { hour: Hour, made: Option<Box<Made>> },
    TablesOnly,
    MissingTables { missed: Vec<TableId>, notes: Vec<String> },
    Refused { reasons: Vec<String> },
    Superseded { notes: Vec<String> },
}

struct Answer {
    generation: u64,
    stamp: u64,
    outcome: Outcome,
}

impl Answer {
    fn key(&self) -> (u64, OutcomeKey) {
        let outcome = match &self.outcome {
            Outcome::Laid { hour, made } => OutcomeKey::Laid(*hour, made.is_some()),
            Outcome::TablesOnly => OutcomeKey::TablesOnly,
            Outcome::MissingTables { missed, notes } => OutcomeKey::MissingTables(missed.clone(), notes.clone()),
            Outcome::Refused { reasons } => OutcomeKey::Refused(reasons.clone()),
            Outcome::Superseded { notes } => OutcomeKey::Superseded(notes.clone()),
        };
        (self.generation, outcome)
    }
}

enum OverlayState {
    Ready { hour: Option<Hour>, notes: Vec<String>, missed: Vec<TableId> },
    Refused { stamp: u64, reasons: Vec<String>, missed: Vec<TableId> },
}

impl Default for OverlayState {
    fn default() -> Self {
        Self::Ready { hour: None, notes: Vec::new(), missed: Vec::new() }
    }
}

fn skips_refused(whole: bool, suppressed: bool, state: &OverlayState, stamp: u64, same_as_last: bool) -> bool {
    !whole
        && !suppressed
        && same_as_last
        && matches!(state, OverlayState::Refused { stamp: refused, missed, .. } if *refused == stamp && missed.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Offered {
    keys: LayInKeys,
    raids: Option<usize>,
    published: [usize; 6],
    draws: Kinds,
    left: LeftToLayers,
    labels: bool,
}

impl Offered {
    fn of(layers: LayerParts<'_>, branch: Option<BranchId>) -> Self {
        let ours = viewer_layer_core::vision::drawn(layers).and_then(viewer_layer_core::vision::Vision::ours);
        Self {
            keys: LayInKeys::of(layers),
            raids: branch.zip(ours).and_then(|(branch, ours)| viewer_layers_political::scripts::raids(layers, branch, ours)).map(|icons| Arc::as_ptr(&*icons) as usize),
            published: counter_kinds::published(layers).identity(),
            draws: counter_kinds::draws(layers),
            left: LeftToLayers::of(Some(layers)),
            labels: layers.draws("labels"),
        }
    }
}

#[derive(Clone)]
struct AskedInput {
    hour: Hour,
    lay: bool,
    camera: Camera,
    target: Option<u32>,
    pixels: [u32; 2],
    ui_scale: u32,
    view: CounterView,
    perspective: Choice,
    offered: Offered,
    art: u64,
}

impl AskedInput {
    fn same(&self, other: &Self) -> bool {
        self.hour == other.hour
            && self.lay == other.lay
            && self.camera == other.camera
            && self.target == other.target
            && self.pixels == other.pixels
            && self.ui_scale == other.ui_scale
            && self.view == other.view
            && self.perspective == other.perspective
            && self.offered == other.offered
            && self.art == other.art
    }
}

#[derive(Default)]
struct Held {
    stop: bool,
    want: Option<Wanted>,
    built: Published<Option<Built>>,
    answer: PublishedKeyed<(u64, OutcomeKey), Answer>,
    answered_stamp: (u64, u64),
    waiting_note: Option<String>,
}

struct Shared {
    held: Mutex<Held>,
    signal: Condvar,
}

pub struct Lend<'a> {
    pub views: &'a views::Store,
    pub applied: Option<Hour>,
    pub branch: Option<BranchId>,
    pub perspective: &'a Choice,
    pub vision: Result<Arc<Vision>, String>,
    pub engaged: Option<Arc<History>>,
    pub draws_labels: bool,
    pub lay_ins: Arc<LayIns>,
    pub keys: LayInKeys,
    pub published: Layered,
    pub draws: Kinds,
    pub left: LeftToLayers,
    pub moved: Arc<MovedHours>,
    pub whole: bool,
    pub lay: bool,
}

pub struct Overlay {
    content: Arc<Content>,
    ask: Option<Ask>,
    shared: Arc<Shared>,
    rows: Option<Arc<views::Reader>>,
    worker: Option<JoinHandle<()>>,
    generation: u64,
    stamp: u64,
    asked: Option<Asked>,
    tables: Option<Arc<OverlayTables>>,
    tables_for: Option<Choice>,
    stands: Option<Hour>,
    taken: u64,
    made: Option<Landed<Made>>,
    lay_owed: Owed,
    state: OverlayState,
    applied_stamp: u64,
    ask_notes: Vec<String>,
    waiting_note: Option<String>,
    loading: Vec<String>,
    posted: bool,
    suppressed: bool,
    art: u64,
    last_asked: Option<AskedInput>,
    offered: Option<Offered>,
    maker: timeline::Maker,
}

impl Overlay {
    pub fn new(content: Arc<Content>) -> Self {
        Self {
            content,
            ask: None,
            shared: Arc::new(Shared { held: Mutex::new(Held::default()), signal: Condvar::new() }),
            rows: None,
            worker: None,
            generation: 0,
            stamp: 0,
            asked: None,
            tables: None,
            tables_for: None,
            stands: None,
            taken: 0,
            made: None,
            lay_owed: Owed::default(),
            state: OverlayState::default(),
            applied_stamp: 0,
            ask_notes: Vec::new(),
            waiting_note: None,
            loading: Vec::new(),
            posted: false,
            suppressed: false,
            art: 0,
            last_asked: None,
            offered: None,
            maker: timeline::maker("overlay"),
        }
    }

    pub fn ask(&mut self, ask: Ask) {
        self.ask = Some(ask);
    }

    pub fn reset(&mut self) {
        self.generation += 1;
        let mut held = lock(&self.shared.held);
        held.want = None;
        held.built.clear();
        held.answer.clear();
        held.answered_stamp = (self.generation, 0);
        held.waiting_note = None;
        drop(held);
        self.shared.signal.notify_all();
        (self.tables, self.tables_for, self.stands, self.asked, self.lay_owed) = (None, None, None, None, Owed::default());
        (self.made, self.state, self.applied_stamp) = (None, OverlayState::default(), 0);
        (self.posted, self.suppressed, self.last_asked, self.offered) = (false, false, None, None);
        self.ask_notes.clear();
        self.waiting_note = None;
        self.loading.clear();
    }

    pub fn take(&mut self) -> Option<Landed<Made>> {
        let answer = lock(&self.shared.held).answer.take();
        if let Some(answer) = answer {
            self.apply_answer(answer);
        }
        let made = self.made.take();
        if let Some(made) = &made {
            self.lay_owed = Owed::default();
            if let OverlayState::Ready { hour, .. } = &mut self.state {
                *hour = Some(made.hour);
            }
        }
        made
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn hour(&self) -> Option<Hour> {
        match &self.state {
            OverlayState::Ready { hour, .. } => *hour,
            OverlayState::Refused { .. } => None,
        }
    }

    pub fn refused(&self) -> bool {
        matches!(self.state, OverlayState::Refused { .. })
    }

    pub fn loading(&self) -> &[String] {
        &self.loading
    }

    pub fn missed(&self) -> &[TableId] {
        match &self.state {
            OverlayState::Ready { missed, .. } | OverlayState::Refused { missed, .. } => missed,
        }
    }

    pub fn needs_reask(&self) -> bool {
        if !self.posted {
            return false;
        }
        let wants_suppressed = self.suppressed && self.made.is_none();
        let wants_retry = !self.missed().is_empty() || self.content.icons.lazy.version() != self.art;
        if !wants_suppressed && !wants_retry {
            return false;
        }
        let held = lock(&self.shared.held);
        let answered = if held.answered_stamp.0 == self.generation { held.answered_stamp.1 } else { 0 };
        if answered < self.stamp {
            return false;
        }
        if wants_retry {
            return true;
        }
        !held.answer.as_ref().is_some_and(|answer| answer.generation == self.generation && matches!(answer.outcome, Outcome::Laid { made: Some(_), .. }))
    }

    pub fn offered_stale(&self, layers: LayerParts<'_>, branch: Option<BranchId>) -> bool {
        self.offered.as_ref().is_some_and(|offered| offered != &Offered::of(layers, branch))
    }

    pub fn tables(&self) -> Option<&Arc<OverlayTables>> {
        self.tables.as_ref()
    }

    pub fn tables_of(&self, perspective: &Choice) -> Option<&Arc<OverlayTables>> {
        self.tables.as_ref().filter(|_| self.tables_for.as_ref() == Some(perspective))
    }

    pub fn rosters(&self) -> Option<Arc<crate::playback::AirRosters>> {
        self.ask.as_ref().map(|ask| Arc::clone(&ask.rosters))
    }

    pub fn stands_at(&self, hour: Hour) -> bool {
        stands(self.hour(), self.missed().len(), hour)
    }

    pub fn ask_step(&mut self, lend: Lend<'_>) -> bool {
        let Lend { views, applied, branch, perspective, vision, engaged, draws_labels, lay_ins, keys, published, draws, left, moved, whole, lay } = lend;
        self.ask_notes.clear();
        let (built, answer) = {
            let mut held = lock(&self.shared.held);
            (held.built.take(), held.answer.take())
        };
        if let Some(built) = built {
            self.install(built.into_inner());
        }
        if let Some(answer) = answer {
            self.apply_answer(answer);
        }
        let wanted_lay = lay;
        if !lay {
            self.made = None;
        }
        let shared = &self.shared;
        let generation = self.generation;
        let lay = self.lay_owed.lays(lay, whole, self.made.is_some(), || {
            let held = lock(&shared.held);
            let answered = if held.answered_stamp.0 == generation { held.answered_stamp.1 } else { 0 };
            (answered, held.answer.as_ref().is_some_and(|answer| answer.generation == generation && matches!(answer.outcome, Outcome::Laid { made: Some(_), .. })))
        });
        let (Some(ask), Some(hour), true) = (self.ask.clone(), applied, branch.is_some()) else {
            (self.offered, self.posted, self.suppressed, self.last_asked) = (None, false, false, None);
            self.rebuild_loading();
            return false;
        };
        let vision = match vision {
            Ok(vision) => vision,
            Err(why) => {
                (self.offered, self.posted, self.suppressed, self.last_asked) = (None, false, false, None);
                self.ask_notes.push(format!("the overlay's vision: {why}"));
                self.rebuild_loading();
                return false;
            }
        };
        let art = self.content.icons.lazy.version();
        let offered = Offered {
            keys: keys.clone(),
            raids: lay_ins.raids.as_ref().map(|icons| Arc::as_ptr(&**icons) as usize),
            published: published.identity(),
            draws,
            left,
            labels: draws_labels,
        };
        let input = AskedInput {
            hour,
            lay: wanted_lay,
            camera: ask.camera,
            target: ask.target.map(f32::to_bits),
            pixels: ask.pixels,
            ui_scale: ask.ui_scale.to_bits(),
            view: ask.view.clone(),
            perspective: perspective.clone(),
            offered: offered.clone(),
            art,
        };
        let same_as_last = self.last_asked.as_ref().is_some_and(|prev| prev.same(&input));
        if skips_refused(whole, self.suppressed, &self.state, self.stamp, same_as_last) {
            self.rebuild_loading();
            return false;
        }
        self.start(views);
        if self.worker.is_none() {
            self.rebuild_loading();
            return false;
        }
        let refused_current = matches!(self.state, OverlayState::Refused { stamp, ref missed, .. } if stamp == self.stamp && missed.is_empty());
        let clearing_refusal = self.refused() && (!same_as_last || self.suppressed || !refused_current);
        if clearing_refusal {
            self.state = OverlayState::default();
        }
        self.stamp += 1;
        self.art = art;
        self.offered = Some(offered);
        self.last_asked = Some(input);
        let want = Wanted {
            generation: self.generation,
            stamp: self.stamp,
            hour,
            moved,
            whole,
            lay,
            asking: Asking { ask, perspective: perspective.clone(), vision, engaged, labels: draws_labels, lay_ins, keys, published, draws, left },
        };
        {
            let mut held = lock(&self.shared.held);
            if clearing_refusal {
                held.answer.forget_key();
            }
            post(&mut held.want, want);
        }
        self.shared.signal.notify_all();
        self.lay_owed.asked(lay, whole, self.stamp);
        self.posted = true;
        self.suppressed = wanted_lay && !lay;
        self.asked = Some(Asked { stamp: self.stamp, hour, lay });
        self.rebuild_loading();
        timeline::mark(self.maker, Mark::Asked);
        true
    }

    fn start(&mut self, store: &views::Store) {
        if self.worker.is_some() {
            return;
        }
        let tables = self.content.tables;
        store.use_columns(TableId::VariantDefinitions);
        let rows = Arc::new(store.reader("overlay", &overlay_needs()));
        let maker = Maker {
            content: Arc::clone(&self.content),
            shared: Arc::clone(&self.shared),
            rows: Arc::clone(&rows),
            declared: overlay_tables(),
            learned: Vec::new(),
            shown: Shown::EVERY,
            tables: None,
            tabled: None,
            tabled_count: 0,
            published_tables: None,
            laying: None,
            laid: None,
            left_out: Kinds::default(),
            lay_ins: Arc::default(),
            lay_in_keys: LayInKeys::default(),
            carried: Carried::default(),
            marks: self.maker,
        };
        let started = std::thread::Builder::new().name("map overlay".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            if tables.is_some() {
                engine_executable::executable_tables::enter(tables);
            }
            let mut maker = maker;
            maker.work();
        });
        match started {
            Ok(worker) => (self.worker, self.rows) = (Some(worker), Some(rows)),
            Err(error) => self.ask_notes.push(format!("the overlay's maker cannot start: {error}")),
        }
    }

    pub fn join(&mut self, settling: Settling) -> bool {
        let Some(Asked { stamp, hour, lay }) = self.asked else { return false };
        let began = Instant::now();
        timeline::mark(self.maker, Mark::JoinBegan);
        let until = began + WHOLE;
        let shared = Arc::clone(&self.shared);
        let mut held = lock(&shared.held);
        loop {
            if let Some(built) = held.built.take() {
                self.install(built.into_inner());
            }
            if let Some(answer) = held.answer.take() {
                self.apply_answer(answer);
            }
            let now = Instant::now();
            let answered = if held.answered_stamp.0 == self.generation { held.answered_stamp.1 } else { 0 };
            if held.stop || !joins_on(settling.strict(), answered >= stamp, now >= until, || {
                self.rows.as_ref().is_some_and(|rows| rows.has_rows_at(hour))
            }) {
                break;
            }
            held = shared.signal.wait_timeout(held, until - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
        timeline::mark(self.maker, Mark::Waited);
        let answered = if held.answered_stamp.0 == self.generation { held.answered_stamp.1 } else { 0 };
        self.waiting_note.clone_from(&held.waiting_note);
        drop(held);
        self.rebuild_loading();
        let failed = fails(settling.strict(), answered, stamp, self.refused() || !self.missed().is_empty());
        shared.signal.notify_all();
        timeline::mark(self.maker, Mark::Joined);
        self.record_join(began, hour);
        (!settling.strict() && self.refused()) || (!failed && self.stands.is_some_and(|stands| stands >= hour) && self.missed().is_empty() && (!lay || self.stands_at(hour)))
    }

    fn record_join(&self, began: Instant, hour: Hour) {
        if !viewer_runtime::view_record::on() {
            return;
        }
        let at = self.stands.is_some_and(|stands| stands >= hour);
        viewer_runtime::view_record::add("overlay", "joins", 1);
        viewer_runtime::view_record::add("overlay", "join_us", began.elapsed().as_micros() as u64);
        viewer_runtime::view_record::add("overlay", if at { "joins_at_the_hour" } else { "joins_behind" }, 1);
        if let Some(behind) = self.hour().map(|held| hour.saturating_sub(held)).filter(|&behind| behind > 0) {
            viewer_runtime::view_record::set("overlay", "hours_behind", u64::try_from(behind).unwrap_or(0));
        }
    }

    fn install(&mut self, built: Built) {
        if !shows((built.generation, built.stamp), self.generation, self.taken) {
            return;
        }
        self.taken = built.stamp;
        self.tables = Some(built.tables);
        self.tables_for = Some(built.perspective);
        self.stands = Some(built.hour);
    }

    fn apply_answer(&mut self, answer: Landed<Answer>) {
        if answer.generation != self.generation || answer.stamp < self.applied_stamp {
            return;
        }
        self.applied_stamp = answer.stamp;
        if matches!(answer.outcome, Outcome::Laid { made: Some(_), .. }) {
            let hour = match &answer.outcome {
                Outcome::Laid { hour, .. } => *hour,
                _ => unreachable!(),
            };
            let made = answer.map(|answer| match answer.outcome {
                Outcome::Laid { made: Some(made), .. } => *made,
                _ => unreachable!(),
            });
            if installs((made.generation, hour), self.generation, self.hour()) {
                self.made = Some(made);
                self.state = OverlayState::Ready { hour: Some(hour), notes: Vec::new(), missed: Vec::new() };
            }
            self.rebuild_loading();
            return;
        }
        let answer = answer.into_inner();
        match answer.outcome {
            Outcome::Laid { hour, made: None } => {
                if !self.refused() && installs((answer.generation, hour), self.generation, self.hour()) {
                    self.state = OverlayState::Ready { hour: Some(hour), notes: Vec::new(), missed: Vec::new() };
                }
            }
            Outcome::Laid { made: Some(_), .. } => unreachable!(),
            Outcome::TablesOnly => match &mut self.state {
                OverlayState::Ready { notes, missed, .. } => {
                    notes.clear();
                    missed.clear();
                }
                OverlayState::Refused { missed, .. } => {
                    missed.clear();
                }
            },
            Outcome::MissingTables { missed, notes } => match &mut self.state {
                OverlayState::Ready { notes: held, missed: held_missed, .. } => {
                    *held = notes;
                    *held_missed = missed;
                }
                OverlayState::Refused { missed: held_missed, .. } => {
                    *held_missed = missed;
                }
            },
            Outcome::Refused { reasons } => {
                self.made = None;
                self.lay_owed = Owed::default();
                if answer.stamp == self.stamp {
                    self.suppressed = false;
                }
                self.state = OverlayState::Refused { stamp: answer.stamp, reasons, missed: Vec::new() };
            }
            Outcome::Superseded { notes } => {
                if let OverlayState::Ready { notes: held, .. } = &mut self.state {
                    *held = notes;
                }
            }
        }
        self.rebuild_loading();
    }

    fn rebuild_loading(&mut self) {
        self.loading.clear();
        self.loading.extend(self.ask_notes.iter().cloned());
        match &self.state {
            OverlayState::Ready { notes, .. } => self.loading.extend(notes.iter().cloned()),
            OverlayState::Refused { reasons, .. } => self.loading.extend(reasons.iter().cloned()),
        }
        if let Some(note) = &self.waiting_note {
            self.loading.push(note.clone());
        }
    }

    pub fn stepped(&mut self) {
        self.asked = None;
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        lock(&self.shared.held).stop = true;
        self.shared.signal.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Gave {
    Superseded,
    MissingTables(Vec<TableId>),
    Refused(String),
}

enum LayOutcome {
    Laid(Option<Box<Made>>),
    MissingTables(Vec<TableId>),
    Refused(String),
}

enum TablesBuilt {
    Ready,
    Missing(Vec<TableId>),
    Refused,
}

struct Maker {
    content: Arc<Content>,
    shared: Arc<Shared>,
    rows: Arc<views::Reader>,
    declared: Vec<TableId>,
    learned: Vec<TableId>,
    shown: Shown,
    tables: Option<OverlayTables>,
    tabled: Option<Tabled>,
    tabled_count: u64,
    published_tables: Option<(u64, u64, Hour, Choice)>,
    laying: Option<(usize, Arc<Presentation>)>,
    laid: Option<Laid>,
    left_out: Kinds,
    lay_ins: Arc<LayIns>,
    lay_in_keys: LayInKeys,
    carried: Carried,
    marks: timeline::Maker,
}

impl Maker {
    fn work(&mut self) {
        loop {
            let want = {
                let shared = Arc::clone(&self.shared);
                let mut held = lock(&shared.held);
                loop {
                    if held.stop {
                        return;
                    }
                    let published = held.built.is_some();
                    if let Some(want) = next_ask(&mut held.want, published) {
                        break want;
                    }
                    held = shared.signal.wait_timeout(held, LOOK).unwrap_or_else(std::sync::PoisonError::into_inner).0;
                }
            };
            self.cycle(want);
        }
    }

    fn cycle(&mut self, mut want: Wanted) {
        if self.carried.take(&mut want) {
            (self.tables, self.tabled, self.laid, self.published_tables) = (None, None, None, None);
        }
        let mut loading = Vec::new();
        let made = self.make(&want, &mut loading);
        let laid_stands = match &made {
            Ok((stands, Some(LayOutcome::Laid(_)))) => Some(*stands),
            _ => None,
        };
        self.carried.ended(laid_stands);
        let outcome = match made {
            Ok((stands, Some(LayOutcome::Laid(made)))) => Outcome::Laid { hour: stands, made },
            Ok((_, None)) => Outcome::TablesOnly,
            Ok((_, Some(LayOutcome::MissingTables(missed)))) | Err(Gave::MissingTables(missed)) => Outcome::MissingTables { missed, notes: loading },
            Ok((_, Some(LayOutcome::Refused(why)))) | Err(Gave::Refused(why)) => {
                self.laid = None;
                loading.push(why);
                Outcome::Refused { reasons: loading }
            }
            Err(Gave::Superseded) => Outcome::Superseded { notes: loading },
        };
        self.finish(&want, outcome);
    }

    fn make(&mut self, want: &Wanted, loading: &mut Vec<String>) -> Result<(Hour, Option<LayOutcome>), Gave> {
        self.declare(want.ask.need(&self.content.counters.defines).shown);
        let (rows, stands, waited_from) = self.rows_at(want, loading)?;
        timeline::mark(self.marks, Mark::Rows);
        note_us("overlay.rows_us", waited_from);
        timeline::mark(self.marks, Mark::BuildBegan);
        let built = Instant::now();
        let tabled = Tabled {
            branch: rows.branch,
            hour: rows.hour,
            rows: Arc::as_ptr(&rows) as usize,
            perspective: want.perspective.clone(),
            vision: Arc::as_ptr(&want.vision) as usize,
            engaged: want.engaged.as_deref().is_some_and(|index| index.covers(rows.branch, rows.hour)),
            left_out: self.left_out,
            labels: want.labels,
            need: want.ask.need(&self.content.counters.defines),
            raids: want.lay_ins.raids.as_ref().map(|icons| Arc::as_ptr(&**icons) as usize),
        };
        let status = if self.tabled.as_ref() == Some(&tabled) {
            TablesBuilt::Ready
        } else {
            self.make_tables(want, &rows, &tabled, loading)
        };
        match status {
            TablesBuilt::Ready if self.tables.is_some() => {}
            TablesBuilt::Missing(missed) => {
                timeline::mark(self.marks, Mark::BuildEnded);
                return Err(Gave::MissingTables(missed));
            }
            TablesBuilt::Ready | TablesBuilt::Refused => {
                timeline::mark(self.marks, Mark::BuildEnded);
                return Err(Gave::Refused(format!("the overlay's tables at hour {} were not made", rows.hour)));
            }
        }
        note_us("overlay.tables_us", built);
        self.publish_tables(want, stands);
        if !want.lay {
            timeline::mark(self.marks, Mark::BuildEnded);
            return Ok((stands, None));
        }
        let made = self.lay(want, &rows, stands);
        timeline::mark(self.marks, Mark::BuildEnded);
        Ok((stands, Some(made)))
    }

    fn rows_at(&mut self, want: &Wanted, loading: &mut Vec<String>) -> Result<(Arc<DecodedFrame>, Hour, Instant), Gave> {
        let started = Instant::now();
        let until = started + WHOLE;
        let (exact, hour) = (want.whole, want.hour);
        loop {
            let seen = self.rows.published();
            let held = self.rows.frame();
            if let Some(rows) = &held {
                let stands = self.stands(rows);
                let reached = if exact { rows.hour == hour } else { stands >= hour };
                if reached && !exact && rows.hour > hour && self.superseded(want) {
                    return Err(Gave::Superseded);
                }
                if reached {
                    return Ok((Arc::clone(rows), stands, started));
                }
            }
            let tabled = self.tabled.as_ref().map(|tabled| tabled.hour);
            match waits(exact, self.superseded(want), held.as_ref().map(|rows| rows.hour), tabled) {
                Waits::Rows => {}
                Waits::Build => {
                    let rows = held.expect("the rows the ask is given up for");
                    let stands = self.stands(&rows);
                    return Ok((rows, stands, started));
                }
                Waits::Give => {
                    if !exact {
                        loading.push(format!("the overlay's rows have not reached hour {hour}"));
                    }
                    return Err(Gave::Superseded);
                }
            }
            if exact {
                if started.elapsed() > Duration::from_secs(1) {
                    lock(&self.shared.held).waiting_note = Some(format!("the overlay's rows have not reached hour {hour}: {}", self.rows_state()));
                }
                if Instant::now() >= until {
                    return Err(Gave::Refused(format!("The overlay's rows did not reach hour {hour}: {}", self.rows_state())));
                }
            }
            self.rows.wait_newer(seen, LOOK.min(until.saturating_duration_since(Instant::now())).max(Duration::from_micros(200)));
        }
    }

    fn rows_state(&self) -> String {
        let held = match self.rows.frame() {
            Some(rows) => format!("the store holds rows of hour {}", rows.hour),
            None => "the store holds none".to_owned(),
        };
        format!("{held}, {} made for it, {} tables read, {} declared", self.rows.published(), self.rows.reads(), self.declared.len())
    }

    fn learn(&mut self, missed: &[TableId]) {
        let before = self.learned.len();
        self.learned.extend(missed.iter().copied());
        self.learned.sort_unstable_by_key(|table| table.index());
        self.learned.dedup();
        if self.learned.len() != before {
            self.declare_now(self.shown);
        }
    }

    fn declare(&mut self, shown: Shown) {
        if self.shown != shown {
            self.declare_now(shown);
        }
    }

    fn declare_now(&mut self, shown: Shown) {
        let mut tables = tables_shown(shown);
        tables.extend(NAVAL_LOOKUPS.iter().chain(AIR_LOOKUPS).copied());
        tables.extend(self.learned.iter().copied());
        tables.sort_unstable_by_key(|table| table.index());
        tables.dedup();
        self.shown = shown;
        if self.declared != tables {
            let held = lock(&self.shared.held);
            self.rows.declare_only(&tables);
            self.declared = tables;
            drop(held);
            self.shared.signal.notify_all();
        }
    }

    fn stands(&self, rows: &DecodedFrame) -> Hour {
        self.rows.through(rows).unwrap_or(rows.hour).max(rows.hour)
    }

    fn superseded(&self, want: &Wanted) -> bool {
        let held = lock(&self.shared.held);
        held.stop || held.want.as_ref().is_some_and(|newer| newer.generation != want.generation || newer.stamp > want.stamp)
    }

    fn make_tables(&mut self, want: &Wanted, rows: &Arc<DecodedFrame>, tabled: &Tabled, loading: &mut Vec<String>) -> TablesBuilt {
        let at = crate::document::Hour::new(&self.content, rows, 0.0).with_air_rosters(Some(Arc::clone(&want.ask.rosters)));
        let raids = want.lay_ins.raids.as_ref().map(|icons| Arc::clone(&**icons)).unwrap_or_default();
        let mut spans = viewer_runtime::profiling::BuilderSpans::new(replay_frame::cpu::enabled());
        let engaged = want.engaged.as_deref();
        let (made, missed) = replay_frame::frame::discovering(|| {
            at.hour_tables_at_head(&want.perspective, Arc::clone(&want.vision), raids, engaged, tabled.left_out, want.left, tabled.labels, tabled.need, &mut spans)
        });
        if !missed.is_empty() {
            self.learn(&missed);
            if want.whole {
                loading.push(format!("The overlay's tables at hour {} read tables its rows do not hold: {missed:?}", rows.hour));
            }
            return TablesBuilt::Missing(missed);
        }
        let built = match made {
            Ok(tables) => {
                self.tables = Some(tables);
                self.tabled = Some(tabled.clone());
                self.tabled_count += 1;
                viewer_runtime::view_record::add("overlay", "tables_built", 1);
                TablesBuilt::Ready
            }
            Err(error) => {
                loading.push(format!("the overlay's tables: {error}"));
                TablesBuilt::Refused
            }
        };
        for part in spans.into_records() {
            viewer_runtime::redraws::note(part.name, (part.ms * 1000.0) as u64);
        }
        built
    }

    fn lay(&mut self, want: &Wanted, rows: &Arc<DecodedFrame>, stands: Hour) -> LayOutcome {
        let started = Instant::now();
        if self.lay_in_keys != want.keys {
            self.lay_ins = Arc::clone(&want.lay_ins);
            self.lay_in_keys = want.keys.clone();
        }
        let lay_ins = Arc::clone(&self.lay_ins);
        let tabled_count = self.tabled_count;
        let Some(tables) = self.tables.as_mut() else {
            return LayOutcome::Refused("the overlay has no tables".to_owned());
        };
        let ask = &want.ask;
        lay_ins.set(tables);
        let left_out = tables.counters().left_out();
        let laying = match &self.laying {
            _ if want.whole => Arc::clone(&ask.view.presentation),
            Some((made_for, laying)) if *made_for == Arc::as_ptr(&ask.view.presentation) as usize => Arc::clone(laying),
            _ => {
                let laying = Arc::new(ask.view.presentation.sharing());
                self.laying = Some((Arc::as_ptr(&ask.view.presentation) as usize, Arc::clone(&laying)));
                laying
            }
        };
        let layered = {
            let mut memory = laying.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            memory.set_layered(want.published.clone(), want.draws, left_out);
            self.left_out = memory.drawing();
            memory.layered().identity()
        };
        let laid = Laid {
            tables: tabled_count,
            view: ask.view.clone(),
            pixels: ask.pixels,
            ui_scale: ask.ui_scale.to_bits(),
            layered,
            lay_ins: want.keys.clone(),
            left: want.left,
            art: self.content.icons.lazy.version(),
        };
        if !want.whole && self.laid.as_ref() == Some(&laid) {
            return LayOutcome::Laid(None);
        }
        let view = CounterView { presentation: Arc::clone(&laying), ..ask.view.clone() };
        let at = crate::document::Hour::new(&self.content, rows, 0.0)
            .with_air_rosters(Some(Arc::clone(&ask.rosters)))
            .with_moved_hours(Arc::clone(&want.moved));
        let read = OverlayRows::at_head(Some(Arc::clone(rows))).leaving(want.left);
        let made = at.prepare_overlay_at(ask.camera, ask.pixels, ask.ui_scale, tables, &view, &read);
        let missed = read.missed();
        if !missed.is_empty() {
            self.learn(&missed);
            self.laid = None;
            viewer_runtime::redraws::note("missed.full", 1);
            return LayOutcome::MissingTables(missed);
        }
        let mut prepared = match made {
            Ok(prepared) => prepared,
            Err(error) => {
                viewer_runtime::redraws::note("refused.full", 1);
                return LayOutcome::Refused(format!("the overlay: {error}"));
            }
        };
        lay_ins.lay(&mut prepared, &at, ask.camera, ask.pixels, ask.ui_scale, tables.ours());
        if want.whole {
            for _ in 0..8 {
                self.content.icons.lazy.settle();
                if !prepared.is_stale(&at) {
                    break;
                }
                match at.prepare_overlay_at(ask.camera, ask.pixels, ask.ui_scale, tables, &view, &read) {
                    Ok(again) => prepared = again,
                    Err(error) => return LayOutcome::Refused(format!("the overlay: {error}")),
                }
                lay_ins.lay(&mut prepared, &at, ask.camera, ask.pixels, ask.ui_scale, tables.ours());
            }
        }
        viewer_runtime::redraws::note("made.full", 1);
        note_us("overlay.layout_us", started);
        self.laid = Some(laid);
        LayOutcome::Laid(Some(Box::new(Made {
            generation: want.generation,
            hour: stands,
            branch: rows.branch,
            prepared,
            tables: Arc::new(tables.clone()),
            rows: Arc::clone(rows),
            placed_at: ask.camera,
            placed_pixels: ask.pixels,
            placed_ui_scale: ask.ui_scale,
            on: laying,
            published: want.published.clone(),
            draws: want.draws,
            view: ask.view.clone(),
            left: want.left,
            lay_ins,
            moved: Arc::clone(&want.moved),
        })))
    }

    fn publish_tables(&mut self, want: &Wanted, stands: Hour) {
        let Some(tables) = &self.tables else { return };
        if want.generation != self.carried.generation {
            return;
        }
        let key = (want.generation, self.tabled_count, stands, want.perspective.clone());
        if self.published_tables.as_ref() == Some(&key) {
            return;
        }
        self.published_tables = Some(key);
        let mut held = lock(&self.shared.held);
        held.built.set_some(Built {
            stamp: want.stamp,
            generation: want.generation,
            hour: stands,
            tables: Arc::new(tables.clone()),
            perspective: want.perspective.clone(),
        });
        drop(held);
        timeline::mark(self.marks, Mark::Published);
        self.shared.signal.notify_all();
        self.rows.notify_progress();
    }

    fn finish(&mut self, want: &Wanted, outcome: Outcome) {
        if want.generation != self.carried.generation {
            return;
        }
        let mut held = lock(&self.shared.held);
        held.waiting_note = None;
        held.answered_stamp = (want.generation, want.stamp);
        let keep_untaken_made = matches!(outcome, Outcome::TablesOnly | Outcome::Superseded { .. } | Outcome::Laid { made: None, .. })
            && held.answer.as_ref().is_some_and(|prev| prev.generation == want.generation && matches!(prev.outcome, Outcome::Laid { made: Some(_), .. }));
        if keep_untaken_made {
            if let Some(prev) = held.answer.take() {
                let mut prev = prev.into_inner();
                prev.stamp = want.stamp;
                if let (Outcome::Laid { hour: prev_hour, .. }, Outcome::Laid { hour, .. }) = (&mut prev.outcome, &outcome) {
                    *prev_hour = (*prev_hour).max(*hour);
                }
                let key = prev.key();
                held.answer.publish(key, prev);
            }
        } else {
            let fresh = matches!(outcome, Outcome::Laid { made: Some(_), .. });
            let answer = Answer { generation: want.generation, stamp: want.stamp, outcome };
            let key = answer.key();
            if fresh {
                held.answer.publish_new(key, answer);
            } else {
                held.answer.publish(key, answer);
            }
        }
        drop(held);
        self.shared.signal.notify_all();
        self.rows.notify_progress();
    }
}

impl Overlaying<Products> for Overlay {
    fn ask_step(&mut self, mut side: OverlayStep<'_>, whole: bool, lay: bool, moved: Arc<MovedHours>) -> bool {
        let keys = LayInKeys::of(side.layers);
        let lay_ins = Arc::new(LayIns::of(side.layers, side.branch));
        let published = counter_kinds::published(side.layers);
        let draws = counter_kinds::draws(side.layers);
        let left = viewer_layers_political::left_to_layers::LeftToLayers::of(Some(side.layers));
        let draws_labels = side.layers.draws("labels");
        let engaged = side.engaged();
        let vision = side.vision(whole).map(Landed::into_inner);
        let (applied, branch) = (side.applied, side.branch);
        let (perspective, views) = (side.perspective, side.views);
        Overlay::ask_step(self, Lend { views, applied, branch, perspective, vision, engaged, draws_labels, lay_ins, keys, published, draws, left, moved, whole, lay })
    }

    fn join(&mut self, layers: &[Box<dyn Layer>], perspective: &Choice, branch: Option<BranchId>, settling: Settling) -> bool {
        let at_hour = Overlay::join(self, settling);
        if let (Some(branch), Some((tables, rosters))) = (branch, self.tables_of(perspective).map(Arc::clone).zip(self.rosters())) {
            counter_kinds::give(layers, perspective, tables.vision(), tables.counters(), branch, move || rosters as viewer_layers_counters::air_mission_counters::Passes);
        }
        at_hour
    }

    fn ask(&mut self, ask: Ask) {
        Overlay::ask(self, ask)
    }

    fn asked(&self) -> bool {
        self.ask.is_some()
    }

    fn stepped(&mut self) {
        Overlay::stepped(self)
    }

    fn stands_at(&self, hour: Hour) -> bool {
        Overlay::stands_at(self, hour)
    }

    fn take(&mut self) -> Option<Landed<Made>> {
        Overlay::take(self)
    }

    fn reset(&mut self) {
        Overlay::reset(self)
    }

    fn generation(&self) -> u64 {
        Overlay::generation(self)
    }

    fn hour(&self) -> Option<Hour> {
        Overlay::hour(self)
    }

    fn loading(&self) -> &[String] {
        Overlay::loading(self)
    }

    fn missed(&self) -> &[TableId] {
        Overlay::missed(self)
    }

    fn refused(&self) -> bool {
        Overlay::refused(self)
    }

    fn needs_reask(&self) -> bool {
        Overlay::needs_reask(self)
    }

    fn offered_stale(&self, layers: LayerParts<'_>, branch: Option<BranchId>) -> bool {
        Overlay::offered_stale(self, layers, branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_views::Presentation;
    use viewer_layer_core::test_support::MakingShownVision;

    fn laid_on(on: &Arc<Presentation>) -> Laid {
        Laid {
            tables: 3,
            view: CounterView { presentation: Arc::clone(on), ..CounterView::default() },
            pixels: [2560, 1440],
            ui_scale: 1.0f32.to_bits(),
            layered: [0; 6],
            lay_ins: Default::default(),
            left: LeftToLayers::default(),
            art: 0,
        }
    }

    #[test]
    fn an_ask_superseded_after_a_seek_back_is_given_up_and_a_stop_is_taken_from_every_wait() {
        assert_eq!(waits(false, false, Some(50), Some(90)), Waits::Rows);
        assert_eq!(waits(false, false, None, None), Waits::Rows);
        assert_eq!(waits(false, true, Some(95), Some(90)), Waits::Build);
        assert_eq!(waits(false, true, Some(50), None), Waits::Build);
        assert_eq!(waits(false, true, Some(50), Some(90)), Waits::Give);
        assert_eq!(waits(false, true, Some(90), Some(90)), Waits::Give);
        assert_eq!(waits(false, true, None, Some(90)), Waits::Give);
        assert_eq!(waits(true, true, Some(95), Some(90)), Waits::Give);
        assert_eq!(waits(true, false, Some(95), Some(90)), Waits::Rows);
    }

    #[test]
    fn a_changed_view_makes_a_new_layout_and_an_unchanged_one_keeps_it() {
        let on = Arc::new(Presentation::default());
        let laid = || laid_on(&on);
        let view = |view: CounterView| CounterView { presentation: Arc::clone(&on), ..view };
        let held = laid();
        assert_eq!(held, laid(), "nothing changed: the frame places the overlay it holds");
        let hovered = view(CounterView { hovered: Some(viewer_runtime::unit_handle::Unit::Division(7)), ..CounterView::default() });
        let selected = view(CounterView { selected: Arc::new([viewer_runtime::unit_handle::Unit::Division(3)].into()), ..CounterView::default() });
        for changed in [
            Laid { view: hovered, ..laid() },
            Laid { view: selected, ..laid() },
            Laid { ui_scale: 1.5f32.to_bits(), ..laid() },
            Laid { pixels: [1920, 1080], ..laid() },
            Laid { tables: 4, ..laid() },
            Laid { layered: [1, 0, 0, 0, 0, 0], ..laid() },
            Laid { art: 1, ..laid() },
            Laid { left: LeftToLayers { resource_icons: true, occupation_marks: false, radar: false }, ..laid() },
        ] {
            assert_ne!(held, changed, "the overlay is laid out again for it");
        }
        let pressed = view(CounterView { pressed: Some(viewer_runtime::unit_handle::Unit::Division(7)), plate_hovered: Some(viewer_runtime::unit_handle::Unit::Division(7)), ..CounterView::default() });
        assert_eq!(held, Laid { view: pressed, ..laid() }, "a plate's state draws the layout again, never lays it out");
        assert_ne!(held, laid_on(&Arc::new(Presentation::default())));
    }

    #[test]
    fn steps_faster_than_a_build_keep_the_newest_overlay_and_never_go_back() {
        const STEPS_PER_BUILD: u32 = 3;
        const HOURS_PER_STEP: Hour = 12;
        let mut asked: Option<(u64, Hour)>;
        let mut building: Option<(u64, Hour, u32)> = None;
        let mut built: Option<(u64, Hour)> = None;
        let (mut taken, mut shown) = (0u64, None::<Hour>);
        let (mut finished, mut installed) = (Vec::new(), Vec::new());
        for step in 1u64..=12 {
            let hour = HOURS_PER_STEP * step as Hour;
            asked = Some((step, hour));
            if let Some((_, _, left)) = &mut building {
                *left -= 1;
                if *left == 0 {
                    let (stamp, at, _) = building.take().expect("held above");
                    assert!(built.is_none(), "nothing is built over an overlay the step has not taken");
                    finished.push(at);
                    built = Some((stamp, at));
                }
            }
            if building.is_none()
                && let Some((stamp, at)) = next_ask(&mut asked, built.is_some())
            {
                assert_eq!(at, hour, "every build is of the newest hour asked");
                building = Some((stamp, at, STEPS_PER_BUILD));
            }
            if let Some((stamp, at)) = built.take() {
                assert!(shows((0, stamp), 0, taken), "the build published is this picture's and newer than the one held");
                assert!(shown.is_none_or(|held| at > held), "the overlay shown only moves forward");
                (taken, shown) = (stamp, Some(at));
                installed.push(at);
            }
        }
        assert_eq!(installed, finished, "every build finished is shown, in order: only the build in flight is between the overlay and the step");
        assert_eq!(shown, Some(HOURS_PER_STEP * 9), "the overlay advanced with every build");
        assert!(!shows((0, taken + 1), 1, taken), "a build in flight across a reset is never shown");
    }

    #[test]
    fn a_layout_installed_is_this_pictures_and_never_stands_before_the_one_held() {
        assert!(installs((3, 100), 3, Some(100)), "the hour it stands at, laid out again");
        assert!(installs((3, 110), 3, Some(100)), "a later hour");
        assert!(!installs((3, 90), 3, Some(100)), "an earlier hour: the overlay never goes back");
        assert!(!installs((2, 200), 3, Some(100)), "laid out for the picture before this one");
        assert!(installs((3, 90), 3, None), "after a seek back, the first layout of the history sought");
    }

    #[test]
    fn nothing_is_laid_out_while_the_layout_published_stands_untaken() {
        assert!(lays(true, false, false), "no layout untaken: the step's ask lays one out");
        assert!(!lays(true, false, true), "one untaken: the memory it is on is not written again");
        assert!(lays(true, true, true), "a whole settle lays out on the view's own memory");
        assert!(!lays(false, true, false), "a caught-up step lays nothing out");
        const CYCLE: u32 = 2;
        let (mut untaken, mut laying, mut left) = (false, None::<Hour>, 0u32);
        let (mut laid, mut taken) = (Vec::new(), Vec::new());
        for frame in 1u64..=12 {
            let hour = 12 * frame as Hour;
            if laying.is_none() && lays(true, false, untaken) {
                (laying, left) = (Some(hour), CYCLE);
                laid.push(hour);
            }
            if let Some(at) = laying {
                left -= 1;
                if left == 0 {
                    (laying, untaken) = (None, true);
                    assert_eq!(laid.last(), Some(&at));
                }
            }
            if untaken {
                untaken = false;
                taken.push(laid[taken.len()]);
            }
        }
        assert_eq!(laid, taken, "every layout the maker lays out reaches the frame, in order");
        assert_eq!(taken.len(), 6, "one layout every two frames: the maker's cycle, and nothing lost to it");
    }

    #[test]
    fn the_overlay_stands_at_the_hour_of_the_layout_the_frame_holds() {
        assert!(stands(Some(100), 0, 100), "the layout the frame holds was laid out for this hour");
        assert!(stands(Some(110), 0, 100), "a layout of a later hour stands at this one");
        assert!(!stands(Some(90), 0, 100), "a layout of an earlier hour: the hour has not arrived");
        assert!(!stands(None, 0, 100), "before the first layout there is no overlay to stand");
        assert!(!stands(Some(100), 1, 100), "a table its tables read is still missing");
        let (mut hour, mut untaken) = (None::<Hour>, None::<Hour>);
        for (head, laid) in [(100, Some(100)), (200, Some(200)), (300, None)] {
            assert!(installs((0, head), 0, hour), "hour {head} is this picture's and no earlier than the one held");
            hour = Some(head);
            untaken = laid.or(untaken.take());
            assert!(stands(hour, 0, head), "hour {head}: the frame is given its picture");
            assert_eq!(untaken.take(), laid, "hour {head}");
        }
    }

    #[test]
    fn a_step_never_waits_for_the_overlay_and_a_whole_settle_waits_for_the_ask_it_posted() {
        for answered in [false, true] {
            assert!(!joins_on(false, answered, false, || true), "a step waits for nothing: answered {answered}");
        }
        assert!(joins_on(true, false, false, || true), "a whole settle waits for the answer to the ask it posted");
        assert!(!joins_on(true, true, false, || true), "answered: the layout this ask made is in");
        assert!(!joins_on(true, false, true, || true), "the wait ran out");
        assert!(!joins_on(true, false, false, || false), "the map must publish the newly declared rows before joining");
    }

    use viewer_runtime::unit_handle::Unit;

    const DIVISION: Unit = Unit::Division(7);
    const OUT: (i32, u32) = (112, 2);
    const BACK: (i32, u32) = (130, 1);

    fn moves(of: &[(Unit, (i32, u32))]) -> Arc<MovedHours> {
        let mut moved = MovedHours::default();
        for &(unit, (hour, province)) in of {
            moved.note(unit, hour, province);
        }
        Arc::new(moved)
    }

    #[derive(Default)]
    struct Bench {
        slot: Option<Request<()>>,
        generation: u64,
        stamp: u64,
        carried: Carried,
        tabled: Option<Hour>,
        layouts: Vec<(Hour, Arc<MovedHours>)>,
    }

    impl Bench {
        fn ask(&mut self, hour: Hour, moved: Arc<MovedHours>, lay: bool) {
            self.stamp += 1;
            let want = Request { generation: self.generation, stamp: self.stamp, hour, moved, whole: false, lay, asking: () };
            post(&mut self.slot, want);
        }

        fn take(&mut self) -> Request<()> {
            let mut want = next_ask(&mut self.slot, false).expect("a request posted");
            self.carried.take(&mut want);
            want
        }

        fn answer(&mut self, want: &Request<()>, rows: Hour, newer: bool) {
            let made = if rows >= want.hour {
                Ok(rows)
            } else {
                match waits(want.whole, newer, Some(rows), self.tabled) {
                    Waits::Build => Ok(rows),
                    Waits::Give => Err(Gave::Superseded),
                    Waits::Rows => panic!("the bench only answers what the maker stops waiting on"),
                }
            }
            .map(|stands| {
                self.tabled = Some(stands);
                (stands, want.lay.then(|| self.layouts.push((stands, Arc::clone(&want.moved)))))
            });
            self.carried.ended(made.ok().and_then(|(stands, laid)| laid.map(|_| stands)));
        }

        fn laid(&self) -> &[(i32, u32)] {
            self.layouts.last().expect("a layout").1.of(DIVISION)
        }
    }

    #[test]
    fn a_replaced_request_carries_its_moves_into_the_one_that_replaces_it() {
        let mut bench = Bench::default();
        bench.ask(118, moves(&[(DIVISION, OUT)]), true);
        bench.ask(136, moves(&[(DIVISION, BACK)]), true);
        let want = bench.take();
        bench.answer(&want, 136, false);
        assert_eq!(bench.laid(), [OUT, BACK], "the out-and-back reaches the layout");
    }

    #[test]
    fn a_request_that_gives_way_while_waiting_for_rows_leaves_its_moves_to_the_next_layout() {
        let mut bench = Bench { tabled: Some(110), ..Bench::default() };
        bench.ask(118, moves(&[(DIVISION, OUT)]), true);
        let claimed = bench.take();
        bench.ask(136, moves(&[(DIVISION, BACK)]), true);
        bench.answer(&claimed, 110, true);
        assert!(bench.layouts.is_empty(), "the claimed request gave way with no layout");
        let want = bench.take();
        bench.answer(&want, 136, false);
        assert_eq!(bench.laid(), [OUT, BACK], "the out-and-back reaches the layout");
    }

    #[test]
    fn a_request_answered_with_tables_alone_leaves_its_moves_to_the_next_layout() {
        let mut bench = Bench::default();
        bench.ask(118, moves(&[(DIVISION, OUT)]), false);
        let tables = bench.take();
        bench.answer(&tables, 118, false);
        assert!(bench.layouts.is_empty(), "tables alone: no layout");
        bench.ask(136, moves(&[(DIVISION, BACK)]), true);
        let want = bench.take();
        bench.answer(&want, 136, false);
        assert_eq!(bench.laid(), [OUT, BACK], "the out-and-back reaches the layout");
    }

    #[test]
    fn a_layout_over_older_rows_keeps_the_later_moves_for_the_next_layout() {
        let earlier = Unit::Division(9);
        let mut bench = Bench { tabled: Some(100), ..Bench::default() };
        bench.ask(148, moves(&[(earlier, (105, 3)), (DIVISION, OUT), (DIVISION, BACK)]), true);
        let want = bench.take();
        bench.ask(160, Arc::default(), true);
        bench.answer(&want, 110, true);
        assert_eq!(bench.layouts.last().map(|laid| laid.0), Some(110), "laid out over the rows at 110");
        let want = bench.take();
        bench.answer(&want, 160, false);
        assert_eq!(bench.laid(), [OUT, BACK], "the moves after 110 reach the next layout, in order");
        assert!(bench.layouts[1].1.of(earlier).is_empty(), "the move at 105 was drawn by the layout over 110");
    }

    #[test]
    fn a_reset_clears_the_moves_carried() {
        let mut bench = Bench::default();
        bench.ask(118, moves(&[(DIVISION, OUT)]), false);
        let tables = bench.take();
        bench.answer(&tables, 118, false);
        bench.ask(130, moves(&[(DIVISION, BACK)]), true);
        bench.generation += 1;
        bench.ask(40, Arc::default(), true);
        let want = bench.take();
        bench.answer(&want, 40, false);
        assert!(bench.laid().is_empty(), "the picture before's moves are not this one's");
    }

    #[test]
    fn no_layout_is_asked_for_while_the_one_asked_for_is_untaken() {
        let mut owed = Owed::default();
        assert!(owed.lays(true, false, false, || unreachable!("nothing owed")), "nothing owed: the step lays out");
        owed.asked(true, false, 1);
        assert!(!owed.lays(true, false, false, || (0, false)), "the maker has not answered it");
        assert!(!owed.lays(true, false, false, || (1, true)), "answered with a layout no step installed");
        assert!(!owed.lays(true, false, true, || (1, false)), "installed and no frame took it");
        assert!(owed.lays(true, true, false, || (0, false)), "a whole settle lays out on the view's own memory");
        assert!(owed.lays(true, false, false, || (1, false)), "answered with no layout pending: let go");
        owed.asked(true, true, 2);
        assert!(owed.lays(true, false, false, || unreachable!("a whole settle owes nothing")), "a whole settle's ask holds nothing");
    }

    #[test]
    fn a_strict_join_fails_unless_its_own_ask_was_answered() {
        assert!(fails(true, 3, 4, false), "strict, an earlier ask answered");
        assert!(!fails(false, 3, 4, false), "a step takes whatever was answered");
        assert!(!fails(true, 4, 4, false), "strict, its own ask answered");
        assert!(fails(false, 4, 4, true), "its own ask answered without what it asked for");
    }

    fn making_vision() -> MakingShownVision {
        MakingShownVision::new(Instant::now)
    }

    fn country(tag: &str) -> Choice {
        Choice { country: viewer_foundation::perspective::Country::Tag(tag.into()), fog: true }
    }

    #[test]
    fn a_vision_made_for_one_perspective_is_never_kept_for_another() {
        let (a, b) = (country("GER"), country("ENG"));
        let mut making = making_vision();
        assert!(making.shown.kept_for(&a).is_err(), "nothing made yet");
        making.ask(&a, 5, 1);
        making.arrive();
        assert_eq!(making.shown.kept_for(&a).map(|vision| vision.ours()), Ok(1), "the copy made for A is A's");
        assert!(making.shown.kept_for(&b).is_err(), "A's copy is no vision of B");
        making.ask(&b, 5, 2);
        assert!(making.shown.kept_for(&b).is_err(), "B's copy is still being made: A's is not shown meanwhile");
        making.arrive();
        assert_eq!(making.shown.kept_for(&b).map(|vision| vision.ours()), Ok(2), "the copy made for B, once it stands");
        assert!(making.shown.kept_for(&a).is_err(), "and B's is no vision of A");
    }

    #[test]
    fn a_forgotten_vision_is_not_kept_and_a_copy_asked_before_the_forget_is_let_go() {
        let a = country("GER");
        let mut making = making_vision();
        making.ask(&a, 90, 1);
        making.arrive();
        assert!(making.shown.kept_for(&a).is_ok(), "the copy of hour 90 stands");
        making.shown.forget();
        assert!(making.shown.kept_for(&a).is_err(), "forgotten: the vision of hour 90 is not kept");
        assert!(making.shown.kept().is_err(), "nor kept for any perspective");
        making.ask(&a, 95, 1);
        making.shown.forget();
        making.arrive();
        assert!(making.shown.kept_for(&a).is_err(), "a copy asked before the forget is let go when it arrives");
        assert!(!making.is_making(), "and nothing is still being made");
        making.ask(&a, 20, 3);
        making.arrive();
        assert_eq!(making.shown.kept_for(&a).map(|vision| vision.ours()), Ok(3), "a copy asked after the forget is kept");
    }

    #[test]
    fn overlay_answer_state_is_atomic_and_unchanged_refusals_do_not_bump_landed() {
        let mut held = Held::default();
        let before = viewer_runtime::landed::count();
        let refusal = Answer { generation: 1, stamp: 1, outcome: Outcome::Refused { reasons: vec!["no supply".into()] } };
        assert!(held.answer.publish(refusal.key(), refusal));
        let after = viewer_runtime::landed::count();
        assert!(after > before, "first refusal moves landed");
        let taken = held.answer.take().unwrap();
        let repeat = Answer { generation: 1, stamp: 2, outcome: Outcome::Refused { reasons: vec!["no supply".into()] } };
        assert!(!held.answer.publish(repeat.key(), repeat), "same refusal after take does not move landed");
        assert_eq!(viewer_runtime::landed::count(), after);
        assert!(matches!(taken.outcome, Outcome::Refused { .. }));

        held.answer.forget_key();
        let before_repeat = viewer_runtime::landed::count();
        let repeat_after_forget = Answer { generation: 1, stamp: 3, outcome: Outcome::Refused { reasons: vec!["no supply".into()] } };
        assert!(held.answer.publish(repeat_after_forget.key(), repeat_after_forget), "forgetting key lets an identical repeat refusal advance landed once");
        assert!(viewer_runtime::landed::count() > before_repeat);
        let _ = held.answer.take();

        let arrived = Answer { generation: 1, stamp: 4, outcome: Outcome::Laid { hour: 12, made: None } };
        assert!(held.answer.publish(arrived.key(), arrived));
        held.answer.forget_key();
        let kept = held.answer.take().expect("forget_key preserves an answer that landed between locks");
        assert_eq!(kept.stamp, 4);

        let refused = OverlayState::Refused { stamp: 4, reasons: vec!["no supply".into()], missed: Vec::new() };
        assert!(skips_refused(false, false, &refused, 4, true), "identical ask at the refused stamp skips");
        assert!(!skips_refused(false, false, &refused, 4, false), "clearing last_asked on vision suspend allows one retry when vision returns");
        assert!(!skips_refused(false, true, &refused, 4, true), "a suppressed layout is not skipped by an earlier refusal");
        assert!(!skips_refused(false, false, &refused, 5, true), "a refusal from an older stamp does not skip a newer stamp");
    }
}
