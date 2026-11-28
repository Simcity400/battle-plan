#[cfg(test)]
pub(crate) use viewer_playback::presentation::MAP_LINES_LOADING;
use crate::map_views::CounterView;
use dashboard_core::protocol::CameraMode;
use job_priority::SpawnCarried as _;
use std::path::Path;
use std::sync::OnceLock;
use viewer_combat as combat;
#[cfg(test)]
use viewer_engagements::combat_selection::Hit as CombatHit;
use viewer_engagements::combat_selection::Selection;
use viewer_gpu::camera::Placement;
use viewer_gpu::camera::Pose;
use viewer_gpu::camera::motion::{Input as MotionInput, Motion, Pan};
use viewer_native_gui::free_icons;
use viewer_gpu::gpu_contract::SOFTWARE_RENDERER_NOTICE;
use viewer_native_gui::native_gui::pointer::Sound;
use viewer_gpu::gpu_contract::device_requirements;
use viewer_gpu::gpu_contract::is_software;
use viewer_foundation::perspective::Choice;
use viewer_runtime::presentation::Continuity;
use viewer_runtime::presentation::Target;
use viewer_gpu::province_pick::{motion_ground_point, pick, terrain_height};
use viewer_gpu::ui_scale::PRESENTATION_FILTER;
use viewer_gpu::ui_scale::Raster;
use viewer_gpu::ui_scale::Setting;
use viewer_gpu::ui_scale::extent as ui_scale_extent;
use viewer_gpu::ui_scale::monitors as ui_scale_monitors;
use viewer_gpu::ui_scale::monitors_generation;
use viewer_gpu::ui_scale::points as ui_scale_points;
use viewer_gpu::ui_scale::refresh_monitors;
use viewer_gpu::ui_scale::to_interface;
use viewer_runtime::profile_writer::profile_print;
use viewer_runtime::profile_writer::profiling;
use viewer_runtime::profiling::hz;
use viewer_runtime::profiling::laps_line;
use viewer_runtime::profiling::sweep as profiling_sweep;
use viewer_runtime::retirement::Dropper;
use viewer_foundation::tooltip_text::Tooltip;
use viewer_runtime::unit_handle::Unit;
use viewer_world::scene::Scene;
use crate::{Config, Options, document::{Document, InitialHour}, render};
use viewer_gpu::{camera::{Camera, MapSize}, frame::{DayNight, GameSpeeds}};
use eframe::{egui, wgpu};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use dashboard_core::map_display::option_defaults;
use viewer_runtime::diagnostics_panel::tooltip_warning_lines;
use viewer_world::scene::SceneInput;
use viewer_counter_kit::CounterReport;
use viewer_runtime::name_draw::Placements;
use viewer_native_gui::sprites::Batch;
use viewer_map_icons::labels::LabelReport;
use viewer_map_text::name_pass::{LayerFade, StrategicNames};
use viewer_world::scene::{Overlay, Request};

mod air_orders;
mod residency;
pub(crate) mod combat_rows;
mod drop;
mod embedded;
pub(crate) mod faction_highlight;
mod ground_click;
mod held_clicks;
mod raid_details;

pub(crate) mod head_rows;
pub(crate) mod highlight;
pub(crate) mod laid_proof;
pub(crate) mod map_hits;
mod hover_report;
mod observe;
mod switch_trace;
pub(crate) mod hover_rows;
pub(crate) mod fly;
mod fly_aircraft;
pub(crate) mod picker;
pub(crate) mod find_view;
mod minimap;
pub(crate) mod playback_cost;
mod presentation;
mod profile_playback;
#[cfg(test)]
pub(crate) mod readback;
mod order_cursor;
mod pointer_changes;
mod selection_input;
mod unit_pick;
mod hud;
mod walk;

#[cfg(test)]
#[test]
fn escape_reaches_only_the_map_owning_its_keys() {
    let (a, b) = (egui::Id::new("map a"), egui::Id::new("map b"));
    assert!(takes_map_keys(true, Some(a), a, false));
    assert!(!takes_map_keys(true, Some(a), b, true));
    assert!(takes_map_keys(true, None, b, true));
    assert!(!takes_map_keys(true, None, a, false));
    assert!(takes_map_keys(false, Some(a), b, false));
    let source = include_str!("app.rs");
    let body = source.split("\n    pub(crate) fn map_ui(").nth(1).unwrap();
    assert!(
        body.contains("let escape = takes_keys && ui.input("),
        "Escape is read for every map"
    );
}

#[cfg(test)]
mod camera_mode_tests;
#[cfg(test)]
mod fly_render_tests;
#[cfg(test)]
mod initial_source_tests;
#[cfg(test)]
mod map_mode_fixture_tests;
#[cfg(test)]
mod map_state_tests;
#[cfg(test)]
mod warning_lines_tests;
#[cfg(test)]
mod render_failure_tests;
#[cfg(test)]
mod spine_tests;

const MAP_INPUT_OWNER: &str = "map camera input owner";

const SETTLES: u32 = 8;

const HELD_GIVEN_UP: Duration = Duration::from_secs(300);

fn takes_map_keys(
    dashboard_map: bool,
    owner: Option<egui::Id>,
    me: egui::Id,
    hovered: bool,
) -> bool {
    !dashboard_map || owner.map_or(hovered, |owner| owner == me)
}

#[derive(Debug, Default)]
struct Presented {
    texture: Option<egui::TextureId>,
    generation: u64,
}

impl Presented {
    fn stale(&self, generation: u64) -> bool {
        self.texture.is_none() || self.generation != generation
    }

    fn kept(&self, generation: Option<u64>) -> Option<egui::TextureId> {
        generation.filter(|&generation| !self.stale(generation)).and(self.texture)
    }

    fn take(&mut self) -> Option<egui::TextureId> {
        self.generation = 0;
        self.texture.take()
    }
}

#[cfg(test)]
#[test]
fn a_map_retired_and_shown_again_registers_its_new_target() {
    let mut presented = Presented::default();
    assert!(presented.stale(1));
    assert_eq!(presented.kept(Some(1)), None, "nothing drawn yet: nothing to keep");
    presented.texture = Some(egui::TextureId::User(7));
    presented.generation = 1;
    assert!(!presented.stale(1));
    assert_eq!(presented.kept(Some(1)), Some(egui::TextureId::User(7)), "the picture last drawn stays on screen");
    assert_eq!(presented.kept(Some(2)), None, "a new target holds no picture yet");
    assert_eq!(presented.kept(None), None, "nor does a map without a scene");
    assert!(presented.stale(2));
    assert_eq!(presented.take(), Some(egui::TextureId::User(7)));
    assert!(presented.stale(1));
}

fn profile_hover_point(
    real: Option<[f32; 2]>,
    synthetic: Option<[f32; 2]>,
    enabled: bool,
    size: [u32; 2],
    ui_scale: f32,
) -> Option<[f32; 2]> {
    let point = match synthetic.filter(|_| enabled) {
        Some(point) => (point
            .into_iter()
            .zip(size)
            .all(|(p, size)| p.is_finite() && p >= 0.0 && p < size as f32))
        .then_some(point),
        None => real,
    };
    point.map(|point| to_interface(point, ui_scale))
}

struct Laps {
    enabled: bool,
    frame_cpu: Option<viewer_runtime::profiling::ThreadSpan>,
    start: Instant,
    last: Instant,
    outside: f64,
    laps: Vec<(&'static str, f64)>,
}

static FRAME_END: Mutex<Option<Instant>> = Mutex::new(None);

fn mark_frame_end() {
    if let Ok(mut end) = FRAME_END.lock() {
        *end = Some(Instant::now());
    }
}

impl Laps {
    fn new(waited: Duration) -> Self {
        let enabled = profiling();
        viewer_runtime::profiling::begin_thread_frame();
        let frame_cpu = enabled.then(|| viewer_runtime::profiling::thread_span("frame"));
        let now = Instant::now();
        let outside = FRAME_END
            .lock()
            .ok()
            .and_then(|end| *end)
            .map_or(0.0, |end| {
                (now.saturating_duration_since(end).saturating_sub(waited)).as_secs_f64() * 1000.0
            });
        Self {
            enabled,
            frame_cpu,
            start: now,
            last: now,
            outside,
            laps: Vec::new(),
        }
    }

    fn lap(&mut self, name: &'static str) {
        if self.enabled {
            let now = Instant::now();
            self.laps
                .push((name, now.duration_since(self.last).as_secs_f64() * 1000.0));
            self.last = now;
        }
    }

    fn total(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn line(&self) -> String {
        laps_line(&self.laps, 0.05)
    }
}

fn gpu_wait(device: &wgpu::Device) -> f64 {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !ENABLED.get_or_init(|| std::env::var_os("BATTLEPLAN_VIEWER_GPU_WAIT").or_else(|| std::env::var_os("HOI4_VIEWER_GPU_WAIT")).is_some()) {
        return 0.0;
    }
    let start = Instant::now();
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    start.elapsed().as_secs_f64() * 1000.0
}

fn pace_profiled_frame() -> Duration {
    let period = Duration::from_secs_f64(1.0 / hz());
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let started = Instant::now();
    let Ok(mut last) = LAST.lock() else {
        return Duration::ZERO;
    };
    if let Some(previous) = *last {
        let due = previous + period;
        while let Some(left) = due.checked_duration_since(Instant::now()) {
            if left > Duration::from_millis(2) {
                std::thread::sleep(left - Duration::from_millis(2));
            } else {
                std::hint::spin_loop();
            }
        }
    }
    *last = Some(Instant::now());
    started.elapsed()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    pub camera: Camera,
    pub viewport: [u32; 2],
    pub ui_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaintingPlaced {
    Placed,
    Refused(viewer_native_gui::anchored::Refused),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OverlayPlaced {
    Unchanged,
    Placed,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Playhead {
    branch: replay_data::BranchId,
    hour: i32,
    fraction: f64,
}

#[derive(Clone, Copy, Debug)]
struct HeldClock {
    playhead: Option<Playhead>,
    playing: bool,
    anchor: Option<(Playhead, bool, f64)>,
    at: Instant,
}

#[derive(Debug, Default)]
struct MapPresented {
    last: Option<(replay_data::BranchId, i32, Option<String>)>,
    seek: Option<u64>,
    reopened: bool,
}

fn overlay_matches_continuity(
    overlay: (u64, replay_data::BranchId, i32, u32),
    presented: (replay_data::BranchId, i32, Option<&str>),
    current_generation: Option<u64>,
    gaps: &[replay_frame::branches::Gap],
) -> bool {
    let (ov_gen, ov_branch, ov_hour, ov_ours) = overlay;
    let (branch, hour, viewing) = presented;
    current_generation.is_none_or(|generation| ov_gen == generation)
        && ov_branch == branch
        && viewing.is_none_or(|viewing| viewing == ov_ours.to_string())
        && !viewer_runtime::presentation::crosses_gap(
            gaps,
            branch,
            ov_hour.min(hour),
            ov_hour.max(hour),
        )
}

fn overlay_rect(clip: egui::Rect, points: f32) -> egui::Rect {
    let inset = 8.0 * points;
    egui::Rect::from_min_size(
        clip.min + egui::vec2(inset, inset),
        egui::vec2(
            (clip.width() - 2.0 * inset).max(1.0),
            (clip.height() * 0.4).max(1.0),
        ),
    )
}

fn scale_style(style: &mut egui::Style, points: f32) {
    for font in style.text_styles.values_mut() {
        font.size *= points;
    }
    let spacing = &mut style.spacing;
    spacing.item_spacing *= points;
    spacing.button_padding *= points;
    spacing.interact_size *= points;
    spacing.icon_width *= points;
    spacing.icon_width_inner *= points;
    spacing.icon_spacing *= points;
    spacing.indent *= points;
    let scroll = &mut spacing.scroll;
    scroll.bar_width *= points;
    scroll.bar_inner_margin *= points;
    scroll.bar_outer_margin *= points;
    scroll.floating_width *= points;
    scroll.floating_allocated_width *= points;
    scroll.handle_min_length *= points;
}

pub(crate) fn map_overlay(
    ui: &mut egui::Ui,
    clip: egui::Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    points: f32,
    add: impl FnOnce(&mut egui::Ui, f32),
) {
    let rect = overlay_rect(clip, points);
    let mut overlay = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    overlay.set_clip_rect(clip);
    scale_style(overlay.style_mut(), points);
    egui::Frame::new()
        .fill(egui::Color32::from_black_alpha(220))
        .inner_margin(egui::Margin::same(6) * points)
        .show(&mut overlay, |ui| {
            add(ui, (rect.height() - 50.0 * points).max(24.0 * points))
        });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CameraKeyFilter {
    None,
    Walk,
    FlyFree,
    FlyAircraft,
}

impl CameraKeyFilter {
    pub(super) fn blocks(self, key: egui::Key) -> bool {
        match self {
            Self::None => false,
            Self::Walk => matches!(
                key,
                egui::Key::W
                    | egui::Key::A
                    | egui::Key::S
                    | egui::Key::D
                    | egui::Key::Space
                    | egui::Key::Equals
                    | egui::Key::Plus
                    | egui::Key::Minus
                    | egui::Key::Num0
                    | egui::Key::V
                    | egui::Key::Escape
            ),
            Self::FlyFree => matches!(
                key,
                egui::Key::W
                    | egui::Key::A
                    | egui::Key::S
                    | egui::Key::D
                    | egui::Key::Space
                    | egui::Key::Equals
                    | egui::Key::Plus
                    | egui::Key::Minus
                    | egui::Key::Num0
                    | egui::Key::Escape
            ),
            Self::FlyAircraft => matches!(
                key,
                egui::Key::W
                    | egui::Key::A
                    | egui::Key::S
                    | egui::Key::D
                    | egui::Key::V
                    | egui::Key::C
                    | egui::Key::ArrowLeft
                    | egui::Key::ArrowRight
                    | egui::Key::ArrowUp
                    | egui::Key::ArrowDown
                    | egui::Key::Escape
            ),
        }
    }
}

pub(crate) fn capture_mode_notice(ui: &mut egui::Ui, replay: &replay::campaign::Replay) {
    let mode = replay.capture_mode;
    if let Some(omits) = replay_data::scope::omits(mode) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                egui::Color32::YELLOW,
                format!("{} recording:", mode_name(mode)),
            );
            ui.colored_label(egui::Color32::YELLOW, format!("not recorded: {omits}."));
        });
    }
}

fn mode_name(mode: replay_data::scope::CaptureMode) -> &'static str {
    match mode {
        replay_data::scope::CaptureMode::Full => "Full",
        replay_data::scope::CaptureMode::Map => "Map",
    }
}

pub(crate) fn partial_notice(ui: &mut egui::Ui, replay: &replay::campaign::Replay) {
    if let Some(replay_recording::control::ControlRecord::Partial { reason, .. }) = replay.partial() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(egui::Color32::YELLOW, "Partial recording:");
            ui.colored_label(egui::Color32::YELLOW, reason);
        });
    }
}

#[derive(Clone, Default)]
struct Uploaded {
    order_key: Option<OrderKey>,
    air_field: Option<((usize, Vec<u64>), Arc<viewer_gpu::ribbon_content::OrderField>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OrderKey {
    pub plan: usize,
    pub raids: usize,
    pub routes: Option<u64>,
    pub air: Option<(usize, Vec<u64>)>,
}

pub(crate) fn order_at<T>(held: Option<&Arc<T>>) -> usize {
    held.map_or(0, |held| Arc::as_ptr(held) as usize)
}

struct LayeredOrders {
    plan: Option<Arc<viewer_gpu::ribbon_content::OrderField>>,
    raids: Option<Arc<viewer_gpu::ribbon_content::OrderField>>,
    routes: Option<u64>,
}

const DIAGNOSTICS_KEY: egui::Key = egui::Key::F3;

const CAMERA_MODE_KEY: egui::Key = egui::Key::F12;

fn is_diagnostics_shortcut(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if *key == DIAGNOSTICS_KEY && modifiers.matches_exact(egui::Modifiers::CTRL)
    )
}

struct Capture {
    path: PathBuf,
    remaining: u32,
    pinned: Option<([u32; 2], Option<Placement>)>,
    exit: bool,
    time: Option<f32>,
    step: f32,
    frames: u32,
    index: u32,
    paced: bool,
    export: Option<crate::host::ExportJob>,
    video: Option<Box<crate::video_export::VideoSession>>,
}

pub(crate) struct VisualStale {
    campaign: Option<String>,
    maps: Vec<(crate::dashboard_maps::MapSourceKey, String)>,
    checked: usize,
    failures: Vec<String>,
}

struct LoadedReplay {
    document: Document,
    revision: Option<String>,
    source: Option<Arc<dashboard_base::Source>>,
    config: Config,
}

fn seek_open_target(
    seek: &crate::embedded::Seek,
    accepted: Option<(&Path, Option<&str>)>,
    pending_load: Option<(&Path, Option<&str>)>,
) -> Option<crate::embedded::OpenRequest> {
    let matches = pending_load
        .or(accepted)
        .is_some_and(|(path, revision)| seek.matches(path, revision));
    (!matches).then(|| seek.open_request())
}

type MatchGame = ((u64, usize), (Raster, Option<String>));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapHit {
    AirMission(usize),
    NavalMission(usize),
    NavalRegionHover(usize),
    Raid,
    Intel,
    Occupation(usize),
    Adjacency(usize),
    Combat(usize),
    NavalObserver(usize),
    PortTooltip(usize),
    Unit(usize),
    Base(usize),
    Pin(usize),
    Blocked,
}

fn prepared_naval_tooltip(
    report: &CounterReport,
    target: Option<MapHit>,
) -> Option<(Arc<Tooltip>, [f32; 4])> {
    let (text, rect) = match target? {
        MapHit::NavalMission(index) => {
            let hit = report.naval_mission_hits.get(index)?;
            (hit.text.as_ref()?, hit.rect)
        }
        MapHit::NavalRegionHover(index) => {
            let hit = report.naval_region_hovers.get(index)?;
            (hit.text.as_ref()?, hit.rect)
        }
        _ => return None,
    };
    Some((Arc::clone(text), rect))
}

fn radar_province(
    target: Option<MapHit>,
    report: &viewer_counter_kit::CounterReport,
) -> Option<u32> {
    let MapHit::Base(index) = target? else {
        return None;
    };
    report.radar_hits.get(&index).copied()
}

fn land_hover_matches_unit(
    hover: &viewer_native_gui::counter_hovers::land::CounterHover,
    unit: &viewer_native_gui::counter_hovers::unit_hits::Hit,
) -> bool {
    let kind = if hover.railway_gun {
        viewer_native_gui::counter_hovers::unit_hits::Kind::RailwayGun
    } else {
        viewer_native_gui::counter_hovers::unit_hits::Kind::Division
    };
    unit.kind == kind && unit.members == hover.members
}

#[cfg(test)]
fn top_map_hit(
    point: [f32; 2],
    report: &viewer_counter_kit::CounterReport,
    raids: &[viewer_raids::map::raids::tooltips::Hit],
    occupation: &[viewer_map_icons::occupation_icons::Hit],
    looks: Option<&free_icons::Looks>,
    intel: Option<&viewer_raids::map::intel_icons::IntelHit>,
) -> Option<MapHit> {
    let shapes = map_hits::Shapes { report, raids, occupation, adjacency: &[], looks, shifts: None };
    map_hits::Index::of(&shapes).top(point, &shapes, intel)
}

fn unit_family(
    report: &CounterReport,
    index: usize,
    hit: &viewer_native_gui::counter_hovers::unit_hits::Hit,
) -> viewer_gpu::icon_tracks::IconFamily {
    if report.regional_naval_unit_hits.contains(&index) {
        viewer_gpu::icon_tracks::IconFamily::Missions
    } else if hit.port {
        viewer_gpu::icon_tracks::IconFamily::Bases
    } else {
        viewer_gpu::icon_tracks::IconFamily::Columns
    }
}

pub struct Viewer {
    dashboard: Option<Box<crate::dashboard_maps::DashboardMaps>>,
    dashboard_active: bool,
    popout_open: bool,
    dashboard_map: bool,
    host_graphics: bool,
    shared_pool: Option<Arc<crate::worker_pool::WorkerPool>>,
    source: Option<Arc<dashboard_base::Source>>,
    loaded_config: Option<Config>,
    rendered: Option<Playhead>,
    rendered_sequence: u64,
    drawn_at: Option<f64>,
    drawn_frame: u64,
    applying_options: Option<dashboard_core::protocol::MapOptions>,
    drawn_options: Option<dashboard_core::protocol::MapOptions>,
    hover: Option<String>,
    tooltip: Option<String>,
    tooltip_delay_ms: Option<f32>,
    perf_view: Option<String>,
    map_warnings: Vec<String>,
    pub(crate) diagnostics: bool,
    diagnostics_open: bool,
    audio: Option<viewer_audio::Owner>,
    pub(crate) audio_focus: bool,
    pub(crate) campaign_tab: bool,
    pub(crate) muted: bool,
    pub(crate) audio_reset: bool,
    pub(crate) music: Option<Arc<viewer_audio::Chances>>,
    pub(crate) battles: Option<Arc<viewer_audio::Battles>>,
    pub(crate) interacted: bool,
    pub(crate) interface_points: f32,
    embedded: Option<crate::embedded::Controller>,
    harness_service: Option<Arc<dashboard::Service>>,
    embedded_open: Option<crate::embedded::OpenRequest>,
    embedded_seek: Option<crate::embedded::Seek>,
    revision: Option<String>,
    expected_revision: Option<String>,
    expected_open_id: Option<u64>,
    installed_open_id: Option<u64>,
    hidden_since: Option<Instant>,
    combat: combat::Window,
    config: Config,
    options: Options,
    awaiting_folders: bool,
    document: Option<Document>,
    loading: Option<mpsc::Receiver<Result<LoadedReplay, String>>>,
    startup_content: Option<content_reader_layers::content::Loading>,
    error: Option<String>,
    scene: Option<Scene>,
    window_layout: Option<viewer_native_gui::native_windows::NativeWindows>,
    scene_build: Option<residency::SceneBuild>,
    scene_error: Option<String>,
    scene_target: Presented,
    map_error: Option<String>,
    camera: Option<Placement>,
    camera_manual: bool,
    settled_poses: bool,
    settled_capture: bool,
    held_view: Option<(PathBuf, HeldClock)>,
    held_clock: Option<HeldClock>,
    zoom: Motion,
    camera_mode: CameraMode,
    free_overlays: bool,
    start_pick: drop::Pick,
    free_overlay: Batch,
    overlay_written: u64,
    free_presented: Option<(viewer_gpu::camera::Camera, [u32; 2], u32, u64, viewer_gpu::icon_tracks::Head)>,
    looks: Option<free_icons::Looks>,
    input: crate::controls::Input,
    orbit: crate::controls::orbit::Orbit,
    walk: walk::Walk,
    flight: Option<crate::controls::flight::Flight>,
    free_cam: Option<crate::controls::walker::Walker>,
    free_cam_speed: Option<f32>,
    fly_view: crate::controls::flight::View,
    last_fly_eye: Option<([f32; 3], f32, f32)>,
    mode_handover: Option<([f32; 3], f32, f32)>,
    hud: hud::Hud,
    flying: Option<(usize, viewer_models::ModelInstance)>,
    fly_clock: Option<f32>,
    fly_footprints: crate::controls::flight::Footprints,
    pointer_grabbed: bool,
    playing: bool,
    speed: f64,
    clock_anchor: Option<(Playhead, bool, f64)>,
    page_clock: crate::page_clock::PageClock,
    day_night: viewer_gpu::frame::DayNightPlayback,
    last_step: Instant,
    profile_playback: profile_playback::Clock,
    started: Instant,
    ambient_clock: AmbientClock,
    opened: Option<Instant>,
    postfx: bool,
    pub(crate) graphics: dashboard_core::protocol::MapGraphics,
    pub(crate) counter_filter: dashboard_core::protocol::CounterFilter,
    pub(crate) player_counters: bool,
    pub(crate) volumes: dashboard_core::protocol::MapAudio,
    overlay_layers: viewer_layer_core::scene_content::OverlayLayers,
    counter_owners: crate::map_views::CounterOwners,
    resolved_settings: crate::map_views::ResolvedSettings,
    pub(crate) ui_scale: Setting,
    fly_aircraft: fly_aircraft::FlyAircraft,
    match_game: Option<MatchGame>,
    monitor_seen: Option<(Option<egui::Vec2>, Option<f32>)>,
    visual_check: Option<std::sync::mpsc::Receiver<VisualStale>>,
    visual_check_note: Option<String>,
    visual_check_after_load: embedded::DeferredCheck,
    stale_visual_revision: Option<String>,
    pub(crate) visual_fallbacks: crate::visual_fallback::Shared,
    visual_report: Option<crate::visual_fallback::Report>,
    drew_frame: bool,
    pub(crate) warming: bool,
    visual_stale_campaign: Option<String>,
    map_preferences_seen: Option<(
        String,
        Option<Vec<content_reader_layers::content::visual::Choice>>,
        u32,
        u8,
    )>,
    playhead: Option<Playhead>,
    frame_head: viewer_native_gui::frame_head::FrameHead,
    animation_seek: bool,
    map_animation_target: Option<Target>,
    playhead_map: Option<viewer_playback::side::SideThread>,
    playhead_refused: Option<String>,
    playhead_opened: bool,
    seek_owed: bool,
    overlay_hour: Option<replay_data::Hour>,

    shown_whole: bool,
    switch_trace: switch_trace::SwitchTrace,
    observing: observe::Observing,
    playhead_opening: Option<crate::playhead_host::Opening>,
    presented: MapPresented,
    last_view: Option<View>,
    uploaded: Uploaded,
    dropper: Option<Arc<Dropper>>,
    worker_pool: Option<Arc<crate::worker_pool::WorkerPool>>,
    raid_tick: Option<f32>,
    convoys_sailing: bool,
    convoy_refused: BTreeMap<String, usize>,
    placements: Option<Arc<Placements>>,
    fade: Option<LayerFade>,
    hovered_name_region: Option<u32>,
    last_frame: Instant,
    camera_step: Option<f32>,
    label_report: LabelReport,
    faction_highlight: faction_highlight::Lane<faction_highlight::Prepared>,
    hover_rows: hover_rows::HoverRows,
    head_rows: head_rows::HeadRows,
    highlight_wanted: bool,
    overlay_frame: Option<Arc<replay_frame::frame::DecodedFrame>>,
    overlay_moved: Arc<viewer_unit_reads::counter_move::MovedHours>,
    overlay_tables: Option<Arc<crate::document::OverlayTables>>,
    overlay_routes_pending: bool,
    overlay_left: viewer_layers_political::left_to_layers::LeftToLayers,
    overlay_lay_ins: Option<std::sync::Arc<viewer_playback::side::LayIns>>,
    overlay_layered: Option<(viewer_counters::counters::layered::Layered, viewer_counter_kit::Kinds)>,
    overlay_shown: Option<(replay_data::BranchId, replay_data::Hour)>,
    overlay_view: Option<CounterView>,
    overlay_placed: Option<Camera>,
    overlay_asked: Option<viewer_counters::counters::Shown>,
    overlay_placed_view: Option<([u32; 2], u32)>,
    overlay_shifts: viewer_native_gui::anchored::Shifts,
    overlay_camera_before: Option<Camera>,
    overlay_head_before: Option<viewer_gpu::icon_tracks::Head>,
    overlay_counted: viewer_native_gui::overlay_count::Watch,
    overlay_unpainted: bool,
    overlay_generation: Option<u64>,
    overlay_clock_unavailable: bool,
    overlay_drawn_fraction: Option<f32>,
    overlay_lookups: Option<(usize, u64, u64)>,
    overlay_installed: u64,
    overlay_checked: u64,
    overlay_looks: u64,
    overlay_loading: Vec<String>,
    held_clicks: std::collections::VecDeque<held_clicks::Held>,
    overlay_missed: Vec<replay_data::rows::TableId>,
    combat_rows: combat_rows::CombatRows,
    tooltip_refused: BTreeMap<String, usize>,
    counter_report: CounterReport,
    combat_painted: combat::Footprint,
    overlay_hits: crate::document::OverlayHits,
    map_hits: std::cell::RefCell<map_hits::Resolved>,
    hits_stamp: u64,
    map_tooltip_target: Option<highlight::TooltipTarget>,
    shown_tooltip: Option<highlight::TooltipTarget>,
    interface_sounds: Vec<InterfaceSound>,
    interface_key: u64,
    counter_held: Option<(Unit, bool)>,
    pressed_on: Option<(Option<Holder>, egui::Pos2, f64, bool)>,
    counter_selecting: Option<Unit>,
    secondary_on: Option<viewer_combat::picker::Control>,
    air_orders: air_orders::AirOrders,
    selection_input: selection_input::SelectionInput,
    overlay: Option<Batch>,
    prepared_overlay: Option<crate::document::PreparedOverlay>,
    overlay_fraction: Option<f32>,
    overlay_art_moved: bool,
    overlay_art_retries: u32,
    held_since: Option<Instant>,
    combat_inputs: Option<usize>,
    overlay_pools: viewer_native_gui::icon_windows::Pools,

    counter_view: CounterView,
    unit_selection: viewer_counters::unit_selection::Selection,
    air_selection: viewer_counters::air_selection::Selection,
    fleet_selection: viewer_counters::naval_selection::Selection,
    perspective: Choice,
    map_mode: viewer_map_modes::map_mode::MapMode,
    diplomatic_selected: Option<u32>,
    ground_selection: Option<ground_click::Outline>,
    picker: picker::PickerState,
    picker_error: Option<String>,
    picker_refused: std::collections::BTreeMap<String, usize>,
    picker_controls: Option<([f32; 2], Vec<viewer_combat::picker::Placed>)>,
    capture: Option<Capture>,
    capture_note: Option<String>,
    private_playhead: bool,
    video_crop: Option<(Option<[u32; 2]>, crate::video_export::CropRect)>,
    video_preload: Option<(
        Arc<replay::campaign::Replay>,
        String,
        Option<dashboard_core::protocol::ResolvedMapSettings>,
    )>,
    failure: Arc<Mutex<Option<String>>>,
    gpu: Option<(wgpu::Device, wgpu::Queue)>,
    profile_frame: u32,
    laps: Option<Laps>,
}

pub(crate) fn load_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    document: &crate::document::Content,
    graphics: dashboard_core::protocol::MapGraphics,
    workers: &viewer_runtime::parallel::Workers,
) -> std::io::Result<Scene> {
    let _startup = crate::perf_log::StartupSpan::new("scene");
    let scene = engine_executable::executable_tables::scope(document.tables, || {
        load_scene_scoped(device, queue, document, graphics, workers)
    });
    if scene.is_err() {
        document.release_loading();
    }
    scene
}

fn load_scene_scoped(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    document: &crate::document::Content,
    graphics: dashboard_core::protocol::MapGraphics,
    workers: &viewer_runtime::parallel::Workers,
) -> std::io::Result<Scene> {
    use viewer_runtime::load_record::stage;
    let textures = {
        let _stage = stage("scene.textures");
        document
            .scene_textures
            .warm(device, queue, &document.assets)?
    };
    let mut scene = {
        let _stage = stage("scene.passes");
        Scene::with_textures(
            device,
            queue,
            textures,
            &document.assets,
            &document.icons.lazy,
            graphics,
            workers,
        )?
    };
    let mut apply = |name: &'static str, input: SceneInput<'_>| {
        let _stage = stage(name);
        scene.apply(device, queue, input)
    };
    apply(
        "scene.animated_text",
        SceneInput::InstallAnimatedText {
            assets: &document.icons.animated_text,
            gui: &document.icons.counters.gui,
        },
    )?;
    apply(
        "scene.borders",
        SceneInput::SetBorders {
            assets: &document.border_assets,
            meshes: &document.border_meshes,
            straits: &document.straits,
        },
    )?;
    apply(
        "scene.rivers",
        SceneInput::SetRivers {
            assets: &document.river_assets,
            mesh: &document.river_mesh,
        },
    )?;
    apply(
        "scene.trees",
        SceneInput::SetTrees {
            assets: &document.tree_assets,
            geometry: document.tree_geometry.clone(),
            field: &document.tree_field,
        },
    )?;
    apply("scene.sprawl", SceneInput::SetSprawl(&document.sprawl))?;
    apply("scene.map_frame", SceneInput::SetMapFrame(&document.frame_assets))?;
    apply("scene.trains", SceneInput::WarmTrains(&document.trains))?;
    apply("scene.aircraft", SceneInput::WarmAircraft(&document.aircraft))?;
    apply("scene.unit_models", SceneInput::WarmUnitModels(&document.unit_models))?;
    apply("scene.map_buildings", SceneInput::WarmMapBuildings(&document.map_buildings))?;
    apply("scene.bridges", SceneInput::WarmBridges(&document.bridges))?;
    apply("scene.railways", SceneInput::WarmRailways(&document.rail_assets))?;
    apply("scene.orders", SceneInput::WarmOrders(&document.orders))?;
    apply("scene.weather", SceneInput::WarmWeather(document.weather_effects.clone()))?;
    apply(
        "scene.political",
        SceneInput::WarmPolitical {
            map: &document.map,
            strategic_regions: &document.strategic_regions,
            field: &document.map_defines.gradient_field,
            cache: &document.political_gpu,
        },
    )?;
    let warm_started = Instant::now();
    apply("scene.pipelines", SceneInput::WarmPipelines)?;
    crate::perf_log::since("pipeline_warmup", warm_started);
    let warm_started = Instant::now();
    let icons = &document.icons;
    {
        let _stage = stage("scene.gui");
        scene.warm_gui(
            device,
            icons
                .counters
                .gui
                .effects()
                .into_iter()
                .chain(icons.combat.gui.effects()),
        );
    }
    crate::perf_log::since("gui_warmup", warm_started);
    Ok(scene)
}

impl Viewer {
    fn check_overlay(
        &mut self,
        what: &str,
        now: &crate::document::Hour<'_>,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        time: f32,
    ) -> Result<(), String> {
        let (Some(tables), true) = (self.overlay_tables.clone(), self.prepared_overlay.is_some()) else {
            overlay_checks::note(&overlay_checks::NO_OVERLAY);
            return Ok(());
        };
        if self.prepared_overlay.as_ref().is_some_and(|prepared| prepared.is_stale(now)) {
            overlay_checks::note(&overlay_checks::ART_MOVED);
            profile_print(format!("CHECK overlay {what} not compared: the art moved since it was laid out"));
            return Ok(());
        }
        let side = live_of(&self.playhead_map);
        let left = self.overlay_left;
        let rows = crate::document::OverlayRows::at_head(self.overlay_frame.clone().filter(|_| side.is_some())).leaving(left);
        let apart = || {
            let presentation = self.counter_view.presentation.snapshot();
            self.counter_view.presentation.lend_layered(&presentation);
            CounterView { presentation: Arc::new(presentation), ..self.counter_view.clone() }
        };
        let (for_shown, for_fresh) = (apart(), apart());
        let layers = self.overlay_layers.clone();
        let held = self.counter_view.clone();
        let full = now.prepare_overlay_at(camera, pixels, ui_scale, &tables, &for_fresh, &rows);
        if !rows.missed().is_empty() {
            overlay_checks::note(&overlay_checks::ROWS_MISSED);
            profile_print(format!(
                "CHECK overlay {what} not compared: the rows the overlay was made from hold none of {:?}",
                rows.missed()
            ));
            return Ok(());
        }
        let mut full = full.map_err(|error| format!("the overlay laid out afresh for the gate: {error}"))?;
        if let Some(moved) = laid_proof::layers_moved(
            self.prepared_overlay.as_ref().expect("held above").layers(),
            full.layers(),
        ) {
            overlay_checks::note(&overlay_checks::LAYERS_MOVED);
            profile_print(format!("CHECK overlay {what} not compared: the layers' product moved since it was laid out ({moved})"));
            return Ok(());
        }
        if let Some(lay_ins) = self.overlay_lay_ins.clone() {
            lay_ins.lay(&mut full, now, camera, pixels, ui_scale, tables.ours());
        }
        let draw = |prepared: &mut crate::document::PreparedOverlay| {
            let (mut batch, mut report) = (Batch::default(), CounterReport::default());
            prepared
                .draw_into(now, camera, pixels, ui_scale, &layers, time, &mut batch, &mut report, &mut Default::default())
                .map(|()| (batch, report, prepared.label_report().clone(), prepared.raid_report().clone()))
        };
        let settle = |prepared: &mut crate::document::PreparedOverlay| {
            let mut rounds = viewer_native_gui::icon_windows::DumpFrames::default();
            let mut drawn = draw(prepared);
            while drawn.is_ok() && rounds.next(&prepared.retries()) {
                drawn = draw(prepared);
            }
            drawn
        };
        let shown = {
            let prepared = self.prepared_overlay.as_mut().expect("held above");
            prepared.set_counter_view(for_shown);
            let drawn = settle(prepared);
            prepared.set_counter_view(held);
            drawn
        };
        let fresh = settle(&mut full);
        if self.prepared_overlay.as_ref().is_some_and(|prepared| prepared.is_stale(now)) {
            overlay_checks::note(&overlay_checks::ART_MOVED);
            profile_print(format!("CHECK overlay {what} not compared: the art moved while it was compared"));
            return Ok(());
        }
        overlay_checks::note(&overlay_checks::COMPARED);
        let mut found = Vec::new();
        match (shown, fresh) {
            (Ok(shown), Ok(fresh)) => {
                if let Some(difference) = laid_proof::first_difference(&laid_proof::clockless(&shown.0), &laid_proof::clockless(&fresh.0)) {
                    found.push(difference);
                }
                if shown.1 != fresh.1 {
                    found.push(laid_proof::first_report_difference("counters' reports", &shown.1, &fresh.1));
                }
                if shown.2 != fresh.2 {
                    found.push(laid_proof::first_report_difference("labels' reports", &shown.2, &fresh.2));
                }
                if shown.3 != fresh.3 {
                    found.push(laid_proof::first_report_difference("raid icons' reports", &shown.3, &fresh.3));
                }
            }
            (shown, fresh) => {
                if shown.is_err() != fresh.is_err() {
                    found.push(format!("one draws and the other is refused (shown: {}, afresh: {})", shown.is_ok(), fresh.is_ok()));
                }
            }
        }
        let pairing = format!(
            "laid out over the tables of build {} and placed at {:?}, the frame holds build {} and draws at {:?}",
            self.prepared_overlay.as_ref().map_or(0, crate::document::PreparedOverlay::tables),
            self.overlay_placed.map(|placed| (placed.center, placed.height)),
            tables.identity(),
            (camera.center, camera.height)
        );
        profile_print(format!(
            "CHECK overlay {what} {}",
            if found.is_empty() { "equal".to_owned() } else { format!("DIFFER ({pairing}): {}", found.join("; ")) }
        ));
        if found.is_empty() {
            Ok(())
        } else {
            Err(format!("the overlay {what} differs from the same hour, view and camera laid out afresh ({pairing}): {}", found.join("; ")))
        }
    }

    pub(crate) fn service(&self) -> Option<Arc<dashboard::Service>> {
        self.embedded
            .as_ref()
            .map(crate::embedded::Controller::service)
            .or_else(|| self.harness_service.clone())
    }

    pub fn new(
        ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        config: Config,
        options: Options,
        error: Option<String>,
        failure: Arc<Mutex<Option<String>>>,
        embedded: Option<crate::embedded::Controller>,
    ) -> Self {
        Self::new_with_video_source(ctx, render_state, config, options, error, failure, embedded, None)
    }

    #[expect(clippy::too_many_arguments, reason = "extends Viewer::new with verified video export replay and settings before initial load")]
    pub(crate) fn new_for_video_export(
        ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        config: Config,
        options: Options,
        failure: Arc<Mutex<Option<String>>>,
        replay: Arc<replay::campaign::Replay>,
        revision: String,
        settings: Option<dashboard_core::protocol::ResolvedMapSettings>,
    ) -> Self {
        Self::new_with_video_source(
            ctx,
            render_state,
            config,
            options,
            None,
            failure,
            None,
            Some((replay, revision, settings)),
        )
    }

    #[expect(clippy::too_many_arguments, reason = "shared Viewer constructor installing optional video preload prior to initial open")]
    fn new_with_video_source(
        ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        config: Config,
        options: Options,
        error: Option<String>,
        failure: Arc<Mutex<Option<String>>>,
        embedded: Option<crate::embedded::Controller>,
        video_source: Option<(
            Arc<replay::campaign::Replay>,
            String,
            Option<dashboard_core::protocol::ResolvedMapSettings>,
        )>,
    ) -> Self {
        ctx.set_theme(egui::Theme::Dark);
        let mut viewer = Self::create(
            config,
            options,
            error,
            failure,
            embedded,
            render_state.map(|state| (state.device.clone(), state.queue.clone())),
        );
        if let Some((replay, revision, settings)) = video_source {
            viewer.configure_video_source(replay, revision, settings);
        }
        if viewer.embedded.is_some() {
            match crate::worker_pool::WorkerPool::new() {
                Ok(pool) => {
                    viewer.dropper = Some(Arc::clone(pool.dropper()));
                    viewer.worker_pool = Some(Arc::clone(&pool));
                    viewer.shared_pool = Some(pool);
                }
                Err(error) => {
                    viewer.error = Some(format!("Native map workers: {error}"));
                    return viewer;
                }
            }
        }
        if let Some(path) = viewer.options.replay.clone() {
            viewer.open(path, ctx, None, None);
        }
        viewer
    }

    fn create(
        config: Config,
        options: Options,
        mut error: Option<String>,
        failure: Arc<Mutex<Option<String>>>,
        embedded: Option<crate::embedded::Controller>,
        gpu: Option<(wgpu::Device, wgpu::Queue)>,
    ) -> Self {
        if let Some((device, _)) = &gpu {
            crate::perf_log::gpu_timer(viewer_gpu::gpu_frame_time::support(device));
        }
        if let Some(embedded) = &embedded
            && let Err(tables_error) = embedded
                .service()
                .configure_content(&config, options.allow_unverified)
        {
            error = Some(format!("Game tables: {tables_error}"));
        }
        let capture = options.window_dump.then(|| Capture {
            path: options.out.clone().unwrap_or_default(),
            remaining: if profiling() {
                if std::env::var("BATTLEPLAN_VIEWER_PROFILE").or_else(|_| std::env::var("HOI4_VIEWER_PROFILE")).is_ok_and(|mode| mode == "seek_end") {
                    1921
                } else {
                    profiling_sweep().end
                }
            } else {
                1
            },
            pinned: Some((options.size, options.camera)),
            exit: true,
            time: Some(options.animation_time),
            step: options.animation_step,
            frames: options.window_frames.max(1),
            index: 0,
            paced: false,
            export: None,
            video: None,
        });
        let perspective = options.perspective.clone();
        let map_mode = options.map_mode.clone();
        let counter_view = CounterView::new(
            options.counter_settings(&content_reader_settings::settings::MapOptions::DEFAULT),
        );
        let hidden_since = embedded.as_ref().map(|_| Instant::now());
        if options.playback_cost_end.is_none() {
            refresh_monitors(|| ());
        }
        Self {
            dashboard: None,
            dashboard_active: false,
            popout_open: false,
            dashboard_map: false,
            host_graphics: false,
            shared_pool: None,
            source: None,
            loaded_config: None,
            rendered: None,
            rendered_sequence: 0,
            drawn_at: None,
            drawn_frame: 0,
            applying_options: None,
            drawn_options: None,
            hover: None,
            tooltip: None,
            tooltip_delay_ms: None,
            perf_view: None,
            map_warnings: Vec::new(),
            diagnostics: options.diagnostics,
            diagnostics_open: false,
            audio: None,
            audio_focus: true,
            campaign_tab: false,
            muted: false,
            audio_reset: false,
            music: None,
            battles: None,
            interacted: false,
            interface_points: 1.0,
            embedded,
            harness_service: None,
            embedded_open: None,
            embedded_seek: None,
            revision: None,
            expected_revision: None,
            expected_open_id: None,
            installed_open_id: None,
            hidden_since,
            combat: combat::Window::default(),
            awaiting_folders: config.resolve_options().is_err(),
            camera: options.camera,
            camera_manual: options.camera.is_some(),
            settled_capture: false,
            settled_poses: false,
            held_view: None,
            held_clock: None,
            zoom: Motion::default(),
            camera_mode: if options.walk.is_some() {
                CameraMode::Walk
            } else if options.fly.is_some() {
                CameraMode::Fly
            } else {
                CameraMode::Map
            },
            free_overlays: false,
            start_pick: drop::Pick::Idle,
            free_overlay: Batch::default(),
            overlay_written: 0,
            free_presented: None,
            looks: None,
            input: crate::controls::Input::default(),
            orbit: crate::controls::orbit::Orbit::default(),
            walk: walk::Walk::new(options.walk.clone()),
            flight: None,
            free_cam: None,
            free_cam_speed: None,
            fly_view: Default::default(),
            last_fly_eye: None,
            mode_handover: None,
            hud: hud::Hud::default(),
            flying: None,
            fly_clock: None,
            fly_footprints: Default::default(),
            pointer_grabbed: false,
            postfx: options.postfx,
            graphics: dashboard_core::protocol::MapGraphics {
                day_night: options.day_night.unwrap_or(option_defaults::DAY_NIGHT),
                show_allied_plans: options.allied_plans.unwrap_or(option_defaults::ALLIED_PLANS),
                ..Default::default()
            },
            counter_filter: Default::default(),
            player_counters: option_defaults::PLAYER_COUNTERS,
            volumes: Default::default(),
            overlay_layers: Default::default(),
            counter_owners: Default::default(),
            resolved_settings: Default::default(),
            ui_scale: Setting::MatchGame,
            fly_aircraft: Default::default(),
            match_game: None,
            monitor_seen: None,
            map_preferences_seen: None,
            visual_check: None,
            visual_check_after_load: Default::default(),
            visual_check_note: None,
            stale_visual_revision: None,
            visual_fallbacks: Default::default(),
            visual_report: None,
            drew_frame: false,
            warming: false,
            visual_stale_campaign: None,
            config,
            options,
            document: None,
            loading: None,
            startup_content: content_reader_layers::content::Generation::loading(),
            error,
            scene: None,
            window_layout: None,
            scene_build: None,
            scene_error: None,
            scene_target: Presented::default(),
            map_error: None,
            playing: false,
            speed: 24.0,
            clock_anchor: None,
            page_clock: Default::default(),
            day_night: Default::default(),
            last_step: Instant::now(),
            profile_playback: Default::default(),
            started: Instant::now(),
            ambient_clock: Default::default(),
            opened: None,
            playhead: None,
            frame_head: Default::default(),
            animation_seek: false,
            map_animation_target: None,
            playhead_map: None,
            playhead_opened: false,
            seek_owed: false,
            overlay_hour: None,
            shown_whole: false,
            switch_trace: Default::default(),
            observing: Default::default(),
            playhead_opening: None,
            playhead_refused: None,
            presented: MapPresented::default(),
            last_view: None,
            uploaded: Uploaded::default(),
            dropper: None,
            worker_pool: None,
            raid_tick: None,
            convoys_sailing: false,
            convoy_refused: BTreeMap::new(),
            placements: None,
            fade: None,
            hovered_name_region: None,
            last_frame: Instant::now(),
            camera_step: None,
            label_report: LabelReport::default(),
            faction_highlight: Default::default(),
            hover_rows: Default::default(),
            head_rows: Default::default(),
            highlight_wanted: false,
            overlay_frame: None,
            overlay_moved: Arc::default(),
            overlay_tables: None,
            overlay_routes_pending: false,
            overlay_left: Default::default(),
            overlay_lay_ins: None,
            overlay_shown: None,
            overlay_view: None,
            overlay_placed_view: None,
            overlay_head_before: None,
            overlay_counted: Default::default(),
            overlay_unpainted: false,
            overlay_generation: None,
            overlay_clock_unavailable: false,
            overlay_shifts: Default::default(),
            overlay_camera_before: None,
            overlay_placed: None,
            overlay_asked: None,
            overlay_layered: None,

            overlay_drawn_fraction: None,
            overlay_lookups: None,

            overlay_installed: 0,
            overlay_checked: 0,
            overlay_looks: 0,
            overlay_loading: Vec::new(),
            held_clicks: Default::default(),
            overlay_missed: Vec::new(),
            combat_rows: Default::default(),
            tooltip_refused: Default::default(),
            counter_report: CounterReport::default(),
            combat_painted: Default::default(),
            overlay_hits: Default::default(),
            map_hits: Default::default(),
            hits_stamp: 0,
            map_tooltip_target: None,
            shown_tooltip: None,
            interface_sounds: Vec::new(),
            interface_key: 0,
            counter_held: None,
            pressed_on: None,
            counter_selecting: None,
            secondary_on: None,
            air_orders: Default::default(),
            selection_input: Default::default(),
            overlay: None,
            prepared_overlay: None,
            overlay_pools: Default::default(),
            overlay_fraction: None,
            overlay_art_moved: false,
            overlay_art_retries: 0,
            held_since: None,
            combat_inputs: None,

            counter_view,
            unit_selection: Default::default(),
            air_selection: Default::default(),
            fleet_selection: Default::default(),
            perspective,
            map_mode,
            diplomatic_selected: None,
            ground_selection: None,
            picker: Default::default(),
            picker_error: None,
            picker_refused: Default::default(),
            picker_controls: None,
            capture,
            capture_note: None,
            private_playhead: false,
            video_crop: None,
            video_preload: None,
            failure,
            gpu,
            profile_frame: 0,
            laps: None,
        }
    }

    pub(crate) fn map_view(
        config: Config,
        mut options: Options,
        gpu: Option<(wgpu::Device, wgpu::Queue)>,
        pool: Arc<crate::worker_pool::WorkerPool>,
    ) -> Self {
        options.replay = None;
        options.window_dump = false;
        options.headless_dump = false;
        options.combat = None;
        let mut viewer = Self::create(config, options, None, Arc::new(Mutex::new(None)), None, gpu);
        viewer.dashboard_map = true;
        viewer.diagnostics = false;
        viewer.audio_focus = false;
        viewer.dropper = Some(Arc::clone(pool.dropper()));
        viewer.worker_pool = Some(Arc::clone(&pool));
        viewer.shared_pool = Some(pool);
        viewer
    }

    pub(super) fn worker_pool(&mut self) -> std::io::Result<Arc<crate::worker_pool::WorkerPool>> {
        if let Some(pool) = &self.worker_pool {
            return Ok(Arc::clone(pool));
        }
        let pool = crate::worker_pool::WorkerPool::new()?;
        self.dropper.get_or_insert_with(|| Arc::clone(pool.dropper()));
        self.worker_pool = Some(Arc::clone(&pool));
        Ok(pool)
    }

    fn take_game_options(&mut self, game: &content_reader_settings::settings::MapOptions) {
        self.counter_view = CounterView::new(self.options.counter_settings(game));
        if !self.host_graphics {
            self.graphics.day_night = self.options.day_night.unwrap_or(game.day_night);
            self.graphics.show_allied_plans = self.options.allied_plans.unwrap_or(game.allied_plans);
        }
    }

    pub(crate) fn install_map(&mut self, document: Document, scene: Scene, revision: String) {
        self.clock_anchor = None;
        self.playhead = None;
        self.day_night = Default::default();
        self.ambient_clock = Default::default();
        self.take_game_options(&document.game_options);
        self.unit_selection = Default::default();
        self.air_selection = Default::default();
        self.fleet_selection = Default::default();
        self.head_rows = Default::default();
        self.held_clicks.clear();
        self.forget_overlay_builds();
        self.combat.content_replaced();
        self.close_playhead();
        self.presented = MapPresented::default();
        self.shown_whole = false;
        drop(self.replace_document(Some(document)));
        if !self.dashboard_map
            && let Some(scene) = &mut self.scene
        {
            crate::perf_log::gpu_times(scene.retire_gpu_frame_times());
        }
        self.scene = Some(scene);
        self.revision = Some(revision);
        self.error = None;
        self.map_error = None;
        self.rendered = None;
        self.uploaded = Uploaded::default();
    }

    fn after_frame_with_visual_mods(&mut self, document: &Document, error: Option<&String>) {
        let Some(built) = document.content.fallback.clone() else {
            return;
        };
        let mut refusals = self
            .scene
            .as_ref()
            .map(|scene| scene.draw_refusals())
            .unwrap_or_default();
        if let Some(error) = error {
            refusals.insert(format!("frame: {error}"));
        }
        refusals.extend(document.content.uncovered_reads.refused());
        let requested = self.playhead.map_or(
            (document.playback.branch(), document.playback.hour()),
            |head| (head.branch, head.hour),
        );
        let shown = self.playhead_map.as_ref().and_then(|side| {
            side.on_screen()
                .filter(|on| on.settled)
                .and_then(|on| Some((side.branch().unwrap_or(requested.0), on.drawn?, self.map_error.is_some())))
        });
        let settled = crate::visual_fallback::settled(
            self.drew_frame && error.is_none(),
            requested,
            self.rendered.as_ref().map(|head| (head.branch, head.hour)),
            shown,
            document.icons.lazy.busy(),
        );
        self.visual_report = Some(crate::visual_fallback::Report {
            built,
            refusals,
            settled,
        });
    }

    pub(crate) fn take_visual_report(&mut self) -> Option<crate::visual_fallback::Report> {
        self.visual_report.take()
    }

    pub(crate) fn has_map(&self) -> bool {
        self.document.is_some() && self.scene.is_some()
    }

    pub(crate) fn release_audio(&mut self) {
        if let Some(audio) = &mut self.audio {
            audio.release();
        }
    }

    fn silence_unshown(&mut self) {
        self.release_audio();
        self.audio_reset = true;
    }

    pub(crate) fn apply_audio_mix(&self) {
        if let (Some(audio), Some(document)) = (&self.audio, &self.document) {
            audio.set_mix(audio_mix(document, self.volumes, self.muted));
        }
    }

    pub(crate) fn follow_map_settings(
        &mut self,
        service: &dashboard::Service,
        scope: String,
    ) -> std::sync::Arc<dashboard_core::protocol::ResolvedMapSettings> {
        let settings =
            self.resolved_settings
                .get(service.map_settings_generation(), scope, |scope| {
                    service.scoped_map_settings(scope)
                });
        self.set_embedded_graphics(&settings);
        settings
    }

    fn layered_orders(&self) -> Option<LayeredOrders> {
        let side = live_of(&self.playhead_map)?;
        let plan = side.products().plan.as_deref().map(|(_, plan)| Arc::clone(plan));
        let routes = side.products().routes.as_deref().map(|(seq, _)| *seq);
        let raids = self.overlay_tables.as_ref().map(|tables| Arc::clone(tables.raid_arrows()));
        Some(LayeredOrders { plan, raids, routes })
    }

    fn frame_day_night(&self, hour: i32, speeds: &Result<GameSpeeds, String>) -> DayNight {
        let playing = self.playing || self.capture.as_ref().is_some_and(|c| c.video.is_some());
        let shown = speeds.as_ref().map_or(!playing, |speeds| {
            speeds.shows_cycle(playing, self.speed)
        });
        DayNight::at_hour(hour).with_cycle(self.graphics.day_night && shown)
    }

    pub(crate) fn configure_video_export(
        &mut self,
        session: crate::video_export::VideoSession,
        speed: f64,
        source_viewport: Option<[u32; 2]>,
        crop: crate::video_export::CropRect,
    ) {
        self.private_playhead = true;
        self.speed = speed;
        self.video_crop = (!crop.is_full()).then_some((source_viewport, crop));
        let size = session.size;
        let first_time = session
            .current()
            .map_or(0.0, |f| f.presentation_time as f32);
        let step = 1.0 / session.fps() as f32;
        self.capture = Some(Capture {
            path: PathBuf::from("video-export"),
            remaining: 1,
            pinned: Some((size, self.camera)),
            exit: true,
            time: Some(first_time),
            step,
            frames: 1,
            index: 0,
            paced: false,
            export: None,
            video: Some(Box::new(session)),
        });
    }

    pub(crate) fn cancel_video_export(&mut self) {
        if let Some(mut capture) = self.end_capture()
            && let Some(mut session) = capture.video.take()
        {
            session.finish_cancelled();
        }
    }

    pub(crate) fn fail_video_export(&mut self, error: String) {
        if let Some(mut capture) = self.end_capture()
            && let Some(mut session) = capture.video.take()
        {
            session.finish_render_failure(error);
        }
    }

    pub(crate) fn video_export_idle(&self) -> bool {
        self.capture.is_none()
    }

    pub(crate) fn build_export_snapshot(&self) -> Result<crate::video_export::MapExportSnapshot, String> {
        let document = self
            .document
            .as_ref()
            .ok_or_else(|| "Open a map before exporting".to_owned())?;
        let replay = document.playback.replay();
        let branch = self
            .playhead
            .map_or_else(|| document.playback.branch(), |head| head.branch);
        let current_hour = self
            .playhead
            .map_or_else(|| document.playback.hour(), |head| head.hour);
        let timeline = dashboard_query::timeline::resolve(replay, branch)
            .map_err(|e| format!("Couldn’t read branch timeline: {e}"))?;
        let (stretches, unheld) = crate::video_export::timeline_to_schedule_inputs(&timeline);
        let first_hour = stretches.first().map_or(current_hour, |s| s.first);
        let last_hour = stretches.last().map_or(current_hour, |s| s.last);
        let (placement, viewport, ui_scale) = if let Some(view) = &self.last_view {
            (view.camera.placement(), view.viewport, view.ui_scale)
        } else {
            let map_size = MapSize::of(&document.map);
            let fallback_vp = [1920, 1080];
            let cam = self.camera.unwrap_or_else(|| {
                Camera::fit(map_size, document.map_defines, fallback_vp).placement()
            });
            (cam, fallback_vp, 1.0)
        };
        let (perspective, perspective_slot) = match &self.perspective.country {
            viewer_foundation::perspective::Country::Tag(tag) => (Some(tag.clone()), None),
            viewer_foundation::perspective::Country::Slot { slot, tag } => {
                (Some(tag.clone()), Some(*slot))
            }
            viewer_foundation::perspective::Country::Recorded => (None, None),
        };
        let mut graphics = self.graphics;
        graphics.texture_quality = document.texture_quality;
        if let Some(scene) = &self.scene {
            graphics.samples = scene.samples();
        }
        let map_settings = Some(dashboard_core::protocol::ResolvedMapSettings {
            graphics,
            audio: self.volumes,
            counters: self.counter_filter.clone(),
            visual_mods: document.visual_mods.clone(),
        });
        let max_texture_dimension = self
            .gpu
            .as_ref()
            .map_or(8192, |(device, _)| device.limits().max_texture_dimension_2d);
        Ok(crate::video_export::MapExportSnapshot {
            path: document.source_path.to_string_lossy().into_owned(),
            revision: self.revision.clone(),
            branch,
            current_hour,
            placement: crate::video_export::SavedPlacement::from(placement),
            viewport,
            ui_scale,
            map_mode: self.shown_mode().token(),
            perspective,
            perspective_slot,
            fog: self.perspective.fog,
            day_night: self.graphics.day_night,
            allied_plans: self.graphics.show_allied_plans,
            counter_colors_country: self.counter_view.settings.color_mode
                == viewer_counter_kit::CounterColorMode::Country,
            nato_counters: self.counter_view.settings.nato,
            audio_muted: self.muted,
            map_settings,
            max_texture_dimension,
            stretches,
            unheld,
            first_hour,
            last_hour,
        })
    }

    pub(crate) fn set_embedded_graphics(
        &mut self,
        settings: &dashboard_core::protocol::ResolvedMapSettings,
    ) {
        let graphics = settings.graphics;
        if self.graphics.samples != graphics.samples {
            self.scene_error = None;
        }
        self.graphics = graphics;
        self.host_graphics = true;
        if self.counter_filter != settings.counters {
            self.counter_filter = settings.counters.clone();
        }
        if self.volumes != settings.audio {
            self.volumes = settings.audio;
            self.apply_audio_mix();
        }
        if let Some(document) = &self.document {
            self.counter_view.settings.nato = graphics
                .nato_counters
                .unwrap_or_else(|| self.options.counter_settings(&document.game_options).nato);
        }
    }

    pub(crate) fn configure_video_source(
        &mut self,
        replay: Arc<replay::campaign::Replay>,
        revision: String,
        settings: Option<dashboard_core::protocol::ResolvedMapSettings>,
    ) {
        if let Some(resolved) = &settings {
            self.set_embedded_graphics(resolved);
        }
        self.video_preload = Some((replay, revision, settings));
    }

    fn free_scene_target(&mut self, frame: &eframe::Frame) {
        if let Some(texture) = self.scene_target.take()
            && let Some(state) = frame.wgpu_render_state()
        {
            match crate::renderer_host::local(state) {
                Some(host) => host.free(texture),
                None => state.renderer.write().free_texture(&texture),
            }
        }
    }

    pub(crate) fn release_viewport(&mut self, frame: &eframe::Frame) {
        let Some(scene) = &mut self.scene else {
            return;
        };
        if scene.view().is_none() {
            return;
        }
        scene.release_viewport();
        self.free_scene_target(frame);
        self.rendered = None;
    }

    pub(crate) fn retire_map<T: Send + 'static>(&mut self, frame: &eframe::Frame, permit: T) {
        self.cancel_scene_build();
        self.scene_error = None;
        self.free_scene_target(frame);
        if !self.dashboard_map
            && let Some(scene) = &mut self.scene
        {
            crate::perf_log::gpu_times(scene.retire_gpu_frame_times());
        }
        let retired = (
            self.replace_document(None),
            self.scene.take(),
            self.overlay_tables.take(),
            self.prepared_overlay.take(),
            std::mem::take(&mut self.faction_highlight),
            permit,
        );
        if let Some(dropper) = &self.dropper {
            dropper.drop_later(retired);
        }
        self.overlay_hour = None;
        self.close_playhead();
        self.rendered = None;
    }

    pub(crate) fn holds_export(&self) -> bool {
        self.capture.as_ref().is_some_and(|capture| capture.export.is_some()) || self.dashboard.as_ref().is_some_and(|dashboard| dashboard.holds_export())
    }

    fn not_drawn(&mut self) {
        self.export_closed("its map was hidden before its picture was saved");
        self.release_audio();
        self.close_playhead();
        if let Some(dashboard) = &mut self.dashboard {
            dashboard.release_audio();
            dashboard.not_drawn();
        }
    }

    pub(crate) fn close_playhead(&mut self) {
        self.window_layout = None;
        for _ in 0..2 {
            self.hover_rows.end_frame(false);
            if self.playhead_map.is_some() {
                self.head_rows.end_frame(None);
                self.combat_rows.end_frame();
            }
        }
        if let Some(side) = self.playhead_map.take() {
            self.drop_later(side);
        }
        self.frame_head.forget();
        self.playhead_opening = None;
        self.playhead_refused = None;
        self.playhead_opened = false;
    }

    fn drop_later(&self, side: viewer_playback::side::SideThread) {
        match &self.dropper {
            Some(dropper) => dropper.drop_later(side),
            None => drop(side),
        }
    }

    pub(crate) fn retry_map(&mut self) {
        self.error = None;
        self.scene_error = None;
        self.map_error = None;
    }

    fn unload_recording(&mut self, path: &Path) -> bool {
        if self
            .embedded_open
            .as_ref()
            .is_some_and(|pending| pending.path == path)
        {
            self.embedded_open = None;
        }
        if self
            .embedded_seek
            .as_ref()
            .is_some_and(|pending| pending.path == path)
        {
            self.embedded_seek = None;
        }
        if self.options.replay.as_deref() != Some(path) {
            return false;
        }
        self.options.replay = None;
        self.cancel_scene_build();
        self.scene_error = None;
        self.close_playhead();
        self.hover_rows = Default::default();
        self.combat_rows = Default::default();
        self.tooltip_refused.clear();
        self.source = None;
        self.loaded_config = None;
        self.revision = None;
        self.expected_revision = None;
        self.expected_open_id = None;
        self.installed_open_id = None;
        self.map_preferences_seen = None;
        self.playhead = None;
        self.seek_owed = false;
        self.opened = None;
        self.playing = false;
        self.export_closed("its recording closed");
        self.combat.close();
        self.retire_combat_inputs();
        self.error = None;
        self.map_error = None;
        true
    }

    pub(crate) fn map_ui(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        target: &dashboard_core::protocol::DashboardMapView,
    ) {
        self.host_perspective(&target.options);
        self.applying_options = Some(target.options.clone());
        if crate::perf_log::enabled() && self.perf_view.as_deref() != Some(target.id.as_str()) {
            self.perf_view = Some(target.id.clone());
        }
        self.apply_host_options(&target.options);
        self.picker.find.focus(target.focus_province);
        let branch = target
            .branch_id
            .parse::<u64>()
            .expect("validated map command");
        let retired = self.page_clock.is_retired(&target.clock_id);
        let head = Playhead {
            branch,
            hour: target.hour,
            fraction: f64::from(target.fraction),
        };
        let valid = self
            .document
            .as_ref()
            .and_then(|document| document.playback.range(branch));
        if !retired {
            let animation_target = Target {
                branch,
                hour: target.hour,
                seek_sequence: target.seek_sequence,
            };
            let sought = self
                .map_animation_target
                .is_some_and(|old| animation_target.seeks_from(old));
            self.animation_seek |= sought;
            self.map_animation_target = Some(animation_target);
            if let Some((_, last)) = valid {
                self.anchor_playback(
                    head,
                    target.playing,
                    target.hours_per_second,
                    sought,
                    Some(crate::page_clock::ClockSample {
                        origin: &target.clock_id,
                        composed_at: target.composed_at,
                    }),
                    Instant::now(),
                    last,
                );
            }
        }
        self.error = match valid {
            Some((first, last)) if (first..=last).contains(&target.hour) => None,
            Some((first, last)) => Some(format!(
                "Requested hour is outside this branch ({first}..{last})"
            )),
            None => Some("The requested branch is not present in this recording".into()),
        };
        if let Some(error) = self.error.clone() {
            self.refuse_capture(&error, ui.ctx());
            self.rendered = None;
            self.silence_unshown();
            ui.centered_and_justified(|ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            });
            return;
        }
        if let Some(mut document) = self.document.take() {
            let result = self.map(ui, frame, &mut document, Some(target.id.as_str()));
            self.after_frame_with_visual_mods(&document, result.as_ref().err());
            if let Err(error) = result {
                self.refuse_capture(&error, ui.ctx());
                self.error = Some(error);
                self.rendered = None;
            }
            self.document = Some(document);
        }
        if !retired
            && self.presentation_ready(self.playhead.unwrap_or(head), target.playing)
            && self.error.is_none()
            && self.map_error.is_none()
        {
            self.rendered_sequence = target.sequence;
        }
    }

    pub(crate) fn reset_drawn_frame(&mut self) {
        self.drew_frame = false;
    }

    pub(crate) fn drew_frame(&self) -> bool {
        self.drew_frame
    }

    fn tick_playhead(&mut self, seconds: f64, last: i32) {
        if let Some(side) = &self.playhead_map {
            side.entry().follow_rate();
        }
        if self.playing
            && let Some(head) = &mut self.playhead
        {
            self.playing = tick_clock(head, self.speed, seconds, last);
        }
    }

    fn anchor_playback(
        &mut self,
        target: Playhead,
        playing: bool,
        speed: f64,
        sought: bool,
        composed: Option<crate::page_clock::ClockSample<'_>>,
        now: Instant,
        last: i32,
    ) {
        let Some(observed) = self.page_clock.observe(composed, now) else {
            return;
        };
        let discontinuity = observed.new_origin && self.playhead.is_some();
        if observed.new_origin {
            self.clock_anchor = None;
        }
        let sought = sought || discontinuity;
        let anchor = (target, playing, speed);
        self.seek_owed |= sought;
        if sought {
            self.frame_head.forget();
        }
        if !sought && !self.seek_owed && self.playhead.is_some() && self.clock_anchor == Some(anchor) {
            return;
        }
        if self.capture.is_none() {
            self.tick_playhead(now.duration_since(self.last_step).as_secs_f64(), last);
        }
        self.last_step = now;
        let moving = playing && !sought;
        let current = (!observed.new_origin).then_some(self.playhead).flatten();
        self.playhead = Some(forward_only(
            current,
            placed_anchor(target, moving, speed, observed.age, last),
            moving,
        ));
        self.speed = speed;
        self.playing = playing && self.playhead.is_some_and(|head| head.hour < last);
        self.clock_anchor = Some(anchor);
        self.follow_branch();
        self.give_anchor(target.hour, playing, speed);
    }

    fn give_anchor(&mut self, hour: i32, playing: bool, speed: f64) {
        let Some(side) = &self.playhead_map else { return };
        let sought = std::mem::take(&mut self.seek_owed);
        if sought {
            self.presented.seek = side.on_screen().map(|on| on.resets);
        }
        side.anchor(hour, playing, speed, sought);
    }

    fn side_opened(&mut self, side: viewer_playback::side::SideThread) {
        self.playhead_map = Some(side);
        if let Some(clock) = self.playhead {
            self.give_anchor(clock.hour, self.playing, self.speed);
        }
    }

    fn saves_a_frame(&self) -> bool {
        self.capture.as_ref().is_some_and(|capture| !capture.paced) || self.options.window_dump
    }

    fn settles_windows(&self) -> bool {
        self.saves_a_frame() && self.embedded.is_none() && !self.dashboard_map
    }

    fn saved_combat(&mut self, picture: bool, left_out: Option<&str>) -> Result<bool, String> {
        if let Some(why) = &self.combat_rows.selector_refused {
            return Err(why.clone());
        }
        if let Some(selector) = self.combat_rows.pending {
            self.combat_rows.selector_waits += u32::from(picture);
            if self.combat_rows.selector_waits > SETTLES {
                return Err(format!("{selector:?}, named at load, was not looked up: its rows of LandCombats, NavalCombats and NavalResults did not come in {SETTLES} frames of the picture"));
            }
            return Ok(false);
        }
        let named = self.options.combat.is_some();
        if self.combat.loading_art() || self.combat.loading_title() { return Ok(false); }
        if self.combat.selection.is_some() && self.combat.is_loading() {
            return match live_of(&self.playhead_map).and_then(|side| side.products().combat_whole.clone()) {
                Some(Err(why)) => Err(format!("its read was not made: {why}")),
                Some(Ok(())) => Err("its read was made but is not of the hour on screen".into()),
                None => Ok(false),
            };
        }
        match (self.combat.not_shown(), left_out) {
            (Some(why), _) if named => Err(why),
            (None, Some(why)) if named => Err(format!("{:?}: {why}", self.combat.selection)),
            _ => Ok(true),
        }
    }

    fn hold_saved_frame(&mut self, hour: replay_data::Hour, stale: Option<bool>, waiting: Option<String>, ctx: &egui::Context) -> Result<(), String> {
        self.hold_saved_frame_within(hour, stale, waiting, HELD_GIVEN_UP, ctx)
    }

    fn hold_saved_frame_within(&mut self, hour: replay_data::Hour, stale: Option<bool>, waiting: Option<String>, given_up: Duration, ctx: &egui::Context) -> Result<(), String> {
        let Some(capture) = self.capture.as_mut().filter(|capture| !capture.paced) else { return Ok(()) };
        let due = capture.remaining <= 1;
        let held = stale != Some(false) || waiting.is_some();
        if held {
            capture.remaining = capture.remaining.max(1);
        }
        if !(held && due) {
            self.held_since = None;
        } else {
            let what = || {
                waiting.clone().unwrap_or_else(|| match stale {
                    None => "no overlay was laid out for it".to_owned(),
                    _ => "its overlay was older than its art".to_owned(),
                })
            };
            if !self.held_within(hour, given_up, what, ctx)? {
                return Ok(());
            }
        }
        if stale != Some(true) {
            self.overlay_art_retries = 0;
            return Ok(());
        }
        if !due {
            return Ok(());
        }
        self.overlay_art_retries += 1;
        if self.overlay_art_retries > SETTLES {
            let why = format!("A saved frame draws the art of its own hour: art still arrived after the picture of hour {hour} was drawn, {SETTLES} frames running");
            return self.refused_frame(why, ctx);
        }
        self.overlay_fraction = None;
        self.overlay_art_moved = true;
        Ok(())
    }

    fn held_within(&mut self, hour: replay_data::Hour, given_up: Duration, what: impl FnOnce() -> String, ctx: &egui::Context) -> Result<bool, String> {
        if self.held_since.get_or_insert_with(Instant::now).elapsed() < given_up {
            return Ok(true);
        }
        let why = format!("A saved frame draws its own hour whole: hour {hour} was still held after {} s, {}", given_up.as_secs(), what());
        self.refused_frame(why, ctx).map(|()| false)
    }

    fn wait_to_draw(&mut self, hour: replay_data::Hour, what: impl FnOnce() -> String, ctx: &egui::Context) -> Result<(), String> {
        self.wait_to_draw_within(hour, what, HELD_GIVEN_UP, ctx)
    }

    fn wait_to_draw_within(&mut self, hour: replay_data::Hour, what: impl FnOnce() -> String, given_up: Duration, ctx: &egui::Context) -> Result<(), String> {
        if !self.capture.as_ref().is_some_and(|capture| !capture.paced) {
            return Ok(());
        }
        self.held_within(hour, given_up, what, ctx).map(drop)
    }

    fn install_overlay(&mut self, made: viewer_playback::overlay::Made, gaps: &[replay_frame::branches::Gap]) {
        let viewer_playback::overlay::Made {
            generation,
            hour,
            branch,
            mut prepared,
            tables,
            rows,
            placed_at,
            placed_pixels,
            placed_ui_scale,
            on: _,
            published,
            draws,
            view,
            left,
            lay_ins,
            moved,
        } = made;
        let current_epoch = self
            .counter_view
            .presentation
            .movement()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .continuity();
        let current_generation = live_of(&self.playhead_map)
            .and_then(|side| side.products().overlay_generation);
        let matches_continuity = self.presented.seek.is_none()
            && self.presented.last.as_ref().is_none_or(|(shown_branch, shown_hour, shown_viewing)| {
                overlay_matches_continuity(
                    (generation, rows.branch, rows.hour, tables.ours()),
                    (*shown_branch, *shown_hour, shown_viewing.as_deref()),
                    current_generation,
                    gaps,
                )
            });
        prepared.set_motion_continuity(if matches_continuity {
            current_epoch
        } else {
            current_epoch.saturating_sub(1)
        });
        self.overlay_generation = Some(generation);
        self.overlay_moved = moved;
        if let Some(old) = self.prepared_overlay.as_mut()
            && self.overlay_view.as_ref() == Some(&view)
        {
            prepared.carry_plates(old);
        }
        prepared.set_counter_view(self.counter_view.clone());
        let retired = (self.prepared_overlay.replace(prepared), self.overlay_tables.replace(tables), self.overlay_frame.replace(rows));
        self.retire(retired);
        self.overlay_routes_pending = true;
        self.overlay_left = left;
        self.overlay_lay_ins = Some(lay_ins);
        self.overlay_layered = Some((published, draws));
        self.overlay_view = Some(view);
        self.overlay_shown = Some((branch, hour));
        self.overlay_placed = Some(placed_at);
        self.overlay_placed_view = Some((placed_pixels, placed_ui_scale.to_bits()));
        self.overlay_fraction = None;
        self.overlay_unpainted = true;
        self.overlay_installed += 1;
        viewer_runtime::redraws::note("made.laid", 1);
    }

    fn withhold_overlay(&mut self, why: &str) {
        self.drop_overlay_drawn();
        self.overlay_unpainted = true;
        self.overlay_fraction = None;
        self.overlay_drawn_fraction = None;
        self.overlay_loading
            .push(format!("the overlay is withheld: {why}"));
    }

    #[expect(clippy::too_many_arguments, reason = "the frame's camera and what the placing reads")]
    fn place_overlay(
        &mut self,
        now: &crate::document::Hour<'_>,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        tables: &crate::document::OverlayTables,
        rows: &Arc<replay_frame::frame::DecodedFrame>,
        left: viewer_layers_political::left_to_layers::LeftToLayers,
    ) -> OverlayPlaced {
        if self.overlay_placed == Some(camera)
            && self.overlay_placed_view == Some((pixels, ui_scale.to_bits()))
        {
            return OverlayPlaced::Unchanged;
        }
        let Some(prepared) = self.prepared_overlay.as_mut() else {
            return OverlayPlaced::Unchanged;
        };
        let read = crate::document::OverlayRows::at_head(Some(Arc::clone(rows))).leaving(left);
        match prepared.project(
            now,
            camera,
            pixels,
            ui_scale,
            tables,
            &self.counter_view,
            &read,
            self.overlay_lay_ins.as_deref(),
        ) {
            Ok(()) => {
                viewer_runtime::redraws::note("made.placed", 1);
                self.overlay_placed = Some(camera);
                self.overlay_placed_view = Some((pixels, ui_scale.to_bits()));
                self.overlay_fraction = None;
                self.overlay_shifts.clear();
                OverlayPlaced::Placed
            }
            Err(error) => {
                let why = error.to_string();
                self.withhold_overlay(&why);
                OverlayPlaced::Failed(why)
            }
        }
    }

    fn place_painting(
        &mut self,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        at: viewer_gpu::icon_tracks::Head,
    ) -> Result<PaintingPlaced, String> {
        let Some(placed) = self.overlay_placed else {
            return Ok(PaintingPlaced::Placed);
        };
        if self.overlay_placed_view != Some((pixels, ui_scale.to_bits())) {
            return Ok(PaintingPlaced::Refused(
                viewer_native_gui::anchored::Refused::NotAMove,
            ));
        }
        if placed == camera
            && (camera.free().is_some() || !self.overlay.as_ref().is_some_and(Batch::moves))
        {
            return Ok(PaintingPlaced::Placed);
        }
        let extent = viewer_native_gui::gui_resources::viewport(pixels, ui_scale);
        let _span = viewer_runtime::profiling::span("overlay_painting_placed");
        let batch = self.overlay.as_mut().expect("the batch this frame painted");
        let moved = match self
            .overlay_shifts
            .move_to(batch, &placed, &camera, pixels, extent, at, check_anchor())
        {
            Ok(moved) => moved,
            Err(why) => {
                viewer_runtime::redraws::note("made.not_moved", 1);
                if viewer_runtime::view_record::on() {
                    viewer_runtime::view_record::add("overlay", why.why(), 1);
                }
                return Ok(PaintingPlaced::Refused(why));
            }
        };
        viewer_runtime::redraws::note("made.painting_placed", 1);
        viewer_runtime::redraws::note("moved.movers", moved.movers as u64);
        viewer_runtime::view_record::add("overlay", "movers_placed", moved.movers as u64);
        viewer_runtime::view_record::add("overlay", "quads_moved", moved.quads as u64);
        for (name, value) in moved.named() {
            viewer_runtime::view_record::add("overlay", name, value);
        }
        self.hits_stamp += 1;
        if !self.overlay_shifts.found().is_empty() {
            return Err(format!(
                "the overlay's painting does not stand where this frame's camera and head put its anchors: {}",
                self.overlay_shifts.found().join("; ")
            ));
        }
        Ok(PaintingPlaced::Placed)
    }

    #[expect(clippy::too_many_arguments, reason = "the frame's camera, view, head, clock state and repaint hook")]
    fn finish_painting(
        &mut self,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        at: viewer_gpu::icon_tracks::Head,
        hour_fraction: f32,
        clock_unavailable: bool,
        relayout_and_repaint: impl FnOnce(&mut Self) -> Result<bool, String>,
    ) -> Result<Option<viewer_native_gui::anchored::Refused>, String> {
        self.overlay_shifts.clear();
        let outcome = match self.place_painting(camera, pixels, ui_scale, at)? {
            PaintingPlaced::Placed => PaintingPlaced::Placed,
            PaintingPlaced::Refused(why) => {
                let needs_relayout = self.overlay_placed != Some(camera)
                    || self.overlay_placed_view != Some((pixels, ui_scale.to_bits()));
                if needs_relayout && relayout_and_repaint(self)? {
                    self.overlay_shifts.clear();
                    self.place_painting(camera, pixels, ui_scale, at)?
                } else {
                    PaintingPlaced::Refused(why)
                }
            }
        };
        match outcome {
            PaintingPlaced::Placed => {
                if viewer_runtime::profiling::counting()
                    && let Some(batch) = self.overlay.as_ref()
                {
                    self.overlay_counted.painted(batch).record();
                }
                self.overlay_unpainted = false;
                self.overlay_clock_unavailable = clock_unavailable;
                self.overlay_fraction = Some(hour_fraction);
                self.overlay_drawn_fraction = Some(hour_fraction);
                Ok(None)
            }
            PaintingPlaced::Refused(why) => {
                self.withhold_overlay(why.why());
                Ok(Some(why))
            }
        }
    }

    fn saved_overlay_camera_waiting(
        &self,
        interface: bool,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
    ) -> Option<String> {
        if !interface {
            return None;
        }
        if self.overlay_placed != Some(camera)
            || self.overlay_placed_view != Some((pixels, ui_scale.to_bits()))
        {
            return Some("its overlay was not laid out for this frame's camera".to_owned());
        }
        if self.overlay_unpainted || self.overlay.is_none() {
            return Some("its overlay was withheld or not painted for this frame's camera".to_owned());
        }
        None
    }

    fn move_overlay(
        &mut self,
        camera: Camera,
        pixels: [u32; 2],
        ui_scale: f32,
        at: viewer_gpu::icon_tracks::Head,
    ) -> Result<bool, String> {
        let moved = self.overlay_camera_before.is_some_and(|before| before != camera);
        self.overlay_camera_before = Some(camera);
        let advanced = self.overlay_head_before.is_some_and(|before| before != at)
            && self.overlay.as_ref().is_some_and(Batch::moves);
        self.overlay_head_before = Some(at);
        let (Some(placed), true) = (self.overlay_placed, moved || advanced) else {
            return Ok(false);
        };
        if self.overlay_unpainted {
            viewer_runtime::redraws::note("made.not_moved", 1);
            if viewer_runtime::view_record::on() {
                viewer_runtime::view_record::add("overlay", "the batch is the painting before the overlay installed", 1);
            }
            return Ok(false);
        }
        if (placed == camera && !advanced)
            || self.overlay_placed_view != Some((pixels, ui_scale.to_bits()))
            || self.overlay_drawn_fraction.is_none()
        {
            return Ok(false);
        }
        let Some(batch) = self.overlay.as_mut() else {
            return Ok(false);
        };
        let extent = viewer_native_gui::gui_resources::viewport(pixels, ui_scale);
        let _span = viewer_runtime::profiling::span("overlay_move");
        match self
            .overlay_shifts
            .move_to(batch, &placed, &camera, pixels, extent, at, check_anchor())
        {
            Ok(moved) => {
                viewer_runtime::redraws::note("made.moved", 1);
                viewer_runtime::redraws::note("moved.movers", moved.movers as u64);
                viewer_runtime::redraws::note("moved.pictures", u64::from(moved.quads > 0));
                viewer_runtime::view_record::add("overlay", "movers_placed", moved.movers as u64);
                viewer_runtime::view_record::add("overlay", "movers_still", moved.movers_still as u64);
                for (name, value) in moved.named() {
                    viewer_runtime::view_record::add("overlay", name, value);
                }
                viewer_runtime::view_record::set("overlay", "step_max", moved.step_max.unsigned_abs().into());
                viewer_runtime::view_record::add("overlay", "anchors_moved", moved.anchors_moved as u64);
                viewer_runtime::view_record::add("overlay", "quads_moved", moved.quads as u64);
                viewer_runtime::view_record::set("overlay", "anchors", moved.anchors as u64);
                self.hits_stamp += 1;
                if !self.overlay_shifts.found().is_empty() {
                    return Err(format!(
                        "the overlay's windows do not stand where this frame's camera puts their anchors: {}",
                        self.overlay_shifts.found().join("; ")
                    ));
                }
                Ok(true)
            }
            Err(why) => {
                viewer_runtime::redraws::note("made.not_moved", 1);
                if viewer_runtime::view_record::on() {
                    viewer_runtime::view_record::add("overlay", why.why(), 1);
                }
                Ok(false)
            }
        }
    }

    fn replace_document(&mut self, document: Option<Document>) -> Option<Document> {
        self.retire_combat_inputs();
        std::mem::replace(&mut self.document, document)
    }

    fn retire_combat_inputs(&mut self) {
        self.combat_inputs = None;
        let inputs = self.combat.take_inputs();
        self.retire(inputs);
    }

    fn retire<T: Send + 'static>(&self, value: T) {
        match &self.dropper {
            Some(dropper) => dropper.drop_later(value),
            None => drop(value),
        }
    }

    fn forget_overlay_builds(&mut self) {
        self.forget_overlay();
        self.frame_head.forget();
    }

    fn forget_overlay(&mut self) {
        let retired = (self.overlay_tables.take(), self.overlay_frame.take(), self.prepared_overlay.take());
        self.retire(retired);
        self.overlay_hour = None;
        self.overlay_shifts.clear();
        self.overlay_placed_view = None;
        (self.overlay_shown, self.overlay_view, self.overlay_hour, self.overlay_drawn_fraction, self.overlay_lookups) = (None, None, None, None, None);
        self.overlay_generation = None;
        self.overlay_clock_unavailable = false;
        self.overlay_layered = None;
        self.overlay_loading.clear();
        self.overlay_missed.clear();
    }

    fn drop_overlay_drawn(&mut self) -> bool {
        if self.overlay.take().is_none() && self.free_overlay.quads.is_empty() {
            return false;
        }
        self.free_overlay = Batch::default();
        self.overlay_written += 1;
        self.overlay_clock_unavailable = false;
        self.overlay_shifts.clear();
        self.counter_report = CounterReport::default();
        self.overlay_hits = Default::default();
        self.hits_stamp += 1;
        true
    }

    fn shown_mode(&self) -> &viewer_map_modes::map_mode::MapMode {
        self.live_map().and_then(|side| side.mode_on_screen()).unwrap_or(&self.map_mode)
    }

    fn take_frame_head(&mut self) -> Option<viewer_gpu::icon_tracks::Head> {
        let clock = self.playhead?;
        let saved = self.options.window_dump || self.capture.as_ref().is_some_and(|capture| !capture.paced);
        let published = self.overlay_shown.map(|(_, hour)| hour);
        let clock = f64::from(clock.hour) + clock.fraction;
        let head = self.frame_head.take(viewer_native_gui::frame_head::Frame {
            clock,
            published,
            speed: self.speed,
            moving: self.playing && self.speed > 0.0 && !saved,
        });
        if viewer_runtime::view_record::on() {
            use viewer_runtime::view_record::{add, set};
            let at = f64::from(head.hour) + f64::from(head.fraction);
            add("head", "frames", 1);
            set("head", "behind_the_clock_milli_hours", ((clock - at) * 1e3) as u64);
            if let Some(published) = published {
                let above = f64::from(published) - at;
                add("head", "past_the_newest_published_hour", u64::from(above < 0.0));
                set("head", "the_newest_published_hour_above_milli", (above.max(0.0) * 1e3) as u64);
            }
        }
        Some(head)
    }

    fn picture_ready(&self) -> bool {
        self.playhead_map.as_ref().is_some_and(|side| side.on_screen().is_some_and(|on| on.drawn.is_some()))
    }

    fn uses_map_seed(&self) -> bool {
        (self.embedded.is_some() || self.dashboard_map)
            && self.capture.is_none()
            && !self.options.window_dump
    }

    fn complete_first_picture(&mut self, playback: &mut viewer_playback::playback::Playback) {
        if self.shown_whole || !self.playhead.is_some_and(|head| self.picture_whole(head, self.playing)) {
            return;
        }
        self.shown_whole = true;
        if self.uses_map_seed() && let Some(frame) = playback.release_bootstrap_frame() {
            self.retire(frame);
        }
    }

    fn presentation_ready(&self, target: Playhead, playing: bool) -> bool {
        self.rendered.is_some_and(|rendered| rendered.branch == target.branch)
            && self.picture_whole(target, playing)
    }

    fn picture_whole(&self, target: Playhead, playing: bool) -> bool {
        self.playhead_map.as_ref().is_some_and(|side| {
            side.mode_on_screen() == Some(&self.map_mode)
                && side.on_screen().is_some_and(|on| {
                    if !side.products().overlay_coming() {
                        return layers_show(&on, target.hour, playing);
                    }
                    self.overlay_view.as_ref().is_some_and(|view| view.matches_mode(&self.map_mode))
                        && whole_picture(&on, self.overlay_hour(), target.hour, playing)
                })
        })
    }

    fn load_complete(&self, art_busy: bool, overlay_fresh: bool) -> bool {
        !art_busy && overlay_fresh && self.playhead.is_some_and(|head| self.picture_whole(head, self.playing))
    }

    pub(crate) fn overlay_hour(&self) -> Option<replay_data::Hour> {
        self.overlay_hour.filter(|_| self.overlay_missed.is_empty())
    }

    fn live_map(&self) -> Option<&viewer_playback::side::SideThread> {
        self.playhead_map.as_ref().filter(|side| side.failure().is_none())
    }

    fn hour_on_screen(&self, document: &Document) -> (replay_data::BranchId, i32) {
        self.layers_on_screen().unwrap_or_else(|| {
            self.playhead.map_or_else(|| (document.playback.branch(), document.playback.hour()), |clock| (clock.branch, clock.hour))
        })
    }

    fn layers_on_screen(&self) -> Option<(replay_data::BranchId, i32)> {
        let side = self.playhead_map.as_ref()?;
        Some((side.branch().or(self.playhead.map(|clock| clock.branch))?, side.on_screen()?.drawn?))
    }

    fn follow_branch(&mut self) {
        let Some(clock) = self.playhead else {
            return;
        };
        if self.playhead_map.as_ref().and_then(|side| side.branch()).is_some_and(|branch| branch != clock.branch) {
            self.close_playhead();
        }
    }

    fn follow_content(&mut self, content: *const crate::document::Content) {
        if self.playhead_map.as_ref().is_some_and(|side| !side.made_over(content)) {
            self.close_playhead();
        }
    }

    fn map_error_of(&mut self, document: &Document) -> Option<String> {
        let Some(side) = &self.playhead_map else {
            return self.playhead_refused.clone();
        };
        if let Some(failure) = side.failure() {
            return Some(failure.to_owned());
        }
        let clock = self.playhead?;
        let replay = document.playback.replay();
        replay.block_at(clock.branch, clock.hour).is_none().then(|| viewer_playback::map_source::gap_reason(replay, clock.branch, clock.hour))
    }

    fn follow_continuity(&mut self, gaps: &[replay_frame::branches::Gap]) -> Option<(Continuity, viewer_runtime::presentation::UnitRestart)> {
        let side = self.live_map()?;
        let on = side.on_screen()?;
        let hour = on.drawn.filter(|_| !on.composing)?;
        let branch = side.branch().or(self.playhead.map(|clock| clock.branch))?;
        let viewing = side.products().ours.map(|slot| slot.to_string());
        let sought = self.presented.reopened || self.presented.seek.is_some_and(|asked| on.resets > asked);
        self.presented.reopened = false;
        if sought {
            self.presented.seek = None;
        }
        self.present_continuity((branch, hour, viewing), sought, gaps)
    }

    fn present_continuity(&mut self, next: (replay_data::BranchId, i32, Option<String>), sought: bool, gaps: &[replay_frame::branches::Gap]) -> Option<(Continuity, viewer_runtime::presentation::UnitRestart)> {
        use viewer_runtime::presentation::{UnitRestart, continuity, unit_restart};
        if !sought && self.presented.last.as_ref() == Some(&next) {
            return None;
        }
        let previous = self.presented.last.as_ref().map(|(branch, hour, viewing)| (*branch, *hour, viewing.as_deref()));
        let shown = (next.0, next.1, next.2.as_deref());
        let (continuity, restart) = (continuity(previous, shown, sought, gaps), unit_restart(previous, shown, sought));
        if continuity == Continuity::Reset {
            self.day_night = Default::default();
            let epoch = self
                .counter_view
                .presentation
                .movement()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
            let current_generation = live_of(&self.playhead_map)
                .and_then(|side| side.products().overlay_generation);
            if let (Some(generation), Some(prepared), Some(rows), Some(tables)) = (
                self.overlay_generation,
                self.prepared_overlay.as_mut(),
                self.overlay_frame.as_ref(),
                self.overlay_tables.as_ref(),
            ) && overlay_matches_continuity(
                (generation, rows.branch, rows.hour, tables.ours()),
                (next.0, next.1, next.2.as_deref()),
                current_generation,
                gaps,
            ) {
                prepared.set_motion_continuity(epoch);
                if !self.overlay_unpainted {
                    self.overlay_unpainted = true;
                    self.overlay_fraction = None;
                }
            }
        }
        if continuity == Continuity::Reset || restart != UnitRestart::Keep {
            self.audio_reset = true;
        }
        self.presented.last = Some(next);
        Some((continuity, restart))
    }

    pub(super) fn reported_error(&self) -> Option<&str> {
        self.error
            .as_deref()
            .or(self.scene_error.as_deref())
            .or(self.map_error.as_deref())
    }

    pub(crate) fn map_state(
        &self,
        target: &dashboard_core::protocol::DashboardMapView,
    ) -> dashboard_core::protocol::DashboardMapState {
        let error = self.reported_error().map(str::to_owned);
        let clock = target.branch_id.parse().ok().map(|branch| {
            let requested = Playhead {
                branch,
                hour: target.hour,
                fraction: f64::from(target.fraction),
            };
            if target.playing {
                self.playhead.unwrap_or(requested)
            } else {
                requested
            }
        });
        let ready = clock.is_some_and(|clock| self.presentation_ready(clock, target.playing))
            && self.rendered_sequence == target.sequence;
        let layers = self.playhead_map.as_ref().and_then(|side| side.on_screen());
        let lag = |hour: i32| {
            clock
                .zip(self.rendered)
                .filter(|(clock, rendered)| target.playing && clock.branch == rendered.branch)
                .and_then(|(clock, _)| u32::try_from(i64::from(clock.hour) - i64::from(hour)).ok())
        };
        let behind = layers.and_then(|on| on.drawn).and_then(lag);
        let behind_reason = behind.filter(|behind| *behind > 0).and(layers.as_ref().map(layers_behind_reason));
        let (hour, fraction) = match (self.rendered, layers.and_then(|on| on.drawn)) {
            (Some(_), Some(drawn)) => (Some(drawn), Some(0.0)),
            _ => (None, None),
        };
        dashboard_core::protocol::DashboardMapState {
            id: target.id.clone(),
            clock_id: target.clock_id.clone(),
            source_id: Some(target.source_id.clone()),
            revision: Some(target.revision.clone()),
            branch_id: self.rendered.map(|shown| shown.branch.to_string()),
            hour,
            fraction,
            sequence: self.rendered_sequence,
            requested_sequence: target.sequence,
            loading: error.is_none() && !ready,
            behind,
            behind_reason,
            error,
            map_mode: self.rendered.map(|_| self.shown_mode().token()),
            drawn_at: self.rendered.and(self.drawn_at),
            hover: self.rendered.and(self.hover.clone()),
            tooltip: self.rendered.and(self.tooltip.clone()),
            tooltip_delay_ms: self.rendered.and(self.tooltip_delay_ms),
            zoom: self.rendered.and(self.last_view.as_ref().map(|view| view.camera.height)),
            center: self.rendered.and(self.last_view.as_ref().map(|view| view.camera.center)),
            options: self.rendered.and(self.drawn_options.clone()),
            applied: self.rendered.and_then(|_| self.applied_settings()),
            notes: self.picker.find.missed().into_iter().collect(),
            close_requests: None,
            keys: Vec::new(),
        }
    }

    fn applied_settings(&self) -> Option<dashboard_core::protocol::AppliedMapSettings> {
        let document = self.document.as_ref()?;
        let mut graphics = self.graphics;
        graphics.texture_quality = document.texture_quality;
        if let Some(scene) = &self.scene {
            graphics.samples = scene.samples();
        }
        Some(dashboard_core::protocol::AppliedMapSettings {
            graphics,
            audio: self.volumes,
            counters: self.counter_filter.clone(),
            visual_mods: document.visual_mods.iter().flatten().map(|choice| choice.id.clone()).collect(),
        })
    }

    fn hour_lines(&self) -> Option<[Vec<String>; 2]> {
        let side = self.playhead_map.as_ref().filter(|side| side.on_screen().is_some())?;
        let lines = side.products().lines.as_ref()?;
        Some([lines.limitations.clone(), lines.political.iter().chain(&lines.reports).cloned().collect()])
    }

    pub(crate) fn vision_refusal_line(&self) -> Option<String> {
        self.live_map().filter(|side| side.on_screen().is_some()).and_then(|side| side.products().vision_refused.clone()).map(|why| format!("{}Viewing country: the vision layer refuses the hour: {why}", viewer_playback::limitations::NOT_GIVEN))
    }

    fn collect_warnings(&self, document: &Document) -> Vec<String> {
        let refused = self.vision_refusal_line();
        let Some([limitations, reports]) = self.hour_lines() else {
            return refused.into_iter().collect();
        };
        let mut warnings: Vec<_> = refused.into_iter().chain(tooltip_warning_lines(&self.tooltip_refused)).collect();
        warnings.extend(self.counter_report.combat_gap_lines());
        warnings.extend(self.combat.warnings(&document.icons.combat.warnings));
        warnings.extend(document.icons.combat.gui.window_warnings());
        warnings.extend(document.icons.counters.gui.window_warnings());
        warnings.extend(
            self.picker_error
                .iter()
                .map(|error| format!("Map mode picker: {error}")),
        );
        warnings.extend(
            self.picker_refused
                .iter()
                .map(|(reason, count)| format!("Map mode picker: {reason} ({count} refused)")),
        );
        warnings.extend(self.air_orders.warnings());
        warnings.extend(self.map_window_warnings());
        warnings.extend(limitations);
        if let Some((device, _)) = &self.gpu {
            warnings.extend(device_requirements(device.features()));
        }
        warnings.extend(document.warnings.iter().cloned());
        warnings.extend(
            document
                .content
                .cursors
                .refusals()
                .map(|why| format!("Cursor: not shown: {why}")),
        );
        warnings.extend(self.selection_input.warnings());
        warnings.extend(
            [Unit::Division(0), Unit::RailwayGun(0), Unit::TaskForce(0)]
                .into_iter()
                .filter_map(|unit| document.icons.counters.plate_sounds(unit).err())
                .map(|why| format!("Counters: their button sounds are not played: {why}")),
        );
        if self.graphics.day_night
            && let Err(error) = &document.game_speeds
        {
            warnings.push(format!(
                "Day/night cycle: {error}, so it shows only while paused"
            ));
        }
        warnings.extend(reports);
        if self.camera_mode == CameraMode::Walk {
            warnings.extend(self.walk.warning());
        }
        if let Some(placements) = &self.placements {
            warnings.extend(placements.warnings.iter().cloned());
        }
        warnings.extend(self.label_report.warnings());
        if let Some(overlay) = &self.prepared_overlay {
            warnings.extend(overlay.raid_report().warnings());
            warnings.extend(overlay.intel_report().warnings());
            warnings.extend(overlay.mode_icon_warnings());
            warnings.extend(overlay.air_orders.warnings());
            warnings.extend(
                overlay
                    .naval_selection
                    .refused
                    .iter()
                    .map(|(why, count)| format!("Navy selection: {count} withheld: {why}")),
            );
            warnings.extend(overlay.stripe_warnings());
        }
        warnings.extend(self.counter_report.warnings());
        for (reason, count) in &self.convoy_refused {
            warnings.push(format!("Trade-route convoys: {count} not drawn: {reason}."));
        }
        if let Some(scene) = &self.scene {
            warnings.extend(scene.pass_warnings());
            warnings.extend(
                scene
                    .shader_refusals()
                    .iter()
                    .map(|(reason, count)| format!("GUI shader: {count} refused draws: {reason}")),
            );
        }
        if let Some(history) = self.layer_history() {
            warnings.extend(history.byte_check().lines());
        }
        let pinned = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.pinned.is_some());
        if self.ui_scale == Setting::MatchGame
            && !pinned
            && let Some((_, (_, Some(note)))) = &self.match_game
        {
            warnings.push(note.clone());
        }
        if self.camera_mode == CameraMode::Fly {
            warnings.extend(self.fly_refusals(document));
        }
        document.uncovered_reads.append_to(&mut warnings);
        warnings
    }

    pub(crate) fn diagnostics_ui(&mut self, ui: &mut egui::Ui, max_height: f32) {
        let side = self.playhead_map.as_ref().filter(|side| side.on_screen().is_some());
        let stats = side.map(|side| {
            side.products().snow.as_deref().map(|summary| vec![format!("Snow and mud: {}", summary.line())]).unwrap_or_default()
        });
        if let Some(stats) = stats {
            for line in stats {
                ui.weak(line);
            }
            if let Some(target) = self.highlighted() {
                ui.weak(format!("Highlighted: {target}"));
            }
            if let Some(target) = self.shown_tooltip() {
                ui.weak(format!("Tooltip: {target}"));
            }
        }
        if self.paused_motion() == PausedMotion::Held {
            ui.weak("Ambient animation: held while paused");
        }
        if let Some(limit) = viewer_runtime::frame_limit::limit() {
            ui.weak(format!("Frame rate limit: {limit:.0} a second"));
        }
        for line in crate::screen_count::readout::lines() {
            ui.weak(line);
        }
        if self.map_warnings.is_empty() {
            return;
        }
        if ui
            .small_button(format!("{} data limitations", self.map_warnings.len()))
            .clicked()
        {
            self.diagnostics_open = !self.diagnostics_open;
        }
        if self.diagnostics_open {
            viewer_runtime::diagnostics_panel::warning_list(ui, &self.map_warnings, max_height);
        }
    }

    fn toggle_diagnostics(&self) {
        if let Some(embedded) = &self.embedded {
            embedded.toggle_diagnostics();
        }
    }

    fn open(
        &mut self,
        mut path: PathBuf,
        ctx: &egui::Context,
        expected_revision: Option<String>,
        open_id: Option<u64>,
    ) {
        let _stage = viewer_runtime::load_record::stage("open.ui");
        self.export_closed("its map was replaced before its picture was saved");
        if self.embedded.is_some() {
            match path.canonicalize() {
                Ok(canonical) => path = canonical,
                Err(error) => {
                    self.error = Some(format!("{}: {error}", path.display()));
                    return;
                }
            }
            if self.loading.is_some() {
                self.embedded_open = Some(crate::embedded::OpenRequest {
                    path,
                    revision: expected_revision,
                    open_id,
                });
                self.embedded_seek = None;
                return;
            }
            self.embedded_open = None;
            if self
                .embedded_seek
                .as_ref()
                .is_some_and(|pending| !pending.matches(&path, expected_revision.as_deref()))
            {
                self.embedded_seek = None;
            }
            self.options.replay = Some(path.clone());
            self.expected_revision = expected_revision.clone();
            self.expected_open_id = open_id;
        }
        let resolve = match self.config.resolve_options() {
            Ok(resolve) => resolve,
            Err(error) => {
                self.failed_load(error.to_string(), ctx);
                self.awaiting_folders = true;
                return;
            }
        };
        if self.gpu.is_none() {
            self.failed_load("WGPU renderer is unavailable".to_owned(), ctx);
            return;
        }
        let held_clock = self
            .held_view
            .take()
            .filter(|(held, _)| *held == path)
            .map(|(_, clock)| clock);
        let held = held_clock.is_some();
        self.options.replay = Some(path.clone());
        let options = self.options.clone();
        let service = self.service();
        let map_seed = self.uses_map_seed();
        let verify_revision = service.is_some();
        let config = self.config.clone();
        let mut content = self
            .dashboard
            .as_ref()
            .map(|dashboard| dashboard.content_snapshot(&config))
            .unwrap_or_default();
        if let (Some(source), Some(document), Some(loaded)) =
            (&self.source, &self.document, &self.loaded_config)
            && loaded.game_dir == config.game_dir
            && loaded.user_dir == config.user_dir
        {
            content.push((source.key().clone(), Arc::clone(&document.content)));
        }
        if let Some(stale) = self.stale_visual_revision.take() {
            content.retain(|(_, content)| content.visual_revision != stale);
        }
        let fallbacks = Arc::clone(&self.visual_fallbacks);
        let (sender, receiver) = mpsc::channel();
        let wake = ctx.clone();
        let startup_content = self.startup_content.take();
        let video_preload = self.video_preload.take();
        let spawned = std::thread::Builder::new()
            .name("native map content".into())
            .spawn_carried(move || {
                viewer_runtime::worker_priority::background_priority();
                let _stage = viewer_runtime::load_record::stage("load");
                let _generation = startup_content.as_ref().map_or_else(
                    content_reader_layers::content::Generation::enter,
                    content_reader_layers::content::Loading::enter_generation,
                );
                drop(startup_content);
                let initial = InitialHour {
                    map_seed,
                    branch: options.branch,
                    hour: options.hour,
                    hour_fraction: options.hour_fraction,
                };
                let load = std::panic::AssertUnwindSafe(|| -> Result<LoadedReplay, String> {
                    let revision = if verify_revision {
                        let _stage = viewer_runtime::load_record::stage("load.replay_revision");
                        let revision = dashboard_core::protocol::replay_revision(&path)
                            .map_err(|error| error.to_string())?;
                        if expected_revision
                            .as_ref()
                            .is_some_and(|expected| *expected != revision)
                        {
                            return Err("Recording changed since it was selected; reopen it in Analytics before viewing this snapshot".into());
                        }
                        Some(revision)
                    } else {
                        None
                    };
                    let source = {
                        let _stage = viewer_runtime::load_record::stage("load.open_map");
                        service
                            .as_ref()
                            .map(|service| service.open_map(&path, revision.as_deref()))
                            .transpose()?
                    };
                    let preferences = source
                        .as_ref()
                        .and_then(|source| {
                            service
                                .as_ref()
                                .map(|service| service.map_settings(&source.replay()))
                        })
                        .or_else(|| {
                            video_preload
                                .as_ref()
                                .and_then(|(_, _, settings)| settings.clone())
                        })
                        .unwrap_or_default();
                    let recording = source.as_ref().map(|source| {
                        crate::visual_fallback::recording(
                            &source.key().source_id,
                            &source.key().revision,
                        )
                    });
                    let decisions = recording.as_ref().map(|recording| {
                        crate::visual_fallback::lock(&fallbacks).decisions(recording)
                    });
                    let mut document = match &source {
                        Some(source) => match content.iter().find(|(key, content)| {
                            key == source.key()
                                && content.fallback.as_ref().is_none_or(|built| {
                                    crate::visual_fallback::lock(&fallbacks).current(built)
                                })
                                && content.visual_mods == preferences.visual_mods
                                && content.texture_quality == preferences.graphics.texture_quality
                        }) {
                            Some((_, content)) => {
                                let _stage = viewer_runtime::load_record::stage("load.shared_content");
                                viewer_runtime::load_record::count("content reused", 1, 0);
                                Document::from_shared_content(
                                    Arc::clone(content),
                                    source.replay(),
                                    &initial,
                                )
                            }
                            None => Document::load_shared(
                                &path,
                                &resolve,
                                options.allow_unverified,
                                &initial,
                                source.replay(),
                                source,
                                preferences.visual_mods.as_deref(),
                                preferences.graphics.texture_quality,
                                recording.as_deref().zip(decisions.as_ref()),
                            ),
                        },
                        None => match &video_preload {
                            Some((pre_replay, pre_rev, pre_settings)) => {
                                let mods = match pre_settings {
                                    Some(resolved) => resolved.visual_mods.as_deref(),
                                    None => (!options.visual_mods.is_empty())
                                        .then_some(options.visual_mods.as_slice()),
                                };
                                Document::load_replay(
                                    &path,
                                    Arc::clone(pre_replay),
                                    pre_rev.clone(),
                                    &resolve,
                                    options.allow_unverified,
                                    &initial,
                                    mods,
                                    preferences.graphics.texture_quality,
                                )
                            }
                            None => Document::load(
                                &path,
                                &resolve,
                                options.allow_unverified,
                                &initial,
                                (!options.visual_mods.is_empty())
                                    .then_some(options.visual_mods.as_slice()),
                                preferences.graphics.texture_quality,
                            ),
                        },
                    }
                    .map_err(|error| error.to_string())?;
                    if let Some(source) = &source {
                        document.bind_source(source);
                    }
                    Ok(LoadedReplay {
                        document,
                        revision,
                        source,
                        config,
                    })
                });
                let result = std::panic::catch_unwind(load).unwrap_or_else(|stopped| {
                    Err(stopped
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| stopped.downcast_ref::<&str>().map(|said| (*said).to_owned()))
                        .unwrap_or_else(|| "Replay loader stopped before returning a result".into()))
                });
                let _ = sender.send(result);
                viewer_runtime::worker_priority::at_least(Some(0), || wake.request_repaint());
            });
        match spawned {
            Ok(_) => self.loading = Some(receiver),
            Err(error) => {
                self.failed_load(error.to_string(), ctx);
                return;
            }
        }
        self.opened = Some(Instant::now());
        self.combat.close();
        drop(self.replace_document(None));
        self.installed_open_id = None;
        self.diplomatic_selected = None;
        self.ground_selection = None;
        self.hover_rows = Default::default();
        self.combat_rows = Default::default();
        self.tooltip_refused.clear();
        self.source = None;
        self.loaded_config = None;
        self.rendered = None;
        self.revision = None;
        if !self.dashboard_map
            && let Some(scene) = &mut self.scene
        {
            crate::perf_log::gpu_times(scene.retire_gpu_frame_times());
        }
        if let Some(scene) = self.scene.take() {
            self.retire(scene);
        }
        self.cancel_scene_build();
        self.scene_error = None;
        self.close_playhead();
        self.playhead = None;
        self.seek_owed = false;
        self.last_view = None;
        self.profile_frame = 0;
        self.scene_target.generation = 0;
        if self.embedded.is_none() {
            self.error = None;
        }
        self.map_error = None;
        self.playing = false;
        self.held_clock = held_clock;
        if held {
            self.walk.forget_figure();
        } else {
            self.camera = self.options.camera;
            self.camera_manual = self.options.camera.is_some();
            self.zoom.reset();
            self.orbit.reset();
            self.walk.reset();
            self.flight = None;
            self.free_cam = None;
            self.last_fly_eye = None;
            self.mode_handover = None;
            self.hud.leave();
            if self.start_pick.at().is_some() {
                self.start_pick = drop::Pick::Choosing;
                self.input.release();
            }
            self.fly_clock = None;
            self.fly_footprints = Default::default();
        }
        self.placements = None;
        self.fade = None;
        self.overlay = None;
        self.overlay_shifts.clear();
        self.overlay_written += 1;
        self.counter_report = CounterReport::default();
        self.combat_painted = Default::default();
        self.overlay_hits = Default::default();
        self.hits_stamp += 1;
        self.counter_held = None;
        self.unit_selection = Default::default();
        self.air_selection = Default::default();
        self.fleet_selection = Default::default();
        self.overlay_fraction = None;
        self.overlay_placed = None;
        self.forget_overlay_builds();
        if let Some(dropper) = &self.dropper {
            dropper.drop_later(std::mem::take(&mut self.faction_highlight));
        }
    }

    fn lap(&mut self, name: &'static str) {
        if let Some(laps) = &mut self.laps {
            laps.lap(name);
        }
    }

    fn failed_load(&mut self, error: String, ctx: &egui::Context) {
        self.refuse_capture(&format!("replay could not be loaded: {error}"), ctx);
        self.error = Some(error);
    }

    fn hour_error(&mut self, error: Option<String>, ctx: &egui::Context) {
        if let Some(error) = &error {
            self.refuse_capture(error, ctx);
        }
        self.map_error = error;
    }

    fn refuse_capture(&mut self, error: &str, ctx: &egui::Context) -> bool {
        let dump = self.refused(error).unwrap_or(false);
        if dump {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        dump
    }

    fn refused_frame(&mut self, why: String, ctx: &egui::Context) -> Result<(), String> {
        if self.refuse_capture(&why, ctx) { Err(why) } else { Ok(()) }
    }

    pub(crate) fn export_closed(&mut self, why: &str) {
        if self.capture.as_ref().is_some_and(|capture| !capture.exit) {
            self.refused(why);
        }
    }

    fn refused(&mut self, error: &str) -> Option<bool> {
        let mut capture = self.end_capture()?;
        let message = format!("Capture {} failed: {error}", capture.path.display());
        if let Some(mut session) = capture.video.take() {
            session.finish_render_failure(error.to_owned());
        } else if capture.exit {
            *self
                .failure
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(message.clone());
        }
        Self::answer_export(&mut capture, Err(message.clone()));
        self.capture_note = Some(message);
        Some(capture.exit)
    }

    fn answer_export(capture: &mut Capture, answer: crate::host::Answer) {
        if let Some(job) = capture.export.take() {
            job.answer(answer);
        }
    }

    #[cfg(test)]
    pub(crate) fn arm_export(&mut self, path: &str) -> crate::host::tests::Answers {
        let (job, answers) = crate::host::tests::job(path);
        self.capture = Some(Capture { path: path.into(), remaining: 0, pinned: None, exit: false, time: None, step: 0.0, frames: 1, index: 0, paced: false, export: Some(job), video: None });
        answers
    }

    fn end_capture(&mut self) -> Option<Capture> {
        (self.overlay_art_retries, self.overlay_art_moved, self.held_since) = (0, false, None);
        self.capture.take()
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let result = self.loading.as_ref().map(|receiver| receiver.try_recv());
        if (self.embedded_open.is_some() || self.options.replay.is_none())
            && result
                .as_ref()
                .is_some_and(|result| !matches!(result, Err(mpsc::TryRecvError::Empty)))
        {
            self.loading = None;
            if let Some(Ok(Ok(loaded))) = result {
                self.retire(loaded);
            }
            return;
        }
        match result {
            Some(Ok(Ok(LoadedReplay {
                document,
                revision,
                source,
                config,
            }))) => {
                let _stage = viewer_runtime::load_record::stage("install");
                self.revision = revision;
                self.source = source;
                self.loaded_config = Some(config);
                if self.embedded.is_some() {
                    self.error = None;
                }
                self.combat_rows.pending = self.options.combat;
                self.combat_rows.selector_refused = None;
                self.combat_rows.selector_waits = 0;
                self.playhead = Some(Playhead {
                    branch: document.playback.branch(),
                    hour: document.playback.hour(),
                    fraction: f64::from(document.hour_fraction),
                });
                self.clock_anchor = None;
                if let Some(held) = self.held_clock.take()
                    && let Some(head) = held.playhead
                    && let Some((_, last)) = document.playback.range(head.branch)
                {
                    let now = Instant::now();
                    let mut head = head;
                    self.playing = held.playing
                        && tick_clock(
                            &mut head,
                            self.speed,
                            now.duration_since(held.at).as_secs_f64(),
                            last,
                        );
                    self.playhead = Some(head);
                    self.clock_anchor = held.anchor;
                    self.last_step = now;
                }
                self.take_game_options(&document.game_options);
                if self.embedded.is_none() && !self.host_graphics {
                    self.graphics.full_resolution = document.game_full_resolution;
                    self.volumes = document.game_audio;
                }
                self.shown_whole = false;
                if let Some((source_viewport, crop)) = self.video_crop.take() {
                    let map_size = MapSize::of(&document.map);
                    let base_vp = source_viewport.unwrap_or(self.options.size);
                    let base = self
                        .camera
                        .unwrap_or_else(|| Camera::fit(map_size, document.map_defines, base_vp).placement());
                    let cropped = crate::video_export::apply_crop_to_placement(
                        base,
                        map_size,
                        document.map_defines,
                        source_viewport,
                        crop,
                    );
                    self.camera = Some(cropped);
                    self.camera_manual = true;
                    if let Some(capture) = self.capture.as_mut()
                        && let Some((_, pinned_cam)) = capture.pinned.as_mut()
                    {
                        *pinned_cam = Some(cropped);
                    }
                }
                self.close_playhead();
                self.scene_error = None;
                drop(self.replace_document(Some(document)));
                self.installed_open_id = self.expected_open_id;
                self.uploaded = Uploaded::default();
                self.loading = None;
                if let (Some(service), Some(source)) = (self.service(), &self.source) {
                    let settings = service.map_settings(&source.replay());
                    self.set_embedded_graphics(&settings);
                }
            }
            Some(Ok(Err(error))) => {
                self.loading = None;
                self.failed_load(error, ctx);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.loading = None;
                self.failed_load(
                    "Replay loader stopped before returning a result".into(),
                    ctx,
                );
            }
            _ => {}
        }
    }

    fn watch_monitors(
        monitor_seen: &mut Option<(Option<egui::Vec2>, Option<f32>)>,
        ctx: &egui::Context,
    ) {
        let seen = ctx.input(|input| {
            let viewport = input.viewport();
            (viewport.monitor_size, viewport.native_pixels_per_point)
        });
        if *monitor_seen != Some(seen) {
            *monitor_seen = Some(seen);
            let ctx = ctx.clone();
            refresh_monitors(viewer_runtime::worker_priority::waking(move || ctx.request_repaint()));
        }
    }

    fn match_game<'a>(
        match_game: &'a mut Option<MatchGame>,
        document: &Document,
    ) -> &'a (Raster, Option<String>) {
        let key = (
            monitors_generation(),
            Arc::as_ptr(&document.content) as usize,
        );
        if match_game.as_ref().is_none_or(|(cached, _)| *cached != key) {
            let monitors = ui_scale_monitors();
            let resolved = Setting::MatchGame.resolve_raster(
                document.game_display.as_ref().map_err(String::as_str),
                monitors
                    .as_ref()
                    .as_ref()
                    .map(Vec::as_slice)
                    .map_err(String::as_str),
            );
            *match_game = Some((key, resolved));
        }
        &match_game.as_ref().expect("resolved above").1
    }

    fn save_capture(
        &mut self,
        state: &eframe::egui_wgpu::RenderState,
        document: &Document,
        camera: Camera,
        pixels: [u32; 2],
        warnings: &[String],
        shown: (replay_data::BranchId, replay_data::Hour),
        ctx: &egui::Context,
    ) {
        match &mut self.capture {
            Some(capture) if capture.remaining > 0 => {
                capture.remaining -= 1;
                ctx.request_repaint();
                return;
            }
            Some(_) => {}
            None => return,
        }
        let mut capture = self.end_capture().expect("matched above");
        if let Some(mut session) = capture.video.take() {
            let scene = self.scene.as_ref().expect("a frame was drawn");
            let unverified = document
                .warnings
                .iter()
                .filter(|warning| warning.starts_with("UNVERIFIED CONTENT"))
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            let unverified_ref = (!unverified.is_empty()).then_some(unverified.as_str());
            session.ensure_mixer(document);
            match session.drain_non_recorded(&state.device, &state.queue) {
                Ok(true) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    session.finish_render_failure(error);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
            let audio_frame = if session.audio_enabled {
                build_audio_frame(
                    document,
                    scene,
                    &camera,
                    AudioInputs {
                        volumes: self.volumes,
                        muted: false,
                        reset: std::mem::take(&mut self.audio_reset),
                        music: self
                            .playhead_map
                            .as_ref()
                            .and_then(|side| side.products().music.as_deref().cloned())
                            .or_else(|| self.music.clone()),
                        battles: self.battles.clone(),
                        interface: std::mem::take(&mut self.interface_sounds),
                        interface_key: &mut self.interface_key,
                    },
                )
            } else {
                None
            };
            let done = match session.consume_recorded(
                &state.device,
                &state.queue,
                scene,
                document,
                &camera,
                audio_frame,
                unverified_ref,
            ) {
                Ok(done) => done,
                Err(error) => {
                    session.finish_render_failure(error);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            };
            if done {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if let Some(next) = session.current().cloned()
                && let video_export::schedule::FrameKind::Recorded {
                    hour,
                    fraction,
                    reset_continuity,
                    ..
                } = next.kind
            {
                let branch = self
                    .playhead
                    .map_or_else(|| document.playback.branch(), |head| head.branch);
                self.playhead = Some(Playhead {
                    branch,
                    hour,
                    fraction,
                });
                if reset_continuity {
                    self.audio_reset = true;
                    self.presented.last = None;
                }
                capture.time = Some(next.presentation_time as f32);
                capture.remaining = 0;
                capture.video = Some(session);
                self.capture = Some(capture);
                ctx.request_repaint();
            } else {
                let _ = session.finish_complete();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let output = if capture.frames > 1 || capture.index > 0 {
            let stem = capture
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            capture
                .path
                .with_file_name(format!("{stem}-{:04}.png", capture.index))
        } else {
            capture.path.clone()
        };
        let scene = self.scene.as_ref().expect("a frame was drawn");
        let unverified = document
            .warnings
            .iter()
            .filter(|warning| warning.starts_with("UNVERIFIED CONTENT"))
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let written = (|| -> std::io::Result<Option<String>> {
            if !unverified.is_empty() {
                render::render_banner(scene, &state.device, &state.queue, &unverified)?;
            }
            match &capture.export {
                Some(job) => render::write_png_under(&job.receipt, scene, &state.device, &state.queue, &output),
                None => render::write_png(scene, &state.device, &state.queue, &output).map(|()| None),
            }
        })();
        if written.is_ok() {
            for warning in warnings {
                eprintln!("Warning: {warning}");
            }
            println!(
                "Captured branch {}, hour {}{} to {} at {}x{}",
                shown.0,
                shown.1,
                if document.hour_fraction > 0.0 {
                    format!(" + {:.3} h", document.hour_fraction)
                } else {
                    String::new()
                },
                output.display(),
                pixels[0],
                pixels[1]
            );
            println!(
                "Camera: {},{},{}",
                camera.center[0], camera.center[1], camera.height
            );
            if let Some(line) = self.flight_line(document) {
                println!("{line}");
            }
            if let Some(time) = capture.time {
                println!("Animation time: {time:.6} s");
            }
            if self.camera_mode == CameraMode::Walk
                && let Some(walk) = self.walk.report()
            {
                println!("{walk}; player draws {}", scene.player_drawn());
            }
            println!("Colour grading {}", if self.postfx { "on" } else { "off" });
        }
        self.capture_written(capture, &output, shown, written.map_err(|error| error.to_string()), ctx);
    }

    fn capture_written(&mut self, mut capture: Capture, output: &Path, shown: (replay_data::BranchId, replay_data::Hour), written: Result<Option<String>, String>, ctx: &egui::Context) {
        let answer = match written {
            Ok(warning) => Ok(crate::host::Saved { message: format!("Saved {}: branch {}, hour {}", output.display(), shown.0, shown.1), warning }),
            Err(error) => {
                let message = format!("{}: {error}", capture.path.display());
                if capture.exit
                    && let Ok(mut failure) = self.failure.lock()
                {
                    *failure = Some(message.clone());
                }
                eprintln!("{message}");
                Err(message)
            }
        };
        self.capture_note = Some(answer.clone().map_or_else(|why| why, |saved| match saved.warning {
            Some(warning) => format!("{}, but {warning}", saved.message),
            None => saved.message,
        }));
        if capture.frames <= 1 {
            Self::answer_export(&mut capture, answer);
        }
        if capture.frames > 1 {
            capture.frames -= 1;
            capture.index += 1;
            capture.time = capture.time.map(|t| t + capture.step);
            capture.remaining = 0;
            self.capture = Some(capture);
            ctx.request_repaint();
        } else if capture.exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn hit_shifts(&self) -> Option<&viewer_native_gui::anchored::Shifts> {
        (!self.overlay_shifts.is_empty()).then_some(&self.overlay_shifts)
    }

    fn hit_shapes(&self) -> map_hits::Shapes<'_> {
        map_hits::Shapes {
            report: &self.counter_report,
            raids: &self.overlay_hits.raids,
            occupation: &self.overlay_hits.occupation,
            adjacency: &self.overlay_hits.adjacency,
            looks: self.looks.as_ref(),
            shifts: self.hit_shifts(),
        }
    }

    fn map_hit(&self, point: [f32; 2]) -> Option<MapHit> {
        let shapes = self.hit_shapes();
        self.map_hits.borrow_mut().at(self.hits_stamp, point, &shapes, |point| {
            self.overlay_hits.intel_at(|hit| self.icon_point(viewer_gpu::icon_tracks::IconFamily::Intel, hit.layer, point))
        })
    }

    fn icon_point(&self, family: viewer_gpu::icon_tracks::IconFamily, layer: u64, point: [f32; 2]) -> Option<([f32; 2], u32)> {
        map_hits::local(self.hit_shifts(), self.looks.as_ref(), family, layer, point)
    }

    fn hit_layer(&self, target: MapHit) -> Option<(viewer_gpu::icon_tracks::IconFamily, u64)> {
        map_hits::hit_layer(&self.hit_shapes(), target)
    }

    fn raid_hit(&self, point: [f32; 2]) -> Option<&viewer_raids::map::raids::tooltips::Hit> {
        map_hits::raid_at(&self.hit_shapes(), point)
    }

    fn local_point(
        &self,
        layer: Option<(viewer_gpu::icon_tracks::IconFamily, u64)>,
        point: [f32; 2],
    ) -> [f32; 2] {
        layer
            .and_then(|(family, layer)| self.icon_point(family, layer, point))
            .map_or(point, |(local, _)| local)
    }

    fn shown_rect(&self, layer: Option<(viewer_gpu::icon_tracks::IconFamily, u64)>, rect: [f32; 4]) -> [f32; 4] {
        layer
            .and_then(|(family, layer)| map_hits::shown(self.hit_shifts(), self.looks.as_ref(), family, layer, rect))
            .unwrap_or(rect)
    }

    fn faction_pin_at(
        &self,
        point: [f32; 2],
    ) -> Option<&viewer_counter_kit::PinHover> {
        let MapHit::Pin(index) = self.map_hit(point)? else {
            return None;
        };
        self.counter_report.faction_pin_hovers.get(index)
    }

    fn update_faction_highlight(
        &mut self,
        document: &Document,
        device: &eframe::wgpu::Device,
        queue: &eframe::wgpu::Queue,
        ctx: &egui::Context,
        point: Option<[f32; 2]>,
    ) -> Result<(), String> {
        let pool = self.worker_pool().map_err(|why| format!("Native map workers: {why}"))?;
        let adjacency = self.map_mode == viewer_map_modes::map_mode::MapMode::Stock(viewer_map_modes::map_mode::Stock::StrategicNavy);
        let color = if adjacency { &document.border_assets.adjacency_highlight_color } else { &document.border_assets.faction_highlight_color };
        let color = color.as_ref().ok().copied();
        let outline = self.ground_selection.clone().filter(|_| document.border_assets.selection_colors.is_ok());
        if color.is_none() && outline.is_none() {
            return Ok(());
        }
        let selected = if color.is_none() {
            Arc::default()
        } else if adjacency {
            point.and_then(|point| match self.map_hit(point) {
                Some(MapHit::Adjacency(index)) => self.overlay_hits.adjacency.get(index),
                _ => None,
            }).map(|hit| Arc::clone(&hit.area)).unwrap_or_default()
        } else {
            point.and_then(|point| self.faction_pin_at(point))
                .map(|hit| Arc::clone(&hit.regions)).unwrap_or_default()
        };
        self.highlight_wanted = faction_highlight::wanted(&selected, outline.as_ref(), self.scene.as_ref().is_some_and(|scene| !scene.faction_highlight_regions().is_empty()));
        let Some(side) = self.playhead_map.as_ref() else { return Ok(()) };
        let stale = side.products().highlight_hour.zip(side.plans_shown()).is_some_and(|(picture, plans)| picture < plans);
        let Some((picture, over)) = side.products().highlight.clone().filter(|_| !stale) else {
            if let Some(scene) = self.scene.as_mut()
                && !scene.faction_highlight_regions().is_empty()
                && let Some(restore) = self.faction_highlight.take_restore()
            {
                scene.apply(device, queue, SceneInput::SetFactionHighlight { plan: &restore, strategic_regions: &[], color: color.unwrap_or_default() }).map_err(|why| why.to_string())?;
                if let Some(side) = self.playhead_map.as_mut() {
                    side.reassert(&mut viewer_layer_core::layers::Gpu { device, queue, scene, content: &document.content, counter_gui: &document.icons.counters.gui, whole: false });
                }
            }
            return Ok(());
        };
        let Some((branch, hour)) = self.layers_on_screen() else { return Ok(()) };
        let key = faction_highlight::Key {
            branch,
            hour,
            perspective: self.perspective.clone(),
            borders: picture,
            regions: Arc::clone(&selected),
            adjacency,
            selection: outline.clone(),
        };
        let scene = self.scene.as_mut().expect("scene built");
        if self.faction_highlight.requested() != Some(&key)
            && scene.highlight_installed()
            && let Some(restore) = self.faction_highlight.take_restore()
        {
            scene
                .apply(device, queue, SceneInput::SetFactionHighlight { plan: &restore, strategic_regions: &[], color: color.unwrap_or_default() })
                .map_err(|why| why.to_string())?;
            if let Some(side) = self.playhead_map.as_mut() {
                side.reassert(&mut viewer_layer_core::layers::Gpu { device, queue, scene, content: &document.content, counter_gui: &document.icons.counters.gui, whole: false });
            }
        }
        let content = Arc::clone(&document.content);
        let dropper = pool.dropper();
        let outline = outline.zip(document.border_assets.selection_colors.as_ref().ok().copied()).map(|(outline, [province, state])| {
            (outline.provinces.iter().copied().collect::<std::collections::BTreeSet<u32>>(), if outline.state { state } else { province })
        });
        if let Some(prepared) = self.faction_highlight.update_with(
            key,
            |job| {
                let wake = ctx.clone();
                pool.try_submit(job, move || wake.request_repaint())
            },
            move || {
                let source = over;
                move || {
                    let provinces: std::collections::BTreeSet<u32> = if adjacency {
                        selected.iter().copied().collect()
                    } else { selected
                        .iter()
                        .flat_map(|id| {
                            content
                                .strategic_regions
                                .get(id)
                                .into_iter()
                                .flat_map(|region| region.provinces.iter().copied())
                        })
                        .collect() };
                    let meshes = &content.border_meshes;
                    let outline = outline.as_ref().map(|(provinces, color)| (provinces, *color));
                    let plan = |regions: &map_display_data::landscape::Regions, zone: &viewer_borders::demilitarized::Zone| faction_highlight::Prepared {
                        plan: viewer_world::scene::FactionHighlight::with_selection(meshes, regions, zone, &provinces, outline),
                        restore: Some(viewer_world::scene::FactionHighlight::new(meshes, regions, zone, &Default::default())),
                    };
                    match source {
                        faction_highlight::Over::Layer(picture) => {
                            let (regions, zone) = picture.regions_and_zone(&content.map);
                            plan(&regions, &zone)
                        }
                        faction_highlight::Over::Mode(regions, zone) => plan(&regions, &zone),
                    }
                }
            },
            dropper,
        ) {
            let result = scene.apply(
                device,
                queue,
                SceneInput::SetFactionHighlight {
                    plan: &prepared.plan,
                    strategic_regions: &self
                        .faction_highlight
                        .requested()
                        .expect("requested highlight")
                        .regions,
                    color: color.unwrap_or_default(),
                },
            );
            dropper.drop_later(prepared.plan);
            result.map_err(|why| why.to_string())?;
            self.faction_highlight.set_restore(prepared.restore);
            self.faction_highlight.installed();
        }
        Ok(())
    }

    fn hover_style(&self) -> viewer_counter_products::map::counter_tooltips::Style {
        viewer_counter_products::map::counter_tooltips::Style {
            nato: self.counter_view.settings.nato,
            diagnostics: self.diagnostics,
        }
    }

    fn map_tooltip(
        &mut self,
        document: &Document,
        point: Option<[f32; 2]>,
        extent: [f32; 2],
        ctx: &egui::Context,
    ) -> Option<(Arc<Tooltip>, [f32; 4], Option<u32>)> {
        self.hover_rows.present(self.layers_on_screen());
        let point = point?;
        let target = self.map_hit(point);
        if matches!(target, Some(MapHit::Intel)) {
            let hit = self.overlay_hits.intel_at(|hit| self.icon_point(viewer_gpu::icon_tracks::IconFamily::Intel, hit.layer, point))?;
            let layer = Some((viewer_gpu::icon_tracks::IconFamily::Intel, hit.layer));
            let rect = self.shown_rect(layer, hit.bounds());
            let empty = hit.tooltip.title.is_empty()
                && hit.tooltip.body.is_empty()
                && hit.tooltip.refused.is_empty();
            return (!empty).then(|| (Arc::clone(&hit.tooltip), rect, None));
        }
        let layer = target.and_then(|target| self.hit_layer(target));
        let local = self.local_point(layer, point);
        let views = self.rows_head()?;
        let head = &views;
        let repaint = || {
            let ctx = ctx.clone();
            viewer_runtime::worker_priority::waking(move || ctx.request_repaint())
        };
        if let Some(MapHit::Occupation(index)) = target {
            let hit = self.overlay_hits.occupation.get(index)?;
            let rect = self.shown_rect(layer, hit.bounds()?);
            use viewer_map_icons::occupation_icons::Hover;
            let hover = hit.hover.clone();
            let text = match &hover {
                Hover::Modifier { state, token } => {
                    let result = (|| {
                        let content = document.icons.occupation.as_ref().map_err(Clone::clone)?;
                        content
                            .definitions
                            .find(token)?
                            .ok_or_else(|| format!("active definition {token} is absent"))
                    })();
                    match result {
                        Ok(definition) => {
                            let vision = self.shown_vision().ok()?;
                            let inputs = self.hover_inputs(document, &vision, ctx);
                            let key = inputs.key(hover_rows::What::Occupation { state: *state, token: token.clone() }, [0, 0]);
                            self.hover_rows.ask(hover_rows::Family::Occupation, head, key, |_| inputs.occupation(*state, definition.clone()))?
                        }
                        Err(why) => {
                            let mut text = Tooltip::default();
                            text.refused
                                .insert(format!("occupation badge {token}: {why}"), 1);
                            Arc::new(text)
                        }
                    }
                }
                Hover::Resistance { state } => {
                    let province = document
                        .icons
                        .raids
                        .trigger_content
                        .state_provinces
                        .get(state)
                        .and_then(|members| members.first())
                        .copied();
                    let Some(province) = province else {
                        let mut text = Tooltip::default();
                        text.refused.insert(format!("occupation marker state {state}: no member province supplies the resistance hover"), 1);
                        return Some((Arc::new(text), rect, None));
                    };
                    let vision = self.shown_vision().ok()?;
                    let inputs = self.hover_inputs(document, &vision, ctx);
                    let (mode, selected, rosters) = (viewer_counter_products::map::province_tooltips::ModeHover::OccupationMarker, self.unit_selection.ordered_units(), document.playback.roster_sampler());
                    let key = inputs.key(hover_rows::What::Province { province, mode: mode.clone(), routes: Vec::new() }, [hover_rows::id(selected), hover_rows::id(&rosters)]);
                    self.hover_rows.ask(hover_rows::Family::Province, head, key, |caches| inputs.province(caches, province, mode, Vec::new(), Arc::clone(selected), rosters))?
                }
            };
            return Some((text, rect, None));
        }
        if let Some((text, rect)) = prepared_naval_tooltip(&self.counter_report, target) {
            return Some((text, self.shown_rect(layer, rect), None));
        }
        if let Some(MapHit::Base(index)) = target
            && let Some(hit) = self.counter_report.air_grid_hits.get(&index)
        {
            return Some((
                Arc::clone(&hit.text),
                self.shown_rect(layer, self.counter_report.base_hits[index].rect),
                None,
            ));
        }
        if let Some(MapHit::Base(index)) = target
            && let Some(&(province, kind)) = self.counter_report.air_base_hits.get(&index)
        {
            let vision = self.shown_vision().ok()?;
            let (inputs, wings) = (self.hover_inputs(document, &vision, ctx), self.air_selection.wings());
            let key = inputs.key(hover_rows::What::AirBase { province, kind }, [hover_rows::id(wings), 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |_| inputs.air_base(province, kind, Arc::clone(wings)))?;
            return Some((
                text,
                self.shown_rect(layer, self.counter_report.base_hits[index].rect),
                None,
            ));
        }
        if let Some(MapHit::Base(index)) = target
            && let Some(&province) = self.counter_report.naval_base_hits.get(&index)
        {
            let vision = self.shown_vision().ok()?;
            let inputs = self.hover_inputs(document, &vision, ctx);
            let fleet_selected = !self.fleet_selection.fleets().is_empty()
                || self.unit_selection.units().iter().any(|unit| matches!(unit, viewer_runtime::unit_handle::Unit::TaskForce(_)));
            let key = inputs.key(hover_rows::What::NavalBase { province, fleet_selected }, [0, 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |_| inputs.naval_base(province, fleet_selected))?;
            return Some((text, self.shown_rect(layer, self.counter_report.base_hits[index].rect), None));
        }
        if let Some(MapHit::AirMission(index)) = target {
            let hit = self.counter_report.air_mission_hits.get(index)?;
            return Some((
                Arc::clone(&hit.text),
                self.shown_rect(layer, hit.rect),
                None,
            ));
        }
        if let Some(MapHit::NavalRegionHover(index)) = target {
            use viewer_counter_kit::NavalRegionalControl;
            let (region, control, rect) = {
                let hit = self.counter_report.naval_region_hovers.get(index)?;
                (hit.region, hit.control, hit.rect)
            };
            let text = match control {
                NavalRegionalControl::Mines | NavalRegionalControl::Accidents => {
                    let mines = matches!(control, NavalRegionalControl::Mines);
                    let vision = self.shown_vision().ok()?;
                    let inputs = self.hover_inputs(document, &vision, ctx);
                    let key = inputs.key(if mines { hover_rows::What::Mines(region) } else { hover_rows::What::Accidents(region) }, [0, 0]);
                    self.hover_rows.ask(hover_rows::Family::Naval, head, key, |_| inputs.naval(region, mines))?
                }
                _ => {
                    let mut text = Tooltip::default();
                    text.refuse(
                        "regional Navy control hover",
                        "drawn control has no prepared hover text".into(),
                    );
                    Arc::new(text)
                }
            };
            return Some((text, self.shown_rect(layer, rect), None));
        }
        if matches!(target, Some(MapHit::Raid))
            && let Some(hit) = self.raid_hit(point).cloned()
        {
            let ours = self.shown_vision().ok()?.ours();
            let text = self.combat_rows.raid(head, document, ours, self.counter_view.settings.nato, hit.key.clone(), ctx)?;
            let rect = [hit.origin[0], hit.origin[1], hit.size[0], hit.size[1]];
            let layer = Some((viewer_gpu::icon_tracks::IconFamily::Raids, hit.layer));
            return Some((text, self.shown_rect(layer, rect), None));
        }
        let land_hit = match target {
            Some(MapHit::Combat(index)) => {
                let hit = &self.counter_report.combat_hits[index];
                match hit.selection {
                    Selection::Land(id) => Some((id, hit.rect)),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((id, rect)) = land_hit {
            let viewer = self.shown_vision().ok()?.ours();
            let index = self.layer_engaged();
            let text = self.combat_rows.land_marker(head, document, viewer, id, index, self.counter_view.settings.nato, repaint())?;
            return Some((text, self.shown_rect(layer, rect), None));
        }
        let naval_hit = match target {
            Some(MapHit::Combat(index)) => Some(&self.counter_report.combat_hits[index]),
            Some(MapHit::NavalObserver(index)) => {
                Some(&self.counter_report.naval_marker_hovers[index])
            }
            _ => None,
        }
        .and_then(|hit| match hit.selection {
            Selection::Naval(id) => Some((id, hit.rect)),
            _ => None,
        });
        if let Some((id, rect)) = naval_hit {
            let viewer = self.shown_vision().ok()?.ours();
            let index = self.layer_engaged();
            let text = self.combat_rows.naval_marker(head, document, viewer, id, index, repaint())?;
            return Some((text, self.shown_rect(layer, rect), None));
        }
        if let Some(hit) = self.counter_report.land_hovers.iter().rev().find(|hit| {
            hit.contains(local)
                && matches!(target, Some(MapHit::Unit(index))
                if land_hover_matches_unit(hit, &self.counter_report.unit_hits[index]))
        }).cloned()
        {
            let vision = self.shown_vision().ok()?;
            let inputs = self.hover_inputs(document, &vision, ctx);
            let key = inputs.key(hover_rows::What::Land(hit.clone()), [0, 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |caches| inputs.land(caches, hit.clone()))?;
            let text = if hit.ours
                && !hit.hidden
                && !hit.railway_gun
                && hit.members.len() == 1
                && hit.control
                    == viewer_native_gui::counter_hovers::land::CounterControl::Normal
            {
                self.combat_rows.division_panel(head, document, text, hit.members[0], repaint())
            } else {
                text
            };
            return Some((text, self.shown_rect(layer, hit.rect), None));
        }
        if let Some(hit) = self
            .counter_report
            .task_force_hovers
            .iter()
            .rev()
            .find(|hit| {
                hit.contains(local)
                    && matches!(target, Some(MapHit::Unit(index))
                if index == hit.unit_hit && hit.matches(&self.counter_report.unit_hits[index]))
            })
            .cloned()
        {
            if !hit.supported {
                return None;
            }
            let vision = self.shown_vision().ok()?;
            let inputs = self.hover_inputs(document, &vision, ctx);
            let key = inputs.key(hover_rows::What::TaskForce(hit.clone()), [0, 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |caches| inputs.task_force(caches, hit.clone()))?;
            return Some((text, self.shown_rect(layer, hit.rect), None));
        }
        if let Some(hit) = self.counter_report.task_force_port_hovers.iter().enumerate().rev()
            .find(|(hover_index, hit)| hit.contains(local) && hit.has_tooltip()
                && self.counter_report.unit_hits.get(hit.unit_hit).is_some_and(|unit| hit.matches(unit))
                && (matches!(target, Some(MapHit::Unit(index)) if index == hit.unit_hit)
                    || matches!(target, Some(MapHit::PortTooltip(index)) if index == *hover_index)))
            .map(|(_, hit)| hit.clone())
        {
            if !hit.supported { return None; }
            let vision = self.shown_vision().ok()?;
            let inputs = self.hover_inputs(document, &vision, ctx);
            let key = inputs.key(hover_rows::What::Port(hit.clone()), [0, 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |caches| inputs.port(caches, hit.clone()))?;
            return Some((text, self.shown_rect(layer, hit.rect), None));
        }
        if let Some(MapHit::Pin(index)) = target {
            let (theatre, rect) = {
                let hit = self.counter_report.faction_pin_hovers.get(index)?;
                (hit.theatre, hit.rect)
            };
            let vision = self.shown_vision().ok()?;
            let inputs = self.hover_inputs(document, &vision, ctx);
            let key = inputs.key(hover_rows::What::FactionTheatre { faction: theatre.0, ordinal: theatre.1 }, [0, 0]);
            let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |_| inputs.faction_theatre(theatre.0, theatre.1))?;
            return Some((text, self.shown_rect(layer, rect), None));
        }
        if target.is_some() {
            return None;
        }
        match self.model_at(document, point, false).map(|picked| picked.map(|picked| unit_pick::location(&picked.candidates, &picked.divisions, picked.unordered))) {
            Ok(None) => {}
            Ok(Some(Ok(province))) => {
                let vision = self.shown_vision().ok()?;
                let inputs = self.hover_inputs(document, &vision, ctx);
                let key = inputs.key(hover_rows::What::Location(province), [0, 0]);
                let text = self.hover_rows.ask(hover_rows::Family::Counters, head, key, |_| inputs.location(province))?;
                return Some((text, [0.0, 0.0, extent[0], extent[1]], None));
            }
            Ok(Some(Err(why))) | Err(why) => {
                let mut text = Tooltip::default();
                text.refused.insert(why.to_owned(), 1);
                return Some((Arc::new(text), [0.0, 0.0, extent[0], extent[1]], None));
            }
        }
        let view = self.last_view.as_ref()?;
        let routes = self.open_map().and_then(|side| side.products().routes.as_deref()).map(|(_, routes)| Arc::clone(routes));
        let route_lines: Vec<_> = routes
            .as_ref()
            .filter(|_| self.counter_view.visibility.trade_routes)
            .map(|routes| {
                viewer_gpu::ribbon_content::trade_route_hit(
                    &routes.field.trade_route_hovers,
                    view.camera.eye(),
                    view.camera.ray(point.map(|v| v * view.ui_scale), view.viewport),
                    document.map.width as f32,
                )
                .into_iter()
                .filter_map(|line| routes.field.trade_route_lines.get(line as usize).copied())
                .collect()
            })
            .unwrap_or_default();
        let province = match pick(
            &view.camera,
            point.map(|v| v * view.ui_scale),
            view.viewport,
            &document.map,
            document.assets.heightmap_texels(),
        ) {
            Ok(province) => province?,
            Err(reason) => {
                let mut text = Tooltip::default();
                text.refused.insert(reason, 1);
                return Some((Arc::new(text), [0.0, 0.0, extent[0], extent[1]], None));
            }
        };
        let vision = self.shown_vision().ok()?;
        let mode = self.shown_mode().clone();
        let counters = &document.content.counters;
        let region = picker::picks_region(counters.kinds.get(&province).copied(), counters.region(province));
        let mut mode_hover = picker::mode_hover(&mode, region, document.map_modes.defines.opinion_thresholds(), || self.combat_rows.infrastructure(head, document, province, repaint()));
        if let viewer_counter_products::map::province_tooltips::ModeHover::Terrain { region } = mode_hover {
            let text = if route_lines.is_empty() {
                self.combat_rows.terrain(head, document, province, &vision, repaint())?
            } else {
                let inputs = self.hover_inputs(document, &vision, ctx);
                let key = inputs.key(hover_rows::What::Province { province, mode: mode_hover, routes: route_lines.clone() }, [0, 0]);
                self.hover_rows.ask(hover_rows::Family::Province, head, key, |_| inputs.terrain(Arc::clone(&document.combat), province, region, route_lines))?
            };
            return Some((text, [0.0, 0.0, extent[0], extent[1]], Some(province)));
        }
        if let viewer_counter_products::map::province_tooltips::ModeHover::StrategicAir(selected) =
            &mut mode_hover
        {
            *selected = Arc::clone(self.air_selection.wings());
        }
        let dominance_sources = || match &self.overlay_tables {
            Some(tables) => tables.naval_dominance_sources(),
            None => Arc::new(Err("naval dominance sources: the overlay's tables at the head are loading".into())),
        };
        if let viewer_counter_products::map::province_tooltips::ModeHover::StrategicNavy(sources)
        | viewer_counter_products::map::province_tooltips::ModeHover::SeaRegion(sources) = &mut mode_hover
        {
            *sources = dominance_sources();
        }
        let inputs = self.hover_inputs(document, &vision, ctx);
        let (selected, rosters) = (self.unit_selection.ordered_units(), document.playback.roster_sampler());
        let key = inputs.key(hover_rows::What::Province { province, mode: mode_hover.clone(), routes: route_lines.clone() }, [hover_rows::id(selected), hover_rows::id(&rosters)]);
        let text = self.hover_rows.ask(hover_rows::Family::Province, head, key, |caches| inputs.province(caches, province, mode_hover, route_lines, Arc::clone(selected), rosters))?;
        Some((text, [0.0, 0.0, extent[0], extent[1]], Some(province)))
    }

    fn map(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        document: &mut Document,
        view_id: Option<&str>,
    ) -> Result<(), String> {
        let state = frame
            .wgpu_render_state()
            .ok_or("WGPU renderer is unavailable")?;
        self.map_with_state(ui, state, document, view_id)
    }

    pub(crate) fn window_frame(
        &mut self,
        ui: &mut egui::Ui,
        state: Option<&eframe::egui_wgpu::RenderState>,
        document: &mut Document,
    ) -> bool {
        let drawn = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let result = match state {
                    Some(state) => self.map_with_state(ui, state, document, None),
                    None => Err("WGPU renderer is unavailable".to_owned()),
                };
                self.after_frame_with_visual_mods(document, result.as_ref().err());
                if let Err(error) = result {
                    self.refuse_capture(&error, ui.ctx());
                    self.error = Some(error);
                    false
                } else {
                    self.drew_frame
                }
            })
            .inner;
        drawn
    }

    fn map_with_state(
        &mut self,
        ui: &mut egui::Ui,
        state: &eframe::egui_wgpu::RenderState,
        document: &mut Document,
        view_id: Option<&str>,
    ) -> Result<(), String> {
        let tables = document.content.tables;
        let result = engine_executable::executable_tables::scope(tables, || {
            self.draw_map(ui, state, document, view_id)
        });
        self.ask_live(document);
        if !self.drew_frame {
            self.silence_unshown();
        }
        result
    }

    fn side_ask(
        &mut self,
        document: &Document,
    ) -> (
        viewer_layer_core::layers::View,
        crate::playhead_host::Head,
        Option<viewer_playback::overlay::Ask>,
        Option<viewer_combat::Asking>,
    ) {
        let drawing = self.last_view.is_some() && self.picture_ready();
        let at = head_wanted(
            self.capture.as_ref().map(|capture| capture.paced),
            self.playhead.map(|head| head.hour),
            self.options.window_dump,
            drawing,
        );
        let view = viewer_layer_core::layers::View {
            map_mode: self.map_mode.clone(),
            perspective: self.perspective.clone(),
            camera_height: self.last_view.as_ref().map_or(0.0, |view| view.camera.height),
            camera: self.last_view.as_ref().map(|view| view.camera.placement()),
            target: self.zoom.target(),
            viewport: self.last_view.as_ref().map_or([0, 0], |view| view.viewport),
            fog: self.perspective.fog,
            graphics: self.graphics,
            sound: self.audio_focus,
            audio: self.volumes,
            overlay: self.overlay_layers.clone(),
            counters: self.counter_filter.clone(),
            player_counters: self.player_counters,
            selected: self.diplomatic_selected,
        };
        let shown_mode = match at {
            crate::playhead_host::Head::Stepped(_) => self.map_mode.clone(),
            _ => self.shown_mode().clone(),
        };
        let overlay_view = {
            let mut view = self.counter_view.clone();
            view.set_mode(&shown_mode);
            view
        };
        let overlay_ask = self.last_view.as_ref().filter(|_| drawing).map(|last| viewer_playback::overlay::Ask {
            camera: last.camera,
            target: self.zoom.target().map(|target| target.height),
            pixels: last.viewport,
            ui_scale: last.ui_scale,
            view: overlay_view,
            rosters: document.playback.air_rosters(),
        });
        self.combat.set_playing(self.playing);
        self.combat.set_settled(self.settles_windows());
        let content_at = Arc::as_ptr(&document.content) as usize;
        if self.combat_inputs != Some(content_at) {
            self.combat_inputs = Some(content_at);
            let raids = Arc::clone(&document.icons.raids);
            self.combat.set_rule_relations(Arc::new(move |frame, rule, country| {
                viewer_unit_reads::movement_reads::rule_relation(frame, raids.movement_reads(), rule, country)
            }));
            let content = Arc::clone(&document.content);
            self.combat.set_navy_sources_of(Arc::new(move |frame, ours, region| content.naval_dominance_sources(frame, ours, region)));
            self.combat.set_battle_title_source(crate::battle_title_source(document));
        }
        let combat_ask = self.combat.asking(&document.combat, &document.playback.roster_sampler(), &self.perspective, self.counter_view.settings.nato);
        self.overlay_asked = overlay_ask.as_ref().map(|ask| ask.shown(&document.content.counters.defines));
        (view, at, overlay_ask, combat_ask)
    }

    fn ask_live(&mut self, document: &Document) {
        if self.playhead_map.is_none() {
            return;
        }
        let (view, at, overlay_ask, combat_ask) = self.side_ask(document);
        if !matches!(at, crate::playhead_host::Head::Stepped(_))
            && let Some(side) = self.playhead_map.as_ref()
        {
            side.ask(view, at, overlay_ask, combat_ask);
        }
    }

    fn draw_map(
        &mut self,
        ui: &mut egui::Ui,
        state: &eframe::egui_wgpu::RenderState,
        document: &mut Document,
        view_id: Option<&str>,
    ) -> Result<(), String> {
        let _other_cost = replay_frame::cpu::scope("frame.refresh", Some("other"));
        let whole = self.options.window_dump || self.capture.as_ref().is_some_and(|capture| !capture.paced);
        let _art = whole.then(viewer_native_gui::native_resources::SynchronousWindowArt::enter);
        let _budget = viewer_runtime::frame_budget::begin(!whole);
        let map_timer = Instant::now();
        self.drew_frame = false;
        let mut laps = self
            .laps
            .take()
            .unwrap_or_else(|| Laps::new(Duration::ZERO));
        laps.lap("before_map");
        viewer_unit_reads::counter_move::take_profile();
        if profiling() {
            static ADAPTER_ONCE: std::sync::Once = std::sync::Once::new();
            shader_pipeline::gpu::stats::mark_window_thread();
            let info = state.adapter.get_info();
            ADAPTER_ONCE.call_once(|| {
                profile_print(format!("PROFILE adapter {} {:?}", info.name, info.backend));
            });
        }
        let wall_clock = self
            .profile_playback
            .sample(Instant::now)
            .filter(|_| self.profile_playback.wall_active());
        if self.capture.is_none() || wall_clock.is_some() {
            let (now, elapsed) = wall_clock.map_or_else(
                || {
                    let now = Instant::now();
                    (now, now.duration_since(self.last_step).as_secs_f64())
                },
                |clock| (clock.now, clock.seconds),
            );
            self.last_step = now;
            if let Some(head) = self.playhead
                && let Some((_, last)) = document.playback.range(head.branch)
            {
                self.tick_playhead(elapsed, last);
            }
            if self.playing {
                ui.ctx().request_repaint();
            }
        }
        self.follow_content(Arc::as_ptr(&document.content));
        self.follow_branch();
        if !self.playhead_opened
            && let Some(clock) = self.playhead
        {
            self.playhead_opened = true;
            let at = crate::playhead_host::At { branch: clock.branch, hour: clock.hour, playing: self.playing, speed: self.speed };
            let ctx = ui.ctx().clone();
            let opened = crate::playhead_host::open_document(document, Some(at), self.private_playhead, move || ctx.request_repaint());
            match opened {
                Ok(opening) => self.playhead_opening = Some(opening),
                Err(error) => self.playhead_refused = Some(refusal(&error)),
            }
        }
        if let Some(made) = self.playhead_opening.as_ref().and_then(crate::playhead_host::Opening::poll) {
            self.playhead_opening = None;
            let wake: Arc<dyn Fn() + Send + Sync> = {
                let ctx = ui.ctx().clone();
                Arc::new(viewer_runtime::worker_priority::waking(move || ctx.request_repaint()))
            };
            match made.and_then(|side| viewer_playback::side::SideThread::new(side, wake)) {
                Ok(side) if self.playhead.is_some_and(|clock| side.branch().is_some_and(|branch| branch != clock.branch)) => {
                    self.drop_later(side);
                    self.playhead_opened = false;
                }
                Ok(side) => {
                    self.head_rows.open_clicks(side.views());
                    document.playback.read_rosters_of(side.entry());
                    let released = document.playback.release_source();
                    self.retire(released);
                    self.presented.reopened = true;
                    self.playhead_refused = None;
                    self.side_opened(side);
                }
                Err(error) => self.playhead_refused = Some(refusal(&error)),
            }
        }
        let mut shown_head = self.take_frame_head();
        let taken = shown_head.map(|head| head.hour).filter(|_| !self.saves_a_frame());
        if let Some(side) = self.playhead_map.as_ref() {
            side.views().show_through(taken);
        }
        self.hover_rows.playing = self.playing && !self.saves_a_frame();
        if self.playhead_map.is_some() {
            let drawing = self.last_view.is_some() && self.picture_ready();
            let at = head_wanted(
                self.capture.as_ref().map(|capture| capture.paced),
                self.playhead.map(|head| head.hour),
                self.options.window_dump,
                drawing,
            );
            let stepped = matches!(at, crate::playhead_host::Head::Stepped(_)).then(|| self.side_ask(document));
            let wanted = builds_warnings(self.diagnostics, self.capture.as_ref());
            let saved = self.saves_a_frame();
            let through = shown_head.map(|head| head.hour).filter(|_| !saved && self.playing);
            let mut installed = None;
            let mut withdrawn = false;
            let mut unsettled = None;
            {
                let presentation = Arc::clone(&self.counter_view.presentation);
                let side = self.playhead_map.as_mut().expect("the map's side is open");
                let window = match stepped {
                    Some((view, crate::playhead_host::Head::Stepped(hour), overlay_ask, combat_ask)) => {
                        side.ask(view, crate::playhead_host::Head::Stepped(hour), overlay_ask, combat_ask);
                        side.stepped_window(hour)
                    }
                    _ => side.window(),
                };
                if let Some(mut window) = { window } {
                    if saved && window.side().failure().is_none() {
                        let hour = window.side().last_frame().head;
                        unsettled = settle_models(window.side_mut(), hour, Duration::from_secs(120))
                            .and_then(|()| settle_plan(window.side_mut(), hour, Duration::from_secs(120)))
                            .err()
                            .map(|why| format!("A saved frame draws the models and plan of its own hour {hour}: {why}"));
                    }
                    if let Some(scene) = self.scene.as_mut() {
                        let mut gpu = viewer_layer_core::layers::Gpu { device: &state.device, queue: &state.queue, scene, content: &document.content, counter_gui: &document.icons.counters.gui, whole: through.is_none() };
                        window.upload(&mut gpu);
                    }
                    window.take(viewer_playback::side::Wants { warnings: wanted, highlight: self.highlight_wanted });
                    withdrawn = window.take_overlay_withdrawal();
                    if let Some(made) = window.take_overlay() {
                        let made = made.into_inner();
                        if !Arc::ptr_eq(&presentation, &made.on) {
                            presentation.take_memory(&made.on);
                        }
                        installed = Some(made);
                    }
                    if let Some(made) = window.take_combat() {
                        self.combat.offer(made.into_inner());
                    }
                }
                if side.products().missed.is_some() {
                    ui.ctx().request_repaint();
                }
                if let Some(why) = side.take_refusal() {
                    unsettled = Some(format!("A saved frame draws its own hour whole: {why}"));
                }
            }
            if withdrawn {
                self.forget_overlay();
            }
            if let Some(made) = installed {
                self.install_overlay(made, &document.playback.replay().gaps);
            }
            if let Some(side) = live_of(&self.playhead_map) {
                self.overlay_hour = side.products().overlay_hour;
            }
            if let Some(why) = unsettled {
                self.refused_frame(why, ui.ctx())?;
                self.rendered = None;
                ui.ctx().request_repaint();
                return Ok(());
            }
            if self.playing
                && !saved
                && let Some(covered) = live_of(&self.playhead_map).and_then(|side| side.products().overlay_hour)
            {
                shown_head = shown_head.map(|head| viewer_native_gui::frame_head::shown(head, Some(covered)));
            }
            if let Some(scene) = self.scene.as_mut() {
                let side = self.playhead_map.as_mut().expect("the map's side is open");
                let written = shown_head.map(|head| head.hour).filter(|_| !saved);
                let mut gpu = viewer_layer_core::layers::Gpu { device: &state.device, queue: &state.queue, scene, content: &document.content, counter_gui: &document.icons.counters.gui, whole: through.is_none() };
                side.install(&mut gpu, written);
                if let Some(installed) = side.installed_through()
                    && shown_head.is_some_and(|head| head.hour != installed)
                {
                    shown_head = Some(viewer_gpu::icon_tracks::Head { hour: installed, fraction: 0.0 });
                }
                side.views().show_through(taken.zip(shown_head).map(|(taken, head)| taken.min(head.hour)));
            }
            if let Some(retired) = self.combat.take_retired() {
                self.retire(retired);
            }
            if let Some(battles) = self.playhead_map.as_ref().and_then(|side| side.products().battles.as_deref().cloned()) {
                self.battles = battles;
            }
        }
        let pinned = self.capture.as_ref().and_then(|capture| capture.pinned);
        if self.last_view.is_none() && let Some(embedded) = &self.embedded {
            let frame = ui.ctx().input(|input| input.viewport().inner_rect).map(|rect| [
                (rect.width() * ui.ctx().pixels_per_point()).round() as u32,
                (rect.height() * ui.ctx().pixels_per_point()).round() as u32,
            ]);
            let placed = embedded.frame_size();
            if frame.is_none() || frame != placed {
                self.rendered = None;
                ui.ctx().request_repaint();
                self.wait_to_draw(document.playback.hour(), || format!("its window measured {frame:?} pixels while its host placed it at {placed:?}"), ui.ctx())?;
                return Ok(());
            }
        }
        let raster = match pinned {
            Some(_) => Raster::direct(self.options.ui_scale.unwrap_or(1.0)),
            None => {
                Self::watch_monitors(&mut self.monitor_seen, ui.ctx());
                let setting = self.ui_scale;
                let (matched, _) = Self::match_game(&mut self.match_game, document);
                match setting {
                    Setting::Manual(scale) => Raster::direct(scale),
                    Setting::MatchGame => *matched,
                }
            }
        };
        let ui_scale = raster.ui_scale;
        self.interface_points =
            ui_scale_points(1.0, raster.screen_scale(), ui.ctx().pixels_per_point());
        let size = ui.available_size();
        if size.x <= 0.0 || size.y <= 0.0 {
            self.rendered = None;
            if self.capture.is_some() {
                ui.ctx().request_repaint();
            }
            self.wait_to_draw(document.playback.hour(), || format!("its map had no area to draw in ({} by {} points)", size.x, size.y), ui.ctx())?;
            return Ok(());
        }
        let pixels = match pinned {
            Some((size, _)) => size,
            None => raster.target([
                (size.x * ui.ctx().pixels_per_point()).round() as u32,
                (size.y * ui.ctx().pixels_per_point()).round() as u32,
            ]),
        };
        {
            let _lap = replay_frame::cpu::scope("frame.refresh.counters", Some("other"));
            let left_out = self.overlay_tables.as_ref().map(|tables| tables.counters().left_out()).unwrap_or_default();
            let (published, draws) = match &self.overlay_layered {
                Some((published, draws)) => (published.clone(), *draws),
                None => self.playhead_map.as_ref().map(|side| (side.products().counters.clone(), side.products().counter_kinds)).unwrap_or_default(),
            };
            self.counter_view.presentation.counters.lock().unwrap_or_else(|error| error.into_inner()).set_layered(published, draws, left_out);
        }
        let map_error = self.map_error_of(document);
        self.hour_error(map_error, ui.ctx());
        self.follow_continuity(&document.playback.replay().gaps);
        if let Some(error) = self.map_error.clone() {
            self.rendered = None;
            ui.centered_and_justified(|ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            });
            return Ok(());
        }
        if !self.picture_ready() {
            if profiling() {
                profile_print(format!("PROFILE no_map frame {}", self.profile_frame));
            }
            self.wait_to_draw(document.playback.hour(), || "no picture of it was composed".to_owned(), ui.ctx())?;
            self.rendered = None;
            ui.ctx().request_repaint();
            ui.centered_and_justified(|ui| {
                ui.spinner();
            });
            return Ok(());
        }
        if self.warming {
            self.rendered = None;
            return Ok(());
        }
        let (stale_hour, perspective) = match self.live_map() {
            Some(side) => (side.composing(), side.on_screen().map_or_else(|| self.perspective.clone(), |_| side.view().perspective.clone())),
            None => (false, self.perspective.clone()),
        };
        if stale_hour && profiling() {
            profile_print(format!("PROFILE stale_hour frame {}", self.profile_frame));
        }
        let mode = self.shown_mode().clone();
        self.counter_view.set_mode(&mode);
        laps.lap("hour_misc");
        drop(_other_cost);
        let _other_cost = replay_frame::cpu::scope("frame.camera", Some("other"));
        let build_warnings = builds_warnings(self.diagnostics, self.capture.as_ref());
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let map_size = MapSize::of(&document.map);
        if !self.camera_manual {
            self.camera = None;
        }
        let defines = document.map_defines;
        let mut camera = self.camera.map_or_else(
            || Camera::fit(map_size, defines, pixels),
            |placement| placement.on(map_size, defines),
        );
        let camera = &mut camera;
        let held_pointer = self.input.captured();
        let placed_walk = pinned.is_some() && self.walk.placed();
        let interface_visible = pinned.is_none()
            && (self.camera_mode == CameraMode::Map
                || self.start_pick.choosing()
                || self.free_overlays);
        let find_typing = interface_visible && !self.picker.hidden && self.picker.find.typing();
        let controls = if pinned.is_some() {
            let time = self.capture.as_ref().and_then(|capture| capture.time);
            self.walk
                .placed_frame(time.unwrap_or(0.0))
                .unwrap_or_default()
        } else {
            let (events, focused, modifiers) =
                ui.input(|input| (input.events.clone(), input.focused, input.modifiers));
            self.input.frame(crate::controls::Window {
                events: &events,
                focused: focused && !find_typing,
                modifiers,
                now: Instant::now(),
            })
        };
        if pinned.is_none()
            && !self.dashboard_map
            && (controls.pressed.contains(&CAMERA_MODE_KEY)
                || controls
                    .pad
                    .pressed(dashboard_core::protocol::GamepadButton::Select))
        {
            self.set_camera_mode(self.camera_mode.next(), self.free_overlays, ui.ctx());
        }
        let free_mode = self.camera_mode != CameraMode::Map
            && !self.start_pick.choosing()
            && (pinned.is_none() || placed_walk || self.options.fly.is_some());
        let interface = match pinned {
            Some(_) if free_mode => self.options.free_overlays.unwrap_or(false),
            Some((_, Some(placement))) if placement.pose != Pose::Map => {
                self.options.free_overlays.unwrap_or(false)
            }
            Some(_) => true,
            None => !free_mode || self.free_overlays,
        };
        let flying_names = free_mode && self.camera_mode == CameraMode::Fly;
        let scale = ui.ctx().pixels_per_point() / raster.stretch;
        let frame_point = |position: egui::Pos2| {
            [
                (position.x - rect.left()) * scale,
                (position.y - rect.top()) * scale,
            ]
        };
        let extent = ui_scale_extent(pixels, ui_scale);
        self.window_layout = if interface {
            let viewport = extent.map(|value| value as u32);
            Some(document.icons.counters.window_cache.at(viewport).map_err(|error| error.to_string())?)
        } else {
            None
        };
        let owner = ui
            .ctx()
            .data(|data| data.get_temp::<egui::Id>(egui::Id::new(MAP_INPUT_OWNER)));
        let takes_keys = takes_map_keys(self.dashboard_map, owner, response.id, response.hovered());
        let escape = takes_keys && ui.input(|input| input.key_pressed(egui::Key::Escape));
        let find_visible = interface && !self.picker.hidden && self.picker.find.open;
        if escape && !held_pointer && !self.start_pick.choosing() && !find_visible {
            if self.combat.tactics_open {
                self.combat.tactics_open = false;
            } else {
                self.combat.close();
                self.unit_selection.clear();
                self.ground_selection = None;
                self.air_selection.clear();
                self.fleet_selection.clear();
            }
        }
        let paused_motion = self.paused_motion();
        let presentation_time = self
            .capture
            .as_ref()
            .and_then(|capture| capture.time)
            .map_or_else(|| self.started.elapsed().as_secs_f64(), f64::from);
        let animation_time = presentation_time as f32;
        let ambient_time = self.ambient_time_at(presentation_time);
        let prepared = self.combat_prepared().map_err(|error| error.to_string())?;
        let combat_geometry = self
            .combat
            .draw(&document.combat, prepared, extent, false, animation_time, false, None)
            .map_err(|error| error.to_string())?;
        let painted = self.combat_painted.clone();
        let over_window = |at: [f32; 2]| combat_geometry.as_ref().is_some_and(|window| window.contains(at)) || painted.contains(at);
        let point = profile_hover_point(
            response
                .hover_pos()
                .or_else(|| response.interact_pointer_pos())
                .map(frame_point),
            self.options.profile_pointer,
            profiling() || self.options.playback_cost_end.is_some(),
            pixels,
            ui_scale,
        );
        let reaches = !free_mode || (interface && !self.input.captured());
        let ((changes, change_modifiers), held, input_time) =
            ui.input(|input| (pointer_changes::changes(input), input.pointer.primary_down(), input.time));
        let frame_modifiers = ui.input(|input| input.modifiers);
        let modifiers_at = |order: usize| change_modifiers.get(order).copied().unwrap_or(frame_modifiers);
        let on_map = |pos: egui::Pos2| {
            response.interact_rect.contains(pos)
                && ui.ctx().layer_id_at(pos) == Some(response.layer_id)
        };
        let admitted = |button: egui::PointerButton| -> Vec<(usize, egui::Pos2, bool)> {
            changes
                .iter()
                .enumerate()
                .filter_map(|(order, &(pos, change))| {
                    let (which, pressed) = change?;
                    (which == button && (!pressed || on_map(pos))).then_some((order, pos, pressed))
                })
                .collect()
        };
        let primary = if reaches {
            admitted(egui::PointerButton::Primary)
        } else {
            Vec::new()
        };
        let secondary = if reaches {
            admitted(egui::PointerButton::Secondary)
        } else {
            Vec::new()
        };
        let interface_at = |pos: egui::Pos2| to_interface(frame_point(pos), ui_scale);
        let gui_input = Some(reaches.then(|| {
            use viewer_native_gui::native_gui::pointer::{Button, Input, Place};
            Input {
                place: point.map(Place::new),
                buttons: primary
                    .iter()
                    .map(|&(order, pos, pressed)| Button {
                        down: pressed,
                        place: Some(Place::new(interface_at(pos))),
                        order,
                    })
                    .collect(),
                held,
            }
        }).unwrap_or_default());
        if takes_keys && reaches {
            let filter = if !free_mode {
                CameraKeyFilter::None
            } else if self.camera_mode == CameraMode::Walk {
                CameraKeyFilter::Walk
            } else if self.fly_aircraft(document).is_some() {
                CameraKeyFilter::FlyAircraft
            } else {
                CameraKeyFilter::FlyFree
            };
            self.confirmation_keys(ui, document, extent, filter);
            self.picker_keys(ui, document, extent, filter);
        }
        let picker_controls = match &self.picker_controls {
            Some((drawn_at, controls)) if !self.picker.hidden && *drawn_at == extent => {
                controls.clone()
            }
            _ => match self.draw_picker(document, extent, false, animation_time, false, None) {
                Some(Ok(drawn)) => drawn.controls,
                _ => Vec::new(),
            },
        };
        let picker_placed_at = |at: [f32; 2]| {
            picker_controls
                .iter()
                .rev()
                .find(|placed| placed.contains(at))
                .cloned()
        };
        let picker_at = |at: [f32; 2]| picker_placed_at(at).map(|placed| placed.control);
        let holder = if self.air_orders.holding() {
            Some(Holder::Confirmation)
        } else if self.picker.holding() {
            Some(Holder::Picker)
        } else if self.combat.holding() {
            Some(Holder::Combat)
        } else {
            self.counter_selecting.map(|_| Holder::Counters)
        };
        let owner = |at: [f32; 2]| {
            if self.air_orders.over(at) {
                Some(Holder::Confirmation)
            } else if picker_at(at).is_some() {
                Some(Holder::Picker)
            } else if over_window(at) {
                Some(Holder::Combat)
            } else {
                matches!(self.map_hit(at), Some(MapHit::Unit(_))).then_some(Holder::Counters)
            }
        };
        let [picker_input, combat_input, counters_input, confirmation_input] =
            [Holder::Picker, Holder::Combat, Holder::Counters, Holder::Confirmation].map(|me| {
                gui_input
                    .as_ref()
                    .map(|input| input.shared(holder, &owner, me))
            });
        let (click_dist, click_time) = ui
            .ctx()
            .options(|options| {
                (options.input_options.max_click_dist, options.input_options.max_click_duration)
            });
        let mut pressed_on = self.pressed_on;
        let mut bare_clicks = Vec::new();
        let mut off_picker_clicks = Vec::new();
        let mut box_press = self.selection_input.press;
        let mut box_releases = Vec::new();
        let boxes_from = |at: [f32; 2]| {
            !free_mode && !self.start_pick.choosing() && owner(at).is_none() && self.map_hit(at).is_none()
        };
        for (this, &(pos, change)) in changes.iter().enumerate().filter(|_| reaches) {
            let Some((egui::PointerButton::Primary, pressed)) = change else {
                if change.is_none()
                    && let Some((_, from, _, far)) = &mut pressed_on
                {
                    *far |= pos.distance(*from) > click_dist;
                }
                if change.is_none()
                    && let Some(press) = &mut box_press
                {
                    press.moved(pos, frame_point(pos));
                }
                continue;
            };
            if pressed && !on_map(pos) {
                continue;
            }
            let at = interface_at(pos);
            if pressed {
                pressed_on = Some((owner(at), pos, input_time, false));
                box_press = boxes_from(at).then(|| selection_input::BoxPress::new(pos, frame_point(pos)));
                continue;
            }
            let boxed = box_press.take();
            if let Some(press) = boxed
                && viewer_counters::box_selection::boxes(press.from_frame, frame_point(pos))
            {
                pressed_on = None;
                box_releases.push((interface_at(press.from), at, this));
                continue;
            }
            if let Some((on, from, since, far)) = pressed_on.take()
                && ((!far && pos.distance(from) <= click_dist && input_time - since <= click_time)
                    || boxed.is_some())
            {
                if on != Some(Holder::Picker) {
                    off_picker_clicks.push(this);
                }
                if on.is_none() && owner(at).is_none() {
                    bare_clicks.push((at, this));
                }
            }
        }
        if !held || !reaches {
            pressed_on = None;
            box_press = None;
        }
        self.pressed_on = pressed_on;
        self.selection_input.press = box_press;
        let mut secondary_clicks = Vec::new();
        let mut air_order_releases = Vec::new();
        for &(order, pos, pressed) in &secondary {
            let placed = picker_placed_at(interface_at(pos));
            let at = interface_at(pos);
            let free = placed.is_none()
                && !over_window(at)
                && !self.air_orders.over(at);
            let under = self.map_hit(at).map(|hit| air_orders::Under::of(hit, &self.counter_report));
            if let Some(under) = self.air_orders.secondary(pressed, free, under) {
                air_order_releases.push((order, at, under));
            }
            if pressed {
                self.secondary_on = placed.map(|placed| placed.control);
            } else if let Some(pressed_on) = self.secondary_on.take()
                && let Some(placed) = placed.filter(|placed| placed.control == pressed_on)
                && matches!(placed.control, viewer_combat::picker::Control::Slot(_))
                && !placed.disabled
            {
                secondary_clicks.push((order, placed));
            }
        }
        if !reaches || !ui.input(|input| input.pointer.secondary_down()) {
            self.secondary_on = None;
            self.air_orders.release();
        }
        let picker_hit = point.filter(|_| reaches).and_then(picker_at);
        let over_picker = picker_hit.is_some();
        if reaches && response.drag_started() && !over_picker {
            self.combat.start_drag(combat_geometry.as_ref(), point);
        }
        if response.drag_stopped() {
            self.combat.dragging = false;
        }
        let over_combat =
            reaches && (self.combat.dragging || over_picker || point.is_some_and(&over_window));
        self.combat.set_modifiers(frame_modifiers.shift, frame_modifiers.ctrl);
        if over_combat {
            if response.dragged() && self.combat.dragging {
                let delta = response.drag_delta();
                self.combat
                    .drag_by([delta.x * scale / ui_scale, delta.y * scale / ui_scale]);
            }
            if let Some(side) = point.and_then(|point| {
                combat_geometry
                    .as_ref()
                    .and_then(|window| window.list_at(point))
            }) {
                let scroll = ui.input(|input| {
                    input
                        .raw
                        .events
                        .iter()
                        .filter_map(|event| match event {
                            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                            _ => None,
                        })
                        .sum::<f32>()
                });
                if scroll < 0.0 {
                    self.combat.scroll(side.clone(), 1, true);
                }
                if scroll > 0.0 {
                    self.combat.scroll(side, -1, true);
                }
            }
        }
        let frame_delta = self
            .camera_step
            .unwrap_or_else(|| self.last_frame.elapsed().as_secs_f32());
        self.last_frame = Instant::now();
        let owner = egui::Id::new(MAP_INPUT_OWNER);
        if response.is_pointer_button_down_on() {
            ui.ctx()
                .data_mut(|data| data.insert_temp(owner, response.id));
        }
        let owns = ui.ctx().data(|data| data.get_temp::<egui::Id>(owner)) == Some(response.id);
        let window_focused = ui.input(|input| input.focused);
        let focused = window_focused && (!self.dashboard_map || owns) && !find_typing;
        let pointed = response.hovered() && !over_combat;
        let mut started = false;
        if self.start_pick.choosing() {
            let shown = self.orbit.view(*camera);
            let field = viewer_gpu::province_map::HeightField {
                width: document.map.width,
                height: document.map.height,
                values: document.assets.heightmap_texels(),
            };
            let ground = |screen: [f32; 2]| drop::ground_at(&shown, screen, pixels, &field);
            let clicked = response
                .interact_pointer_pos()
                .filter(|_| response.clicked() && !over_combat)
                .and_then(|position| ground(frame_point(position)));
            let south = controls
                .pad
                .pressed(dashboard_core::protocol::GamepadButton::South);
            let east = controls
                .pad
                .pressed(dashboard_core::protocol::GamepadButton::East);
            let centre = south
                .then(|| ground([pixels[0] as f32 * 0.5, pixels[1] as f32 * 0.5]))
                .flatten();
            match drop::choose(escape, east, clicked, south, centre, camera.map.x()) {
                drop::Choice::Wait => {}
                drop::Choice::Cancel => {
                    self.set_camera_mode(CameraMode::Map, self.free_overlays, ui.ctx());
                }
                drop::Choice::Start(at) => {
                    self.start_pick = drop::Pick::At(at);
                    self.picker.find.field.focused = false;
                    self.input.forget_pad_taps();
                    if south {
                        self.input
                            .suppress_pad(dashboard_core::protocol::GamepadButton::South);
                    }
                    self.hud.enter_mode();
                    started = true;
                    ui.ctx().request_repaint();
                }
            }
        }
        let icon_click = free_mode
            && interface
            && response.clicked()
            && !self.input.captured()
            && point.is_some_and(|point| {
                matches!(
                    self.map_hit(point),
                    Some(MapHit::Combat(_) | MapHit::Unit(_))
                )
            });
        if free_mode
            && !icon_click
            && !over_combat
            && (response.clicked() || response.drag_started())
        {
            self.picker.find.field.focused = false;
            self.input.capture();
        }
        if !free_mode
            && response.dragged_by(egui::PointerButton::Middle)
            && !over_combat
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.camera_manual = true;
            let delta = response.drag_delta();
            let to = frame_point(pointer);
            let from = [to[0] - delta.x * scale, to[1] - delta.y * scale];
            let before = camera.center;
            if self.orbit.is_native() {
                camera.pan(from, to, pixels);
            } else {
                self.orbit.pan(camera, from, to, pixels);
            }
            self.zoom
                .shift([camera.center[0] - before[0], camera.center[1] - before[1]]);
        }
        let texels = document.assets.heightmap_texels();
        let raster_fits =
            texels.len() as u64 == u64::from(document.map.width) * u64::from(document.map.height);
        let floor = |x: f32, z: f32| {
            if raster_fits {
                terrain_height(&document.map, texels, [x, z])
            } else {
                0.0
            }
        };
        let presses: Vec<([f32; 2], bool, egui::Modifiers)> = primary
            .iter()
            .map(|&(order, pos, pressed)| (interface_at(pos), pressed, modifiers_at(order)))
            .collect();
        let map_pinged = self.map_windows_pointer(
            ui,
            document,
            camera,
            &floor,
            if reaches { &picker_controls } else { &[] },
            point.filter(|_| reaches),
            &presses,
            held && reaches,
            extent,
            &over_window,
        );
        let law = &document.camera_law;
        let (lines, zoom_keys, keys) = ui.input(|input| {
            let mut lines = Vec::new();
            let mut zoom_keys = Vec::new();
            for event in &input.raw.events {
                match event {
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta,
                        ..
                    } => lines.push(delta.y),
                    egui::Event::Key {
                        key: key @ (egui::Key::PageUp | egui::Key::PageDown),
                        pressed: true,
                        ..
                    } => zoom_keys.push(*key == egui::Key::PageUp),
                    _ => {}
                }
            }
            let keys = Pan {
                up: input.key_down(egui::Key::ArrowUp),
                down: input.key_down(egui::Key::ArrowDown),
                left: input.key_down(egui::Key::ArrowLeft),
                right: input.key_down(egui::Key::ArrowRight),
            };
            (lines, zoom_keys, keys)
        });
        let native = !free_mode && self.orbit.is_native();
        if focused && !free_mode {
            for zoom_in in zoom_keys {
                self.camera_manual = true;
                self.zoom.key(camera, law, zoom_in, &floor);
            }
        }
        let notches = if pointed && native {
            self.zoom.wheel_events(lines)
        } else {
            Vec::new()
        };
        self.interacted = response.clicked() || response.dragged() || !notches.is_empty();
        let pointer = response.hover_pos().map(frame_point);
        let ground = match pointer {
            Some(point) if !notches.is_empty() => {
                motion_ground_point(camera, point, pixels, &document.map, texels)
                    .ok()
                    .flatten()
            }
            _ => None,
        };
        let held = if focused && !free_mode {
            keys
        } else {
            Pan::default()
        };
        let edge = pointer.filter(|&point| focused && pointed && law.edge_scrolls(point, pixels));
        let input = MotionInput {
            cursor: pointer.filter(|_| focused && pointed && native),
            notches: &notches,
            ground,
            keys: if native { held } else { Pan::default() },
        };
        let stepped = self
            .zoom
            .step(camera, law, frame_delta, pixels, &input, &floor);
        let moved = self.zoom.ease(camera, law, frame_delta, pixels, &floor);
        if stepped || moved {
            self.camera_manual = true;
            request_animation_frame(ui.ctx(), map_timer.elapsed());
        }
        if !free_mode {
            let mut orbited = false;
            let mut push = [0.0f32; 2];
            if !self.orbit.is_native() {
                let axis = |low: bool, high: bool| f32::from(i8::from(high) - i8::from(low));
                push = [axis(held.left, held.right), axis(held.down, held.up)];
                if let Some([x, y]) = edge {
                    let toward = [x - pixels[0] as f32 * 0.5, pixels[1] as f32 * 0.5 - y];
                    let length = toward[0].hypot(toward[1]);
                    if length > 0.0 {
                        push = [push[0] + toward[0] / length, push[1] + toward[1] / length];
                    }
                }
                let scroll = ui.input(|input| input.smooth_scroll_delta.y);
                if pointed
                    && scroll != 0.0
                    && let Some(point) = pointer
                {
                    self.orbit.zoom(
                        camera,
                        (scroll * crate::controls::orbit::WHEEL_ZOOM).exp(),
                        point,
                        pixels,
                    );
                    orbited = true;
                }
            }
            orbited |= self.orbit.update(&controls, camera, push);
            if orbited {
                self.zoom.reset();
                self.camera_manual = true;
                request_animation_frame(ui.ctx(), map_timer.elapsed());
            }
        }
        let pending_camera = crate::suite_input::take_camera(view_id).filter(|command| !command.is_cancelled());
        if let Some(command) = &pending_camera {
            self.zoom.reset();
            self.orbit.reset();
            self.camera_manual = crate::suite_input::apply_camera(camera, command.target, pixels, &floor);
            ui.ctx().request_repaint();
        }
        let shift = viewer_gpu::province_admission::seam_shift(*camera, pixels);
        if shift != 0.0 {
            camera.center[0] += shift;
            self.zoom.shift([shift, 0.0]);
        }
        if let Some(command) = pending_camera {
            command.finish(Ok(crate::suite_input::ResolvedCamera {
                center: camera.center,
                height: camera.height,
            }));
        }
        self.camera = Some(camera.placement());
        let (camera, overlay_camera) = match pinned.filter(|_| !placed_walk) {
            Some((size, camera)) => {
                let camera = camera.map_or_else(
                    || Camera::fit(map_size, defines, size),
                    |placement| placement.on(map_size, defines),
                );
                match self.options.fly {
                    Some(scripted) if free_mode => {
                        let time = self.capture.as_ref().and_then(|capture| capture.time);
                        let dt = match (time, self.fly_clock) {
                            (Some(time), Some(last)) => (time - last).max(0.0),
                            _ => 0.0,
                        };
                        self.fly_clock = time;
                        let eye = self.fly(document, camera, None, 0.0, &scripted.frame(dt));
                        (eye, if interface { eye } else { camera })
                    }
                    _ => (camera, camera),
                }
            }
            None => {
                let map = self.orbit.view(*camera);
                if free_mode {
                    let texels = document.assets.heightmap_texels();
                    let field = viewer_gpu::province_map::HeightField {
                        width: document.map.width,
                        height: document.map.height,
                        values: texels,
                    };
                    let floor = |x: f32, z: f32| field.surface_height(x, z);
                    let wheel = if response.hovered() && !over_combat {
                        ui.input(|input| input.smooth_scroll_delta.y)
                    } else {
                        0.0
                    };
                    let eye = if self.camera_mode == CameraMode::Walk {
                        let vision = self.shown_vision().ok();
                        let head = head_rows::head(live_of(&self.playhead_map));
                        let branch = head_rows::branch(live_of(&self.playhead_map), document);
                        let key = self.head_rows.walk_hour(head).map(|hour| (branch, hour, perspective.clone()));
                        if let Some(key) = key.filter(|key| self.walk.stale(key))
                            && let Some(figure) = vision.and_then(|vision| self.head_rows.walk(head, None, |frame| document.unit_models.infantry_figure(frame, vision.ours())))
                        {
                            self.walk.set_figure(key, figure);
                        }
                        let world = crate::controls::walker::World::of(&map, &floor);
                        let start = self.start_pick.at().unwrap_or(camera.center);
                        let previous = self.walk.state().copied();
                        let eye = self.walk.camera(&controls, wheel, map, start, &world);
                        if self.walk.state() != previous.as_ref() || controls.moved_or_looked() {
                            request_animation_frame(ui.ctx(), map_timer.elapsed());
                        }
                        eye
                    } else {
                        let previous = self.free_cam;
                        let eye = self.fly(document, map, self.start_pick.at(), wheel, &controls);
                        if self.flight.is_some() || self.free_cam != previous || controls.moved_or_looked() {
                            request_animation_frame(ui.ctx(), map_timer.elapsed());
                        }
                        eye
                    };
                    (eye, if interface { eye } else { map })
                } else {
                    (map, map)
                }
            }
        };
        crate::perf_log::frame_view(camera.free().is_some(), camera.height);
        self.grab_pointer(ui.ctx());
        let mut box_stroke: Option<(egui::Rect, egui::Stroke)> = None;
        if response.hovered() && !self.pointer_grabbed {
            use viewer_playback::cursors::MapCursor;
            let shift = ui.ctx().input(|input| input.modifiers.shift);
            let chosen = self.map_cursor(document, point.filter(|_| !free_mode || interface), over_combat, shift);
            let cursors = &document.content.cursors;
            if let Ok(cursor) = cursors.get(chosen).or_else(|_| cursors.get(MapCursor::Normal)) {
                let cursor_time = if self.dashboard_map {
                    f64::from(ambient_time)
                } else {
                    self.started.elapsed().as_secs_f64()
                };
                let (image, next) = cursor.at(cursor_time);
                ui.ctx().output_mut(|output| output.cursor_image = Some(image.clone()));
                if let Some(delay) = next
                    && paused_motion != PausedMotion::Held
                {
                    ui.ctx().request_repaint_after(Duration::from_secs_f64(delay));
                }
            }
        }
        if self.selection_input.dragging()
            && let Some(press) = self.selection_input.press
        {
            let stroke = egui::Stroke::new(2.0 / ui.ctx().pixels_per_point(), egui::Color32::WHITE);
            box_stroke = Some((egui::Rect::from_two_pos(press.from, press.at), stroke));
        }
        laps.lap("panel_camera");
        drop(_other_cost);
        let _other_cost = replay_frame::cpu::scope("frame.movers", Some("other"));
        let scene = self.scene.as_mut().expect("created above");
        let saving = self.options.window_dump
            || self
                .capture
                .as_ref()
                .is_some_and(|capture| capture.remaining == 0);
        if saving != self.settled_capture {
            scene.settle_builds(saving);
            self.settled_capture = saving;
        }
        let stopped = saving || !self.playing;
        if stopped != self.settled_poses {
            scene.settle_poses(stopped);
            self.settled_poses = stopped;
        }
        let tick = 0.0;
        let convoys_shown = |defines: &viewer_unit_models::trade_routes::ConvoyDefines| {
            viewer_unit_models::trade_routes::convoys_shown(
                camera.height,
                defines,
                self.graphics.units,
                self.counter_view.visibility.trade_routes,
            )
        };
        let posed = live_of(&self.playhead_map).and_then(|side| side.products().units.as_deref().cloned());
        let sailing = posed.as_ref().is_some_and(|prepared| {
            prepared.has_convoys()
                && document
                    .trade_routes
                    .as_ref()
                    .is_ok_and(|defines| convoys_shown(&defines.convoys))
        });
        if (self.raid_tick != Some(tick) || sailing || self.convoys_sailing)
            && let Some(prepared) = posed.as_ref()
        {
            let heights = document.content.heights();
            let mut units = prepared.raid_units(&document.unit_models, Some((&heights, tick)));
            self.convoy_refused.clear();
            units.extend(prepared.convoy_units(
                &document.unit_models,
                ambient_time,
                &|x, z| heights.surface_height(x, z),
                convoys_shown,
                &mut self.convoy_refused,
            ));
            scene
                .apply(&state.device, &state.queue, SceneInput::SetRaidUnits(units))
                .map_err(|error| error.to_string())?;
            self.raid_tick = Some(tick);
            self.convoys_sailing = sailing;
        }
        laps.lap("movers_place");
        drop(_other_cost);
        let _other_cost = replay_frame::cpu::scope("frame.selection", Some("other"));
        let fade = self
            .fade
            .get_or_insert_with(|| LayerFade::instant(camera))
            .step(camera, frame_delta);
        self.fade = Some(fade);
        laps.lap("names_fade");
        let seen = self.shown_vision().ok();
        let head = head_rows::head(live_of(&self.playhead_map));
        let branch = head_rows::branch(live_of(&self.playhead_map), document);
        let frame = None;
        if let Some(ours) = seen.as_ref().map(|vision| vision.ours()) {
            if self.unit_selection.units().is_empty() {
                self.unit_selection.context(branch);
            } else {
                self.head_rows.units(head, frame, |frame| self.unit_selection.observe(branch, frame));
            }
            if self.air_selection.wings().is_empty() && self.air_selection.bases().is_empty() {
                self.air_selection.context(branch);
            } else {
                self.head_rows.air(head, frame, |frame| self.air_selection.observe(branch, ours, frame));
            }
            if self.fleet_selection.fleets().is_empty() {
                self.fleet_selection.context(branch);
            } else {
                self.head_rows.fleets(head, frame, |frame| self.fleet_selection.observe(branch, frame));
            }
        }
        self.counter_view.selected = Arc::clone(self.unit_selection.units());
        laps.lap("unit_selection");
        self.counter_view.air_selected = Arc::clone(self.air_selection.wings());
        self.counter_view.air_bases = Arc::clone(self.air_selection.bases());
        laps.lap("air_selection");
        self.counter_view.fleets_selected = Arc::clone(self.fleet_selection.fleets());
        laps.lap("fleet_selection");
        drop(_other_cost);
        let _other_cost = replay_frame::cpu::scope("frame.name-hover", Some("other"));
        let point = point.filter(|_| !free_mode || (interface && !self.input.captured()));
        let counters_point = point.filter(|_| {
            counters_input
                .as_ref()
                .is_none_or(|input| input.place.is_some())
        });
        self.counter_view.hovered = counters_point.filter(|_| !over_combat).and_then(|point| {
            let MapHit::Unit(index) = self.map_hit(point)? else {
                return None;
            };
            self.counter_report.unit_hits[index].units().next()
        });
        self.counter_view.raid_hovered = counters_point.filter(|_| !over_combat).and_then(|point| {
            matches!(self.map_hit(point)?, MapHit::Raid).then_some(())?;
            self.raid_hit(point).map(|hit| hit.province)
        });
        let frame_sounds = self.interface_sounds.len();
        let was = (self.counter_view.pressed, self.counter_view.plate_hovered);
        let mut held = self.counter_held;
        let mut counter_sounds = Vec::new();
        let mut counter_clicks: Vec<(Unit, usize)> = Vec::new();
        let plate = |pressed: Option<Unit>, hovered: Option<Unit>| {
            pressed
                .map(|unit| (unit, true))
                .or(hovered.map(|unit| (unit, false)))
        };
        let mut change =
            |next: Option<(Unit, bool)>, order: usize, sounds: &mut Vec<(Unit, Sound, usize)>| {
                if held == next {
                    return;
                }
                if let Some((before, _)) = held
                    && next.is_none_or(|(after, _)| after != before)
                {
                    sounds.push((before, Sound::Up, order));
                }
                if let Some((after, down)) = next {
                    let sound = if down { Sound::Down } else { Sound::Over };
                    sounds.push((after, sound, order));
                }
                held = next;
            };
        let counter_at = |at: [f32; 2]| {
            if picker_at(at).is_some() || over_window(at) {
                return None;
            }
            let target = self.map_hit(at)?;
            let MapHit::Unit(index) = target else {
                return None;
            };
            let hit = &self.counter_report.unit_hits[index];
            let local = self.local_point(self.hit_layer(target), at);
            Some((hit.units().next()?, hit.on_plate(local)))
        };
        let plate_of = |under: Option<(Unit, bool)>| under.filter(|(_, on)| *on).map(|(u, _)| u);
        let unders: Vec<_> = counters_input
            .iter()
            .flat_map(|input| &input.buttons)
            .map(|button| button.place.and_then(|place| counter_at(place.at)))
            .collect();
        let plate_hovered = plate_of(
            counters_point
                .filter(|_| !over_combat)
                .and_then(counter_at),
        );
        if let Some(input) = &counters_input {
            for (button, &under) in input.buttons.iter().zip(&unders) {
                if !button.down {
                    if let Some(selecting) = self.counter_selecting
                        && under.map(|(unit, _)| unit) == Some(selecting)
                    {
                        counter_clicks.push((selecting, button.order));
                    }
                    if let Some(pressed) = self.counter_view.pressed
                        && plate_of(under) == Some(pressed)
                    {
                        counter_sounds.push((pressed, Sound::Click, button.order));
                    }
                }
                self.counter_selecting = under.filter(|_| button.down).map(|(unit, _)| unit);
                self.counter_view.pressed = plate_of(under).filter(|_| button.down);
                change(
                    plate(self.counter_view.pressed, plate_of(under)),
                    button.order,
                    &mut counter_sounds,
                );
            }
            if !input.held {
                self.counter_selecting = None;
                self.counter_view.pressed = None;
            }
        }
        self.counter_view.plate_hovered = plate_hovered;
        change(
            plate(self.counter_view.pressed, self.counter_view.plate_hovered),
            viewer_native_gui::native_gui::pointer::LAST,
            &mut counter_sounds,
        );
        self.counter_held = held;
        for (unit, sound, order) in counter_sounds {
            let control = highlight::HoverTarget::Counter(unit).to_string();
            if let Ok(sounds) = document.icons.counters.plate_sounds(unit)
                && let Some(played) = sound.played(&control, &sounds, order)
            {
                self.interface_sounds
                    .push(InterfaceSound::new("counter", &played));
            }
        }
        if (self.counter_view.pressed, self.counter_view.plate_hovered) != was {
            viewer_runtime::redraws::note("asked.pointer", 1);
            self.overlay_fraction = None;
        }
        if let Some(prepared) = self.prepared_overlay.as_mut() {
            let now = (self.counter_view.raid_details.clone(), self.counter_view.raid_hovered);
            if raid_details::redraws(&prepared.raid_view(), &now) {
                prepared.set_raid_view(now);
                self.overlay_fraction = None;
            }
        }
        self.last_view = Some(View {
            camera: overlay_camera,
            viewport: pixels,
            ui_scale,
        });
        let hovered_name_province = point
            .filter(|_| !document.content.game_strategic_names)
            .and_then(|point| {
                pick(
                    &camera,
                    point.map(|v| v * ui_scale),
                    pixels,
                    &document.map,
                    document.assets.heightmap_texels(),
                )
                .ok()
                .flatten()
            });
        let hovered_name_region = hovered_name_province
            .map(|province| document.content.counters.region(province).unwrap_or(0));
        let names_mode = self.shown_mode();
        let strategic_names = matches!(
            names_mode,
            viewer_map_modes::map_mode::MapMode::Stock(
                viewer_map_modes::map_mode::Stock::StrategicAir
                    | viewer_map_modes::map_mode::Stock::StrategicNavy
            )
        );
        let show_all_names = document.content.game_strategic_names || !strategic_names;
        let strategic_names = if show_all_names {
            StrategicNames::All
        } else {
            StrategicNames::Hover(hovered_name_province)
        };
        let name_fade = fade.strategic_hover(
            show_all_names,
            hovered_name_region,
            self.hovered_name_region,
        );
        self.fade = Some(name_fade);
        if name_fade != LayerFade::instant(camera) {
            request_animation_frame(ui.ctx(), map_timer.elapsed());
        }
        self.hovered_name_region = hovered_name_region;
        laps.lap("names_pick");
        drop(_other_cost);
        let overlay_prepare_cpu = viewer_runtime::profiling::thread_span("overlay_prepare");
        let left = self.overlay_left;
        let held = self.overlay_frame.clone().zip(self.overlay_tables.clone());
        if held.is_none() {
            self.rendered = None;
            if live_of(&self.playhead_map).is_none_or(|side| side.products().overlay_coming()) {
                if let Some(texture) = self.scene_target.kept(self.scene.as_ref().map(|scene| scene.generation())) {
                    ui.painter().image(texture, rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                return Ok(());
            }
            if self.drop_overlay_drawn()
                && let Some(scene) = self.scene.as_mut()
            {
                let (device, queue, none) = (&state.device, &state.queue, viewer_aircraft::radar_symbols::Field::default());
                for input in [SceneInput::SetAirRanges(&none), SceneInput::SetAirTransfers(&none), SceneInput::SetAirAssignment(&Default::default()), SceneInput::SetStrategicStyles(&Default::default())] {
                    scene.apply(device, queue, input).map_err(|error| error.to_string())?;
                }
            }
        }
        let window_misses = document.icons.combat.gui.window_art_misses();
        let (overlay_ms, additions, combat_out, combat_art) = 'overlay: {
            let Some((rows, tables)) = held.clone() else {
                let mut tooltip_refused = BTreeMap::new();
                if let Some(side) = live_of(&self.playhead_map) {
                    let products = side.products();
                    self.overlay_missed.clone_from(&products.overlay_missed);
                    for reason in &products.overlay_loading {
                        *tooltip_refused.entry(reason.clone()).or_default() += 1;
                    }
                }
                self.tooltip_refused = tooltip_refused;
                break 'overlay (0.0, Batch::default(), Some("the frame drew no overlay to draw it in".to_owned()), None);
            };
            let now = crate::document::Hour::new(&document.content, &rows, 0.0).with_air_rosters(Some(document.playback.air_rosters())).with_moved_hours(Arc::clone(&self.overlay_moved));
            let _other_cost = replay_frame::cpu::scope("frame.overlay.prepare", Some("other"));
            let overlay_timer = Instant::now();
            if !document.icons.lazy.has_wake() {
                let ctx = ui.ctx().clone();
                document.icons.lazy.set_wake(viewer_runtime::worker_priority::waking(move || ctx.request_repaint()));
            }
            if std::mem::take(&mut self.overlay_routes_pending) && tables.counters().routes_by_key() {
                let pool = self.worker_pool().map_err(|why| format!("Native map workers: {why}"))?;
                let (fill, counters, movement, wake) = (Arc::clone(&tables), Arc::clone(&document.content.counters), document.content.movement.clone(), ui.ctx().clone());
                pool.try_submit(Box::new(move || viewer_runtime::redraws::apart(|| fill.counters().fill_routes(&counters, movement.as_deref()))), move || wake.request_repaint());
            }
            let overlay_state = live_of(&self.playhead_map).map(|side| {
                let products = side.products();
                (products.overlay_missed.clone(), products.overlay_loading.clone())
            });
            if let Some((missed, loading)) = overlay_state {
                self.overlay_missed = missed;
                self.overlay_loading.extend(loading);
            }
            let at = shown_head.unwrap_or_default();
            let head = head_rows::head(live_of(&self.playhead_map));
            let clock_speed = self
                .head_rows
                .map(head, frame, viewer_unit_reads::counter_move::MovementFrame::clock_speed)
                .flatten();
            let clock_lost = clock_speed.is_none()
                && (self.overlay.as_ref().is_some_and(Batch::moves)
                    || (self.overlay.is_some() && !self.overlay_clock_unavailable));
            let clock_recovered = clock_speed.is_some() && self.overlay_clock_unavailable;
            if clock_lost || clock_recovered {
                self.overlay_fraction = None;
            }
            let saved = self.saves_a_frame();
            if saved
                && (!self.overlay_shifts.is_empty()
                    || self.overlay_placed != Some(overlay_camera)
                    || self.overlay_placed_view != Some((pixels, ui_scale.to_bits())))
            {
                self.overlay_fraction = None;
            }
            let moved_now = !(clock_lost || clock_recovered || saved)
                && self.move_overlay(overlay_camera, pixels, ui_scale, at)?;
            let place_outcome = if moved_now {
                OverlayPlaced::Unchanged
            } else {
                self.place_overlay(&now, overlay_camera, pixels, ui_scale, &tables, &rows, left)
            };
            let placed_now = matches!(place_outcome, OverlayPlaced::Placed);
            let place_failed = match place_outcome {
                OverlayPlaced::Failed(why) => Some(why),
                _ => None,
            };
            if let Some(side) = live_of(&self.playhead_map) {
                match side.products().placements.as_deref().cloned() {
                    Some(names) if !self.placements.as_ref().is_some_and(|held| Arc::ptr_eq(held, &names)) => {
                        viewer_runtime::view_record::add("names window", "taken_from_layer", 1);
                        drop(self.placements.replace(names));
                    }
                    None if side.draws("names") => {
                        self.placements = None;
                    }
                    _ => {}
                }
            }
            if viewer_runtime::view_record::on() {
                let draws = live_of(&self.playhead_map).is_some_and(|side| side.draws("names"));
                viewer_runtime::view_record::set("names window", "layer_draws", u64::from(draws));
                viewer_runtime::view_record::set("names window", "generation", self.placements.as_ref().map_or(0, |held| held.generation));
                viewer_runtime::view_record::set("names window", "names", self.placements.as_ref().map_or(0, |held| held.country.len() as u64));
            }
            overlay_prepare_cpu.finish();
            laps.lap("overlay_prepare");
            drop(_other_cost);
            let _other_cost = replay_frame::cpu::scope("frame.overlay.draw", Some("other"));
            let overlay_draw_cpu = viewer_runtime::profiling::thread_span("overlay_draw");
            let owners_span = viewer_runtime::profiling::span("overlay_owners");
            let ours = Some(tables.ours());
            let head = head_rows::head(live_of(&self.playhead_map));
            let filtered = self.counter_owners.get(frame, head, &self.counter_filter);
            let layers = viewer_layer_core::scene_content::OverlayLayers::shown(
                &self.graphics,
                match ours {
                    Some(ours) => self.shown_counter_owners(filtered, ours),
                    None => filtered,
                },
            );
            if layers != self.overlay_layers {
                self.overlay_layers = layers;
                self.overlay_fraction = None;
            }
            drop(owners_span);
            let scene_span = viewer_runtime::profiling::span("overlay_scene_inputs");
            let (_, movement_hour) = self.hour_on_screen(document);
            if let Some(scene) = self.scene.as_mut() {
                scene
                    .apply(
                        &state.device,
                        &state.queue,
                        SceneInput::ShareProvinceSchedule(
                            self.counter_view.presentation.schedule(),
                        ),
                    )
                    .map_err(|error| error.to_string())?;
                let head = head_rows::head(live_of(&self.playhead_map));
                let selected = self.unit_selection.units().as_ref();
                if let Some(ours) = ours {
                    let mut arrows = |frame: &replay_frame::frame::DecodedFrame| {
                        crate::document::select_movement(scene, &state.device, &state.queue, &document.content, frame, ours, selected)
                    };
                    let moved = if selected.is_empty() { Some(arrows(now.frame)) } else { self.head_rows.arrows(head, frame, arrows) };
                    if let Some(Err(error)) = moved {
                        self.overlay_loading.push(format!("the selection's movement arrows: {error}"));
                    }
                }
                scene
                    .apply(
                        &state.device,
                        &state.queue,
                        SceneInput::AdvanceProvinces {
                            camera,
                            size: pixels,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                let presentation = &self.counter_view.presentation;
                {
                    let schedule = presentation
                        .schedule()
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    presentation
                        .movement()
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .begin_frame(
                            viewer_unit_reads::counter_move::MovementFrame {
                                time: animation_time,
                                hour: movement_hour,
                                head: at,
                                overlay_layout: self
                                    .prepared_overlay
                                    .as_ref()
                                    .and_then(|prepared| prepared.motion_layout().cloned()),
                                clock_speed,
                                speeds: document.content.counters.defines.movement_speeds,
                                steady: self.options.window_dump,
                            },
                            schedule.updates().filter_map(|(province, update)| {
                                (update == viewer_gpu::province_schedule::Update::InView)
                                    .then_some(province)
                            }),
                        );
                }
            }
            drop(scene_span);
            let mut painted_now = false;
            if let Some(why) = place_failed {
                if self.saves_a_frame() {
                    let reason = format!(
                        "A saved frame draws the overlay for the requested camera: {why}"
                    );
                    if self.refuse_capture(&reason, ui.ctx()) {
                        return Err(reason);
                    }
                }
            } else if self.overlay_fraction != Some(document.hour_fraction)
                && let Some(prepared) = self.prepared_overlay.as_mut()
            {
                let quiet = self.overlay_fraction.is_some_and(f32::is_nan)
                    && self.overlay_drawn_fraction == Some(document.hour_fraction);
                if quiet {
                    viewer_runtime::redraws::note("made.quiet", 1);
                    prepared
                        .draw_quiet(&now, overlay_camera, pixels, ui_scale, &self.overlay_layers, animation_time, self.overlay_pools)
                        .map_err(|error| error.to_string())?;
                    self.overlay_fraction = Some(document.hour_fraction);
                    self.overlay_drawn_fraction = Some(document.hour_fraction);
                } else {
                    viewer_runtime::redraws::note("made.draw", 1);
                    painted_now = true;
                    self.overlay_written += 1;
                    prepared.set_pressed(self.counter_view.pressed, self.counter_view.plate_hovered);
                    let mut rounds = viewer_native_gui::icon_windows::DumpFrames::default();
                    let mut installed = false;
                    let mut settles = 0;
                    let mut art_place_failed = false;
                    let art_moved = std::mem::take(&mut self.overlay_art_moved);
                    loop {
                        let (Some(rows), Some(tables)) = (self.overlay_frame.clone(), self.overlay_tables.clone()) else {
                            break;
                        };
                        let held = crate::document::Hour::new(&document.content, &rows, 0.0).with_air_rosters(Some(document.playback.air_rosters())).with_moved_hours(Arc::clone(&self.overlay_moved));
                        if std::mem::take(&mut installed)
                            && let OverlayPlaced::Failed(why) = self.place_overlay(
                                &held,
                                overlay_camera,
                                pixels,
                                ui_scale,
                                &tables,
                                &rows,
                                self.overlay_left,
                            )
                        {
                            if self.saves_a_frame() {
                                let reason = format!(
                                    "A saved frame draws the overlay for the requested camera: {why}"
                                );
                                if self.refuse_capture(&reason, ui.ctx()) {
                                    return Err(reason);
                                }
                            }
                            art_place_failed = true;
                            break;
                        }
                        let prepared = self.prepared_overlay.as_mut().expect("held above");
                        prepared
                            .draw_into(
                                &held,
                                overlay_camera,
                                pixels,
                                ui_scale,
                                &self.overlay_layers,
                                animation_time,
                                self.overlay.get_or_insert_with(Batch::default),
                                &mut self.counter_report,
                                &mut self.overlay_hits,
                            )
                            .map_err(|error| error.to_string())?;
                        if self.label_report != *prepared.label_report() {
                            self.label_report.clone_from(prepared.label_report());
                        }
                        if self.rendered.is_some() && !art_moved {
                            break;
                        }
                        let pending = prepared.retries();
                        if rounds.next(&pending) {
                            continue;
                        }
                        if !pending.is_empty() {
                            viewer_runtime::redraws::note("made.icons_pending", pending.iter().map(|retry| retry.requests as u64).sum());
                        }
                        if self.capture.as_ref().is_some_and(|capture| !capture.paced) {
                            document.icons.lazy.settle();
                        } else {
                            document.icons.lazy.settle_on_frame();
                            if !prepared.is_stale(&held) && (document.icons.counters.gui.loading_window_art() || document.icons.combat.gui.loading_window_art()) {
                                break;
                            }
                        }
                        if !prepared.is_stale(&held) && !document.icons.lazy.has_pending() {
                            break;
                        }
                        settles += 1;
                        let laid = match self.playhead_map.as_mut() {
                            Some(_) if settles > SETTLES => Err(format!("the art of hour {} was still arriving after {SETTLES} layouts", rows.hour)),
                            Some(side) => side.lay_overlay_again(),
                            None => Err("the map's side is not open".to_owned()),
                        };
                        let made = match laid {
                            Ok(made) => made,
                            Err(why) if self.saves_a_frame() => {
                                let why = format!("A saved frame draws the art of its own hour: the overlay of hour {} was not laid out again over the art that arrived while the frame drew it: {why}", rows.hour);
                                if self.refuse_capture(&why, ui.ctx()) {
                                    return Err(why);
                                }
                                break;
                            }
                            Err(_) => break,
                        };
                        self.install_overlay(made.into_inner(), &document.playback.replay().gaps);
                        installed = true;
                        rounds = viewer_native_gui::icon_windows::DumpFrames::default();
                    }
                    if !art_place_failed {
                        let withheld = self.finish_painting(
                            overlay_camera,
                            pixels,
                            ui_scale,
                            at,
                            document.hour_fraction,
                            clock_speed.is_none(),
                            |viewer| {
                                let (Some(rows), Some(tables)) = (viewer.overlay_frame.clone(), viewer.overlay_tables.clone()) else {
                                    return Ok(false);
                                };
                                let held = crate::document::Hour::new(&document.content, &rows, 0.0)
                                    .with_air_rosters(Some(document.playback.air_rosters()))
                                    .with_moved_hours(Arc::clone(&viewer.overlay_moved));
                                if !matches!(
                                    viewer.place_overlay(&held, overlay_camera, pixels, ui_scale, &tables, &rows, viewer.overlay_left),
                                    OverlayPlaced::Placed | OverlayPlaced::Unchanged
                                ) {
                                    return Ok(false);
                                }
                                let prepared = viewer.prepared_overlay.as_mut().expect("held above");
                                prepared.set_pressed(viewer.counter_view.pressed, viewer.counter_view.plate_hovered);
                                prepared
                                    .draw_into(
                                        &held,
                                        overlay_camera,
                                        pixels,
                                        ui_scale,
                                        &viewer.overlay_layers,
                                        animation_time,
                                        viewer.overlay.get_or_insert_with(Batch::default),
                                        &mut viewer.counter_report,
                                        &mut viewer.overlay_hits,
                                    )
                                    .map_err(|error| error.to_string())?;
                                if viewer.label_report != *prepared.label_report() {
                                    viewer.label_report.clone_from(prepared.label_report());
                                }
                                viewer.overlay_written += 1;
                                Ok(true)
                            },
                        )?;
                        if let Some(why) = withheld
                            && self.saves_a_frame()
                        {
                            let reason = format!(
                                "A saved frame draws the overlay for the requested camera: {}",
                                why.why()
                            );
                            if self.refuse_capture(&reason, ui.ctx()) {
                                return Err(reason);
                            }
                        }
                    }
                }
            }
            if let Some(scene) = self.scene.as_mut() {
                let movement_span = viewer_runtime::profiling::span("overlay_unit_movement");
                let presentation = &self.counter_view.presentation;
                presentation
                    .movement()
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .set_overlay_layout(
                        self.prepared_overlay
                            .as_ref()
                            .and_then(|prepared| prepared.motion_layout().cloned()),
                    );
                let positions: Option<Box<dyn Iterator<Item = (viewer_runtime::unit_handle::Unit, &viewer_unit_reads::counter_move::Position, f32, f32)> + '_>> =
                    live_of(&self.playhead_map).map(|side| Box::new(side.products().movement.iter().map(|(unit, place, scale, fade)| (*unit, place, *scale, *fade))) as _);
                if let Some(positions) = positions {
                    scene
                        .apply(
                            &state.device,
                            &state.queue,
                            SceneInput::SetUnitMovement {
                                positions,
                                history: presentation.movement(),
                            },
                        )
                        .map_err(|error| error.to_string())?;
                }
                drop(movement_span);
            }
            let rows = self.overlay_frame.clone().unwrap_or_else(|| Arc::clone(&rows));
            let tables = self.overlay_tables.clone().unwrap_or_else(|| Arc::clone(&tables));
            let now = crate::document::Hour::new(&document.content, &rows, 0.0).with_air_rosters(Some(document.playback.air_rosters())).with_moved_hours(Arc::clone(&self.overlay_moved));
            {
                let (identity, (made, kept)) = (tables.identity(), tables.counters().lookups());
                let (before_made, before_kept) = match self.overlay_lookups {
                    Some((held, made, kept)) if held == identity => (made, kept),
                    _ => (0, 0),
                };
                viewer_runtime::redraws::note("lookups.made", made.saturating_sub(before_made));
                viewer_runtime::redraws::note("lookups.kept", kept.saturating_sub(before_kept));
                viewer_runtime::view_record::add("overlay", "lookups_made", made.saturating_sub(before_made));
                viewer_runtime::view_record::add("overlay", "lookups_kept", kept.saturating_sub(before_kept));
                self.overlay_lookups = Some((identity, made, kept));
            }
            if let (Some((shown_branch, shown_hour)), Some((branch, drawn))) = (self.overlay_shown, self.layers_on_screen())
                && (shown_branch != branch || shown_hour != drawn)
            {
                viewer_runtime::redraws::note("overlay.hour_apart", 1);
                viewer_runtime::view_record::add("overlay", "hour_apart", 1);
                viewer_runtime::view_record::set("overlay", "hours_apart", u64::from(shown_hour.abs_diff(drawn)));
            }
            let running = self.counter_view.presentation.take_running();
            if running.any {
                viewer_runtime::redraws::note("asked.running", 1);
                if running.visible {
                    self.overlay_fraction = None;
                } else if running.windows.any() {
                    self.overlay_fraction = Some(f32::NAN);
                    self.overlay_pools = running.windows;
                } else {
                    viewer_runtime::redraws::note("asked.movement_only", 1);
                }
                ui.ctx().request_repaint();
            }
            let presented = (overlay_camera, pixels, ui_scale.to_bits(), self.overlay_written, at);
            self.looks = if interface
                && overlay_camera.free().is_some()
                && let Some(sprites) = self.overlay.as_ref()
            {
                if self.free_presented == Some(presented) && self.looks.is_some() {
                    viewer_runtime::redraws::note("free.placed_kept", 1);
                    self.looks.take()
                } else {
                    let _span = viewer_runtime::profiling::span("overlay_free_present");
                    viewer_runtime::redraws::note("free.placed", 1);
                    let ground = free_icons::Ground {
                        field: document.assets.height_field(),
                        sight: document.assets.terrain_sight(),
                    };
                    self.free_presented = Some(presented);
                    Some(free_icons::present(
                        &overlay_camera,
                        pixels,
                        ui_scale,
                        Some(&ground),
                        sprites,
                        at,
                        &mut self.free_overlay,
                    ))
                }
            } else {
                self.free_presented = None;
                None
            };
            self.hits_stamp += 1;
            if check_overlay() {
                let found = viewer_native_gui::sprites::overlay_check_failures();
                if !found.is_empty() {
                    return Err(format!(
                        "the overlay's resident GPU bytes differ from the same batch uploaded afresh: {}",
                        found.join("; ")
                    ));
                }
                self.overlay_looks = self.overlay_looks.wrapping_add(1);
                overlay_checks::note(&overlay_checks::LOOKED);
                let kept = (self.overlay_looks % 10 == 0 && self.overlay_shifts.is_empty())
                    .then_some("with its windows kept");
                if let Some(what) = painted_now
                    .then_some("as this frame painted it")
                    .or_else(|| placed_now.then_some("placed at this camera"))
                    .or_else(|| (self.overlay_checked != self.overlay_installed).then_some("as the step's maker laid it out"))
                    .or(kept)
                {
                    self.overlay_checked = self.overlay_installed;
                    let placed = self.overlay_placed.unwrap_or(overlay_camera);
                    self.check_overlay(what, &now, placed, pixels, ui_scale, animation_time)?;
                } else {
                    overlay_checks::note(&overlay_checks::NOTHING_NEW);
                }
            }
            self.head_rows.click_at(self.click_hour());
            self.head_rows.clickable(self.clickable(document));
            if *self.shown_mode()
                == viewer_map_modes::map_mode::MapMode::Stock(
                    viewer_map_modes::map_mode::Stock::StrategicNavy,
                )
            {
                let head = head_rows::head(live_of(&self.playhead_map));
                let mut fleet_added = false;
                response.context_menu(|ui| {
                    let choices = match ours {
                        Some(ours) => self.head_rows.clicks(head, frame, |frame| viewer_counters::naval_selection::fleet_choices(frame, ours)).unwrap_or_else(|| Err(std::io::Error::other("the fleets at the head are loading"))),
                        None => Err(std::io::Error::other("the viewing country is loading")),
                    };
                    match choices {
                        Err(why) => {
                            ui.label(why.to_string());
                        }
                        Ok(choices) => {
                            for (fleet, name) in choices {
                                if ui
                                    .selectable_label(
                                        self.fleet_selection.fleets().contains(&fleet),
                                        name,
                                    )
                                    .clicked()
                                {
                                    let shift = ui.input(|input| input.modifiers.shift);
                                    let was = self.fleet_selection.fleets().contains(&fleet);
                                    self.fleet_selection.click(fleet, shift);
                                    fleet_added |= !was && self.fleet_selection.fleets().contains(&fleet);
                                    if !shift {
                                        self.unit_selection.clear();
                                        self.ground_selection = None;
                                        self.air_selection.clear();
                                    }
                                    self.counter_view.selected =
                                        Arc::clone(self.unit_selection.units());
                                    self.counter_view.fleets_selected =
                                        Arc::clone(self.fleet_selection.fleets());
                                    ui.close();
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    }
                });
                if fleet_added {
                    self.selection_sounds(document, &[], true, ours, frame, viewer_native_gui::native_gui::pointer::LAST);
                }
            }
            let overlay_ms = overlay_timer.elapsed().as_secs_f64() * 1000.0;
            overlay_draw_cpu.finish();
            laps.lap("overlay_draw");
            drop(_other_cost);
            let _other_cost = replay_frame::cpu::scope("frame.interface", Some("other"));
            let _combat_cost = replay_frame::cpu::scope("frame.interface.combat", Some("other"));
            self.combat.set_playing(self.playing);
            let prepared = self.combat_prepared().map_err(|error| error.to_string())?;
            let combat_art = document.icons.lazy.version();
            let drawn = self
                .combat
                .draw(
                    &document.combat,
                    prepared,
                    extent,
                    true,
                    animation_time,
                    true,
                    combat_input
                        .as_ref()
                        .map(|input| input.filter(|at| picker_at(at).is_none())),
                )
                .map_err(|error| error.to_string())?;
            if self.combat.gui_active() {
                ui.ctx().request_repaint();
            }
            self.combat_painted = drawn.as_ref().map(combat::Drawn::footprint).unwrap_or_default();
            if let Some(drawn) = &drawn {
                self.interface_sounds.extend(
                    drawn
                        .sounds
                        .iter()
                        .map(|played| InterfaceSound::new("combat", played)),
                );
            }
            laps.lap("combat_draw");
            drop(_combat_cost);
            let _picker_cost = replay_frame::cpu::scope("frame.interface.picker", Some("other"));
            let picker_misses = document.icons.combat.gui.window_art_misses();
            let picker_drawn = match self.draw_picker(
                document,
                extent,
                true,
                animation_time,
                true,
                picker_input,
            ) {
                Some(Ok(drawn)) => Some(drawn),
                Some(Err(error)) => {
                    self.picker_error = Some(error);
                    None
                }
                None => None,
            }.filter(|_| picker_misses == document.icons.combat.gui.window_art_misses());
            if self.picker.pointing() {
                ui.ctx().request_repaint();
            }
            let confirmation_misses = document.icons.combat.gui.window_art_misses();
            let confirmation_drawn = self.draw_confirmation(document, extent, animation_time, confirmation_input)
                .filter(|_| confirmation_misses == document.icons.combat.gui.window_art_misses());
            if self.confirmation_pointing() {
                ui.ctx().request_repaint();
            }
            if let Some(drawn) = &confirmation_drawn {
                self.interface_sounds.extend(drawn.drawn.sounds.iter().map(|played| InterfaceSound::new("confirmation", played)));
            }
            if let Some(picker_drawn) = &picker_drawn {
                self.interface_sounds.extend(
                    picker_drawn
                        .drawn
                        .sounds
                        .iter()
                        .map(|played| InterfaceSound::new("picker", played)),
                );
                self.picker_error = None;
                self.picker_refused.clone_from(&picker_drawn.drawn.refused);
            }
            match (&picker_drawn, &mut self.picker_controls) {
                (Some(drawn), Some((drawn_at, controls))) => {
                    *drawn_at = extent;
                    controls.clone_from(&drawn.controls);
                }
                (Some(drawn), held) => *held = Some((extent, drawn.controls.clone())),
                (None, held) => *held = None,
            }
            let over_picker = point.is_some_and(|point| {
                picker_drawn
                    .as_ref()
                    .is_some_and(|drawn| drawn.control_at(point).is_some())
            });
            let mut clicks: Vec<(usize, Act)> = picker_drawn
                .iter()
                .flat_map(|drawn| drawn.clicked.iter())
                .map(|(control, order)| (*order, Act::Picker(control.clone(), false)))
                .collect();
            for (order, placed) in secondary_clicks {
                if let Some((path, effect)) = &placed.click {
                    let played = viewer_native_gui::native_gui::pointer::Played {
                        effect: effect.clone(),
                        click: Some(path.clone()),
                        order,
                    };
                    self.interface_sounds
                        .push(InterfaceSound::new("picker", &played));
                }
                clicks.push((order, Act::Picker(placed.control, true)));
            }
            if let Some(window) = &drawn {
                clicks.extend(
                    window
                        .clicks
                        .iter()
                        .enumerate()
                        .map(|(at, (_, _, order))| (*order, Act::Combat(at))),
                );
            }
            clicks.extend(
                off_picker_clicks
                    .iter()
                    .map(|&order| (order, Act::Dismiss)),
            );
            clicks.extend(confirmation_drawn.iter().flat_map(|drawn| drawn.clicked.iter()).map(|&(control, order)| (order, Act::Confirmation(control))));
            clicks.extend(air_order_releases.iter().map(|&(order, point, under)| (order, Act::AirOrder(point, under))));
            if !started && !map_pinged {
                clicks.extend(
                    counter_clicks
                        .iter()
                        .map(|&(unit, order)| (order, Act::Counter(unit))),
                );
                if !self.combat.dragging && (!free_mode || icon_click) {
                    clicks.extend(
                        bare_clicks
                            .iter()
                            .map(|&(point, order)| (order, Act::Map(point))),
                    );
                }
                if !self.combat.dragging && !free_mode {
                    clicks.extend(box_releases.iter().map(|&(from, to, order)| (order, Act::Box(from, to))));
                }
            }
            clicks.sort_by_key(|(order, _)| *order);
            self.answer_held(document, frame, ui.ctx());
            let mut closed = false;
            for (order, click) in clicks {
                use viewer_combat::picker::Control;
                let modifiers = modifiers_at(order);
                match click {
                    Act::Dismiss => {
                        if self.picker.open != viewer_combat::picker::Open::None {
                            self.picker.open = viewer_combat::picker::Open::None;
                            closed = true;
                        }
                    }
                    Act::Counter(unit) => {
                        let hit = self
                            .counter_report
                            .unit_hits
                            .iter()
                            .position(|hit| hit.units().next() == Some(unit));
                        if hit.is_some() {
                            let before = Arc::clone(self.unit_selection.units());
                            self.click_units(hit, modifiers.shift, ui.ctx());
                            self.selection_added(document, &before, ours, frame, order);
                        }
                    }
                    Act::Box(from, to) => self.select_box(document, from, to, ours, frame, modifiers, ui.ctx(), order),
                    Act::Map(point) => {
                        if self.map_hit(point).is_none() {
                            match self.model_at(document, point, true) {
                                Ok(Some(picked)) => {
                                    self.click_model(document, picked, ours, frame, modifiers.shift, ui.ctx(), order);
                                    continue;
                                }
                                Err(why) => {
                                    self.selection_input.refuse(why);
                                    continue;
                                }
                                Ok(None) => {}
                            }
                        }
                        if modifiers.ctrl
                            && self.map_hit(point).is_none()
                            && let Some(province) = pick(&camera, point.map(|value| value * ui_scale), pixels, &document.map, document.assets.heightmap_texels())?
                            && self.ctrl_click(document, frame, ours, province, modifiers.shift, ui.ctx())
                        {
                            ui.ctx().request_repaint();
                            continue;
                        }
                        let ground_blocked = modifiers.shift
                            && !(self.unit_selection.units().is_empty() && self.fleet_selection.fleets().is_empty() && self.air_selection.wings().is_empty());
                        if matches!(self.shown_mode(), viewer_map_modes::map_mode::MapMode::Stock(viewer_map_modes::map_mode::Stock::Diplomacy))
                            && !ground_blocked
                            && self.map_hit(point).is_none()
                            && let Some(province) = pick(
                                &camera,
                                point.map(|value| value * ui_scale),
                                pixels,
                                &document.map,
                                document.assets.heightmap_texels(),
                            )?
                            && let Some(country) = self.counter_report.perspective.province_controller(province)
                            && self.diplomatic_selected != Some(country)
                        {
                            self.diplomatic_selected = Some(country);
                            ui.ctx().request_repaint();
                        }
                        match self.map_hit(point) {
                            Some(MapHit::NavalMission(index))
                                if self.counter_report.naval_mission_hits[index].mission != 0 =>
                            {
                                let Some(viewer) = self.viewing(ours, "the naval mission's members") else { continue };
                                let hit = &self.counter_report.naval_mission_hits[index];
                                let click = held_clicks::Click::NavalMission { viewer, region: hit.region, mission: hit.mission, rect: hit.rect, layer: hit.layer, order };
                                self.click(document, click, frame, ui.ctx());
                            }
                            Some(MapHit::Base(index))
                                if self.counter_report.air_grid_hits.contains_key(&index) =>
                            {
                                if let Some(members) =
                                    self.counter_report.air_grid_hits[&index].members.clone()
                                {
                                    let shift = modifiers.shift;
                                    self.click(document, held_clicks::Click::AirGrid { members, shift }, frame, ui.ctx());
                                }
                            }
                            Some(MapHit::Base(index))
                                if self.counter_report.air_base_hits.contains_key(&index) =>
                            {
                                let (province, kind) = self.counter_report.air_base_hits[&index];
                                let shift = modifiers.shift;
                                let Some(viewer) = self.viewing(ours, "the air base's target") else { continue };
                                self.click(document, held_clicks::Click::AirBase { viewer, province, kind, shift }, frame, ui.ctx());
                            }
                            Some(MapHit::Base(index))
                                if self.counter_report.naval_base_hits.contains_key(&index) =>
                            {
                                if modifiers.shift || modifiers.ctrl {
                                    continue;
                                }
                                let province = self.counter_report.naval_base_hits[&index];
                                let Some(viewer) = self.viewing(ours, "the naval base's task forces") else { continue };
                                let base = &self.counter_report.base_hits[index];
                                let click = held_clicks::Click::NavalBase { viewer, province, rect: base.rect, layer: base.layer };
                                self.click(document, click, frame, ui.ctx());
                            }
                            Some(MapHit::AirMission(index)) => {
                                use viewer_counter_kit::AirMissionClick;
                                let hit = &self.counter_report.air_mission_hits[index];
                                let mission = match hit.part.click() {
                                    AirMissionClick::SelectMission(mission) => mission,
                                    AirMissionClick::Nothing => continue,
                                    AirMissionClick::OpenRegion => {
                                        self.combat.open(viewer_engagements::combat_selection::Selection::AirRegion(hit.region));
                                        continue;
                                    }
                                };
                                let region = hit.region;
                                let shift = modifiers.shift;
                                let Some(viewer) = self.viewing(ours, "the air mission's members") else { continue };
                                self.click(document, held_clicks::Click::AirMission { viewer, region, mission, shift }, frame, ui.ctx());
                            }
                            Some(MapHit::Combat(index)) => self
                                .combat
                                .open(self.counter_report.combat_hits[index].selection),
                            Some(MapHit::Raid) => {
                                let Some(hit) = self.raid_hit(point).cloned() else {
                                    continue;
                                };
                                let viewer_raids::map::raids::tooltips::Key::Offer(targets) = &hit.key else {
                                    continue;
                                };
                                if !raid_details::opens(self.counter_view.raid_details.as_ref().map(|(province, _)| *province), hit.province) {
                                    self.counter_view.raid_details = None;
                                } else {
                                    let Some(viewer) = self.viewing(ours, "the raid targets' details") else { continue };
                                    let click = held_clicks::Click::Raid { viewer, province: hit.province, targets: Arc::clone(targets) };
                                    self.click(document, click, frame, ui.ctx());
                                }
                                ui.ctx().request_repaint();
                            }
                            None if ground_blocked => {}
                            None => {
                                self.click_units(None, modifiers.shift, ui.ctx());
                                let Some(province) = pick(
                                    &camera,
                                    point.map(|v| v * ui_scale),
                                    pixels,
                                    &document.map,
                                    document.assets.heightmap_texels(),
                                )?
                                else {
                                    continue;
                                };
                                self.select_province(document, frame, province, ui.ctx());
                            }
                            _ => {}
                        }
                    }
                    Act::Picker(Control::Entry(_) | Control::SlotEntry(_) | Control::Default, _)
                        if closed => {}
                    Act::Picker(control, secondary) => {
                        self.picker_click(document, &control, secondary);
                    }
                    Act::Combat(at) => {
                        if let Some(window) = &drawn {
                            let (path, rect, _) = &window.clicks[at];
                            self.combat.set_modifiers(modifiers.shift, modifiers.ctrl);
                            self.combat.click(window, path, *rect);
                        }
                    }
                    Act::Confirmation(control) => self.confirmation_click(control),
                    Act::AirOrder(point, under) => {
                        let shift = modifiers.shift;
                        let pick_at = |point: [f32; 2]| {
                            pick(&camera, point.map(|value| value * ui_scale), pixels, &document.map, document.assets.heightmap_texels()).ok().flatten()
                        };
                        self.air_order(document, point, under, order, shift, pick_at, ui.ctx());
                    }
                }
            }
            self.interface_sounds[frame_sounds..].sort_by_key(|sound| sound.order);
            let mut drawn = drawn;
            if let Some(picker_drawn) = &picker_drawn {
                drawn
                    .get_or_insert_with(viewer_combat::Drawn::default)
                    .add_hovers(&picker_drawn.drawn);
            }
            if let Some(confirmation) = &confirmation_drawn {
                drawn.get_or_insert_with(viewer_combat::Drawn::default).add_hovers(&confirmation.drawn);
            }
            laps.lap("picker");
            drop(_picker_cost);
            let over_confirmation = point.is_some_and(|point| self.air_orders.over(point));
            let tooltip_point = point.filter(|_| !over_combat && !over_picker && !over_confirmation);
            let map_hit = self.map_tooltip(document, tooltip_point, extent, ui.ctx());
            self.combat_rows.end_frame();
            self.head_rows.end_frame(head_rows::head(live_of(&self.playhead_map)));
            let tooltip_counter = tooltip_point.and_then(|point| match self.map_hit(point)? {
                MapHit::Unit(index) => self.counter_report.unit_hits[index].units().next(),
                _ => None,
            });
            laps.lap("map_tooltip");
            self.hover = hover_report::describe(
                point.and_then(|point| picker_drawn.as_ref()?.control_at(point)),
                self.counter_view.hovered,
            );
            self.tooltip = None;
            self.tooltip_delay_ms = document
                .icons
                .combat
                .tooltip_defines
                .as_ref()
                .ok()
                .map(|defines| defines.timing[0] * 1000.0);
            let mut tooltip_refused = map_hit
                .as_ref()
                .map(|(text, _, _)| text.refused.clone())
                .unwrap_or_default();
            if let Some((text, _, province)) = &map_hit
                && (!text.title.is_empty() || !text.body.is_empty())
            {
                self.map_tooltip_target = province
                    .map(highlight::TooltipTarget::Province)
                    .or(tooltip_counter.map(highlight::TooltipTarget::Counter));
            }
            self.shown_tooltip = None;
            let mut additions = Batch::default();
            let tooltip_misses = document.icons.counters.gui.window_art_misses();
            let tooltip_time = Instant::now();
            if let Some((text, anchor)) = self.combat.tooltip(
                drawn.as_ref(),
                document.icons.combat.tooltip_defines.as_ref().ok(),
                point,
                map_hit.as_ref().map(|(text, rect, province)| {
                    (
                        text.title.as_str(),
                        Some(text.body.as_str()),
                        *rect,
                        *province,
                    )
                }),
                tooltip_time,
            ) && self.graphics.tooltips
            {
                self.tooltip = Some(
                    map_hit
                        .as_ref()
                        .filter(|(tip, _, _)| text.starts_with(tip.title.as_str()))
                        .and_then(|(_, _, province)| *province)
                        .map_or_else(
                            || "shown".to_owned(),
                            |province| format!("province:{province}"),
                        ),
                );
                let refused = match self.prepared_overlay.as_ref() {
                    Some(prepared) => prepared.draw_tooltip(&now, &mut additions, text, anchor, extent, animation_time).map_err(|error| error.to_string())?,
                    None => [("the overlay is not laid out yet".to_owned(), 1)].into_iter().collect(),
                };
                self.shown_tooltip = (refused.is_empty()
                    && interface
                    && self.combat.tooltip_from_map())
                    .then(|| self.map_tooltip_target.clone())
                    .flatten();
                for (reason, count) in refused {
                    *tooltip_refused.entry(reason).or_default() += count;
                }
            }
            if self
                .capture
                .as_ref()
                .is_some_and(|capture| capture.remaining == 0)
            {
                let inventory = |inputs: &hover_rows::Inputs, report: &CounterReport| {
                    let key = inputs.key(hover_rows::What::Capture, [report.land_hovers.as_ptr() as usize ^ report.land_hovers.len(), report.task_force_hovers.as_ptr() as usize ^ report.task_force_port_hovers.as_ptr() as usize]);
                    let task_forces: Vec<_> = report.task_force_hovers.iter().filter(|hover| hover.supported).cloned().collect();
                    let ports: Vec<_> = report.task_force_port_hovers.iter().filter(|hover| hover.supported && hover.has_tooltip()).cloned().collect();
                    let (inputs, land) = (inputs.clone(), report.land_hovers.clone());
                    (key, move |_: &hover_rows::Caches| inputs.capture(land.clone(), task_forces.clone(), ports.clone()))
                };
                let made = match (self.rows_head(), self.shown_vision()) {
                    (Some(views), Ok(vision)) => {
                        let (key, job) = inventory(&self.hover_inputs(document, &vision, ui.ctx()), &self.counter_report);
                        self.hover_rows.wait(hover_rows::Family::Capture, &views, key, job)
                    }
                    _ => None,
                };
                for (reason, &count) in made.iter().flat_map(|text| text.refused.iter()) {
                    let current = tooltip_refused.entry(reason.clone()).or_default();
                    *current = (*current).max(count);
                }
            }
            self.hover_rows.end_frame(self.capture.is_some());
            for (reason, count) in self.hover_rows.loading().into_iter().chain(self.combat_rows.loading()) {
                *tooltip_refused.entry(reason).or_default() += count;
            }
            if !self.overlay_missed.is_empty() {
                self.overlay_loading.push(format!("the overlay: loading: {:?}", self.overlay_missed));
            }
            for reason in self.overlay_loading.drain(..) {
                *tooltip_refused.entry(reason).or_default() += 1;
            }
            self.tooltip_refused = tooltip_refused;
            if let Some(delay) = self.combat.tooltip_repaint_after(
                document.icons.combat.tooltip_defines.as_ref().ok(),
                tooltip_time,
            ) {
                ui.ctx().request_repaint_after(delay);
            }
            let combat_out = match &drawn {
                None => Some("its quads were not drawn into this frame".to_owned()),
                Some(drawn) => drawn.missing(),
            };
            if tooltip_misses != document.icons.counters.gui.window_art_misses() {
                additions.quads.clear();
            }
            if let Some(drawn) = drawn {
                additions.quads.extend(drawn.batch.quads);
            }
            if let Some(picker_drawn) = picker_drawn {
                additions.quads.extend(picker_drawn.drawn.batch.quads);
            }
            if let Some(confirmation) = confirmation_drawn {
                additions.quads.extend(confirmation.drawn.batch.quads);
            }
            laps.lap("combat_hover");
            (overlay_ms, additions, combat_out, Some(combat_art))
        };
        let held = self.overlay_frame.clone().zip(self.overlay_tables.clone()).or(held);
        let now = held.as_ref().map(|(rows, _)| crate::document::Hour::new(&document.content, rows, 0.0).with_air_rosters(Some(document.playback.air_rosters())).with_moved_hours(Arc::clone(&self.overlay_moved)));
        let _other_cost = replay_frame::cpu::scope("frame.scene-inputs", Some("other"));
        let warnings = if build_warnings {
            self.collect_warnings(document)
        } else {
            Vec::new()
        };
        laps.lap("diagnostics");
        let render_cpu = viewer_runtime::profiling::thread_span("render");
        self.update_faction_highlight(
            document,
            &state.device,
            &state.queue,
            ui.ctx(),
            point.filter(|_| !over_combat),
        )?;
        let radar_at = point
            .filter(|_| !over_combat)
            .and_then(|point| radar_province(self.map_hit(point), &self.counter_report))
            .and_then(|province| Some((province, self.counter_report.perspective.province_controller(province)?)));
        let base_at = point.filter(|_| !over_combat).and_then(|point| {
            let MapHit::Base(index) = self.map_hit(point)? else {
                return None;
            };
            self.counter_report.air_base_hits.get(&index).copied()
        });
        let state_of = |province: u32| {
            let provinces = now.as_ref()?.frame.complete::<replay_data::rows::tables::Provinces>().ok()?;
            provinces.binary_search_by_key(&province, |row| row.id).ok().map(|at| provinces[at].state)
        };
        let hovered_radar = radar_at.and_then(|(province, controller)| Some((state_of(province)?, controller)));
        let hovered_air_base = base_at.and_then(|(province, kind)| Some((state_of(province)?, kind)));
        let hovered_air_carriers = point.filter(|_| !over_combat).and_then(|point| {
            let MapHit::Unit(index) = self.map_hit(point)? else {
                return None;
            };
            let hit = &self.counter_report.unit_hits[index];
            (!hit.port
                && hit.kind == viewer_native_gui::counter_hovers::unit_hits::Kind::TaskForce)
                .then(|| Arc::clone(&hit.members))
        });
        let (_, shown_hour) = self.hour_on_screen(document);
        let target_light = self.frame_day_night(shown_hour, &document.game_speeds);
        let day_night = self.day_night.update(
            shown_hour,
            target_light,
            f64::from(animation_time),
            1.0 / self.speed,
        );
        if day_night.blending_to(target_light) && self.capture.is_none() {
            request_animation_frame(ui.ctx(), map_timer.elapsed());
        }
        let layered_radar = self.playhead_map.as_ref().and_then(|side| side.products().radar.clone());
        let layered = self.layered_orders();
        let scene = self.scene.as_mut().unwrap();
        scene.track_paused_motion((paused_motion == PausedMotion::Live).then_some(shown_hour));
        let (device, queue) = (&state.device, &state.queue);
        if let Some(field) = layered_radar.as_deref().or(self.prepared_overlay.as_ref().map(|prepared| &*prepared.radar)) {
            scene
                .apply(device, queue, SceneInput::SetRadar { field, shown: self.picker.radar, hovered: hovered_radar })
                .map_err(|e| e.to_string())?;
        }
        let (plan, raids, routes) = layered.as_ref().map_or((None, None, None), |layered| (layered.plan.clone(), layered.raids.clone(), layered.routes));
        let mut air = None;
        if let Some(prepared) = &self.prepared_overlay {
            let carrier_ranges = hovered_air_carriers
                .as_deref()
                .map(|forces| prepared.air_ranges.carrier_ranges(forces));
            let ranges = carrier_ranges
                .as_ref()
                .unwrap_or_else(|| prepared.air_ranges.shown(hovered_air_base));
            scene
                .apply(device, queue, SceneInput::SetAirRanges(&ranges.ranges))
                .map_err(|e| e.to_string())?;
            scene
                .apply(
                    device,
                    queue,
                    SceneInput::SetAirTransfers(&prepared.air_orders.transfers.progress),
                )
                .map_err(|e| e.to_string())?;
            scene
                .apply(
                    device,
                    queue,
                    SceneInput::SetAirAssignment(prepared.province_stripes()),
                )
                .map_err(|e| e.to_string())?;
            scene
                .apply(
                    device,
                    queue,
                    SceneInput::SetStrategicStyles(&prepared.naval_selection.styles),
                )
                .map_err(|e| e.to_string())?;
            air = Some((Arc::clone(&prepared.air_orders), ranges.highlighted_bases.clone()));
        }
        if layered.is_some() || plan.is_some() {
            let key = OrderKey { plan: order_at(plan.as_ref()), raids: order_at(raids.as_ref()), routes, air: air.as_ref().map(|(orders, bases)| (order_at(Some(orders)), bases.clone())) };
            if self.uploaded.order_key.as_ref() != Some(&key) {
                let air_field = match (&air, &key.air) {
                    (Some((orders, bases)), Some(held)) => {
                        if self.uploaded.air_field.as_ref().is_none_or(|(had, _)| had != held) {
                            self.uploaded.air_field = Some((held.clone(), Arc::new(orders.highlighted(bases))));
                        }
                        self.uploaded.air_field.as_ref().map(|(_, field)| Arc::clone(field))
                    }
                    _ => None,
                };
                let routes_field = routes.and(live_of(&self.playhead_map)).and_then(|side| side.products().routes.as_deref()).map(|(_, routes)| &routes.field);
                scene
                    .apply(device, queue, SceneInput::SetOrders { assets: &document.orders, parts: [plan.as_deref(), raids.as_deref(), routes_field, air_field.as_deref()] })
                    .map_err(|error| error.to_string())?;
                self.uploaded.order_key = Some(key);
            }
        }
        scene
            .apply(
                &state.device,
                &state.queue,
                SceneInput::SetGraphics(self.graphics),
            )
            .map_err(|error| error.to_string())?;
        let library = document.unit_models.library();
        let aircraft = document.aircraft.library();
        let flying = self.flying.take();
        let player = match self.camera_mode {
            CameraMode::Walk if free_mode => self.walk.player(&library),
            CameraMode::Fly => Self::fly_model(flying, &aircraft),
            _ => None,
        };
        scene
            .apply(&state.device, &state.queue, SceneInput::SetPlayer(player))
            .map_err(|error| error.to_string())?;
        drop((library, aircraft));
        scene
            .apply(
                &state.device,
                &state.queue,
                SceneInput::SetVisibility(self.counter_view.visibility),
            )
            .map_err(|error| error.to_string())?;
        let (head, selected) = (head_rows::head(live_of(&self.playhead_map)), self.unit_selection.units().as_ref());
        if let (Some((_, tables)), Some(now)) = (&held, &now) {
            let ours = tables.ours();
            let mut arrows = |frame: &replay_frame::frame::DecodedFrame| {
                crate::document::select_movement(scene, &state.device, &state.queue, &document.content, frame, ours, selected)
            };
            let moved = if selected.is_empty() { Some(arrows(now.frame)) } else { self.head_rows.arrows(head, frame, arrows) };
            if let Some(Err(error)) = moved {
                self.overlay_loading.push(format!("the selection's movement arrows: {error}"));
            }
        }
        scene
            .apply(
                &state.device,
                &state.queue,
                SceneInput::LabelFrame(u64::from(self.profile_frame)),
            )
            .map_err(|error| error.to_string())?;
        scene
            .apply(
                &state.device,
                &state.queue,
                SceneInput::CutProvinceFrame(ambient_time),
            )
            .map_err(|error| error.to_string())?;
        let overlay = if self.looks.is_some() {
            &mut self.free_overlay
        } else {
            self.overlay.get_or_insert_with(Batch::default)
        };
        let laid_out = overlay.quads.len();
        overlay.quads.extend(additions.quads);
        let timed = !self.dashboard_map;
        if timed && let Some(frame) = crate::perf_log::frame_id() {
            scene.time_frame(frame);
        }
        let rendered = scene
            .render(
                &state.device,
                &state.queue,
                Request {
                    size: pixels,
                    camera,
                    day_night,
                    animation_time: ambient_time,
                    map_time: ambient_time,
                    transition_time: Some(animation_time),
                    postfx: self.postfx,
                    no_water: false,
                    no_shadows: false,
                    overlay: Overlay {
                        names: self.placements.as_deref(),
                        layer_alpha: name_fade.0,
                        strategic_names,
                        sprites: interface.then_some(&*overlay),
                        ui_scale,
                        interface,
                        country_names: interface || flying_names,
                        ground: camera.free().map(|_| free_icons::Ground {
                            field: document.assets.height_field(),
                            sight: document.assets.terrain_sight(),
                        }),
                    },
                    no_trees: false,
                },
            );
        overlay.quads.truncate(laid_out);
        if timed {
            crate::perf_log::gpu_times(scene.gpu_frame_times(&state.device));
        }
        rendered.map_err(|e| e.to_string())?;
        schedule_after_render(
            ui.ctx(),
            map_timer.elapsed(),
            scene.needs_frame(),
            paused_motion,
            || {
                let view = viewer_models::motion_view::MotionView::new(&camera, pixels);
                let mut delay = scene.next_arrival(&camera, pixels, &view, ambient_time);
                if sailing
                    && camera.height - viewer_models::WATER_HEIGHT < camera.defines.map_objects_cutoff
                    && let Some(prepared) = &posed
                {
                    let heights = document.content.heights();
                    delay = delay.min(prepared.next_convoy_arrival(
                        &document.unit_models,
                        &view,
                        &|x, z| heights.surface_height(x, z),
                    ));
                }
                delay
            },
        );
        drop(_other_cost);
        let _other_cost = replay_frame::cpu::scope("frame.after-render", Some("other"));
        let shown_stamp = presentation::shown_stamp(profiling(), || {
            (document.playback.branch(), document.playback.hour())
        });
        let playback_stamp =
            profiling().then(|| self.profile_playback.stamp(self.playhead, self.playing));
        let silent = self.capture.is_some()
            && (self.campaign_tab || std::env::var_os("BATTLEPLAN_VIEWER_AUDIO_LOG").or_else(|| std::env::var_os("HOI4_VIEWER_AUDIO_LOG")).is_some());
        if silent {
            viewer_audio::System::silent();
        }
        if (self.capture.is_none() || silent) && self.audio_focus {
            let _other_cost = replay_frame::cpu::scope("frame.audio", Some("other"));
            present_audio(
                &mut self.audio,
                ui.ctx(),
                document,
                scene,
                &camera,
                paused_motion != PausedMotion::Held,
                AudioInputs {
                    volumes: self.volumes,
                    muted: self.muted,
                    reset: std::mem::take(&mut self.audio_reset),
                    music: self.playhead_map.as_ref().and_then(|side| side.products().music.as_deref().cloned()).or_else(|| self.music.clone()),
                    battles: self.battles.clone(),
                    interface: std::mem::take(&mut self.interface_sounds),
                    interface_key: &mut self.interface_key,
                },
            );
        } else if !self.audio_focus
            && let Some(audio) = &mut self.audio
        {
            self.interface_sounds.clear();
            audio.release();
        }
        if timed {
            crate::screen_count::readout::count(&state.device, &state.queue, scene);
        }
        let [animate, overlay_upload, encode, submit] = scene.render_laps();
        if self.scene_target.stale(scene.generation()) {
            let view = scene.view().ok_or("Map target was not created")?;
            if let Some(host) = crate::renderer_host::local(state) {
                let target = crate::renderer_host::Target {
                    view: view.clone(),
                    filter: PRESENTATION_FILTER,
                };
                if let Some(texture) = self.scene_target.texture {
                    host.rebind(texture, target);
                } else {
                    self.scene_target.texture = Some(host.register(target));
                }
            } else {
                let mut egui_renderer = state.renderer.write();
                if let Some(texture) = self.scene_target.texture {
                    egui_renderer.update_egui_texture_from_wgpu_texture(
                        &state.device,
                        view,
                        PRESENTATION_FILTER,
                        texture,
                    );
                } else {
                    self.scene_target.texture = Some(egui_renderer.register_native_texture(
                        &state.device,
                        view,
                        PRESENTATION_FILTER,
                    ));
                }
            }
            self.scene_target.generation = scene.generation();
        }
        render_cpu.finish();
        laps.lap("render");
        if profiling() {
            if let Some(cpu) = laps.frame_cpu.take() {
                cpu.finish();
            }
            let profile = format!(
                "PROFILE frame {} fraction {} height {} total {:.6} overlay {:.6} spline {:.6} hour {} frame_total {:.3} outside {:.3} scene {:.3}/{:.3}/{:.3}/{:.3} gpu {:.3} laps {} detail {} spans {} thread {} cpu_hz {} {} {} perf_frame={} commands={:?}",
                self.profile_frame,
                document.hour_fraction,
                camera.height,
                map_timer.elapsed().as_secs_f64() * 1000.0,
                overlay_ms,
                viewer_unit_reads::counter_move::take_profile(),
                document.playback.hour(),
                laps.total(),
                laps.outside,
                animate,
                overlay_upload,
                encode,
                submit,
                gpu_wait(&state.device),
                laps.line(),
                self.scene
                    .as_ref()
                    .map_or_else(String::new, |scene| scene.detail_laps()),
                viewer_runtime::profiling::take_spans(),
                viewer_runtime::profiling::take_thread_spans(),
                viewer_runtime::profiling::cpu_clock_hz()
                    .map_or_else(|| "unavailable".to_owned(), |hz| hz.to_string()),
                playback_stamp.expect("PROFILE frame sampled its playback stamp"),
                shown_stamp.expect("PROFILE frame sampled its shown stamp"),
                crate::perf_log::frame_id().map_or_else(|| "unavailable".to_owned(), |id| id.to_string()),
                shader_pipeline::gpu::stats::take(),
            );
            crate::perf_log::profile_frame(&profile);
            profile_print(profile);
        }
        let (shown_branch, shown_at) = self.hour_on_screen(document);
        if self.capture.as_ref().is_some_and(|capture| !capture.paced) {
            document.icons.lazy.settle();
            let (stale, waiting) = match (&held, &now) {
                (Some((rows, tables)), Some(now)) => {
                    let stale = self.prepared_overlay.as_ref().map(|prepared| prepared.is_stale(now));
                    let raids = &tables.raids().report;
                    let waiting = if let Some(missed) = missed_tables(&self.overlay_missed) {
                        Some(format!("its overlay was laid out without {missed}"))
                    } else if rows.hour != shown_at {
                        Some(format!("its overlay stood at hour {} while the map showed hour {shown_at}", rows.hour))
                    } else if let Some(why) = self.saved_overlay_camera_waiting(interface, overlay_camera, pixels, ui_scale) {
                        Some(why)
                    } else if raids.pending {
                        Some("its raid icons were still being built".to_owned())
                    } else if !raids.cycle_complete() {
                        Some(format!("its raid offers had evaluated {} of {} targets", raids.cycle.0, raids.cycle.1))
                    } else {
                        None
                    };
                    (stale, waiting)
                }
                _ => (Some(false), None),
            };
            let hour = held.as_ref().map_or(shown_at, |(rows, _)| rows.hour);
            let picture = self.picture_ready();
            let art_moved = combat_art.is_some_and(|art| art != document.icons.lazy.version()) || document.icons.lazy.has_pending();
            let (left_out, art_retry) = combat_left_out(interface, combat_out, art_moved);
            let stale = if art_retry { Some(true) } else { stale };
            let combat = match self.saved_combat(picture, left_out.as_deref()) {
                Ok(there) => there,
                Err(why) => {
                    self.refused_frame(format!("A saved frame draws the combat window of its own hour {hour}: {why}"), ui.ctx())?;
                    true
                }
            };
            let waiting = waiting
                .or_else(|| document.icons.lazy.has_pending().then(|| "its window art, flags or portraits were still decoding".to_owned()))
                .or_else(|| {
                    (!combat).then(|| match &self.combat_rows.pending {
                        Some(selector) => format!("the combat it names ({selector:?}) was not found in its rows yet"),
                        None => "its combat window's read of the hour was still loading".to_owned(),
                    })
                })
                .or_else(|| art_retry.then(|| "its combat window was drawn again for art that arrived".to_owned()));
            self.hold_saved_frame(hour, stale, waiting, ui.ctx())?;
        }
        self.save_capture(state, document, camera, pixels, &warnings, (shown_branch, shown_at), ui.ctx());
        let mut warnings = warnings;
        if let Some(audio) = self.audio.as_ref().filter(|audio| audio.owns()) {
            let status = audio.status();
            if let Some(Err(error)) = &status.device {
                warnings.push(format!("Map audio is silent: {error}."));
            }
            warnings.extend(
                status
                    .notes
                    .iter()
                    .map(|note| format!("Map audio: {note}.")),
            );
            if let Some(scene) = &self.scene {
                warnings.extend(
                    scene.sound_refusals.iter().map(|(refusal, actors)| {
                        format!("Map audio: {refusal} (actors: {actors}).")
                    }),
                );
            }
            if let Some(source) = &document.audio
                && let Some(why) = viewer_audio::battle::silence(&source.content)
            {
                warnings.push(format!("Map battle ambience: {why}."));
            }
            if let Some(battles) = &self.battles
                && !battles.refusals.is_empty()
            {
                warnings.push(format!(
                    "Map battle ambience: {}.",
                    battles.refusals.join("; ")
                ));
            }
            let music = match &self.playhead_map {
                Some(side) => side.products().music.as_deref().cloned(),
                None => self.music.clone(),
            };
            if let Some(music) = &music
                && !music.refusals.is_empty()
            {
                warnings.push(format!(
                    "Map music keeps the base chance of songs whose conditions the recording cannot answer: {}.",
                    music.refusals.join("; ")
                ));
            }
        }
        self.map_warnings = warnings;
        self.profile_frame += 1;
        let texture = self
            .scene_target
            .texture
            .ok_or("Map target was not registered")?;
        ui.painter().image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        if let Some((drag, stroke)) = box_stroke {
            ui.painter().with_clip_rect(rect).rect_stroke(drag, 0.0, stroke, egui::StrokeKind::Middle);
        }
        if self.combat.loading_title() { ui.ctx().request_repaint_after(Duration::from_millis(50)); }
        let loading_art = if self.combat.loading_art() {
            Some("Loading combat window art...")
        } else if self.combat.is_loading() {
            Some("Reading the combat window...")
        } else if document.icons.counters.gui.loading_window_art() {
            Some("Loading map counter art...")
        } else if window_misses != document.icons.combat.gui.window_art_misses() {
            Some("Loading map window art...")
        } else {
            None
        };
        let software = is_software(&state.adapter);
        let diagnostics = self.diagnostics && self.embedded.is_some() && !self.dashboard_map;
        let partial = document.playback.replay().is_partial()
            && self.embedded.is_some()
            && !self.dashboard_map;
        let scoped = replay_data::scope::omits(document.capture_mode()).is_some();
        let vision_refused = self.vision_refusal_line();
        if loading_art.is_some() || software || diagnostics || partial || scoped || vision_refused.is_some() {
            map_overlay(
                ui,
                rect.intersect(ui.clip_rect()),
                "diagnostics",
                self.interface_points,
                |ui, height| {
                    if let Some(loading) = loading_art { ui.label(loading); }
                    if let Some(refused) = &vision_refused {
                        ui.colored_label(egui::Color32::YELLOW, refused);
                    }
                    if partial {
                        partial_notice(ui, document.playback.replay());
                    }
                    if scoped {
                        capture_mode_notice(ui, document.playback.replay());
                    }
                    if software {
                        ui.colored_label(egui::Color32::YELLOW, SOFTWARE_RENDERER_NOTICE);
                    }
                    if diagnostics {
                        self.diagnostics_ui(ui, height);
                    }
                },
            );
        }
        if pinned.is_none() {
            let hud_rect = rect.intersect(ui.clip_rect());
            let hud_now = self.started.elapsed().as_secs_f64();
            if self.start_pick.choosing() {
                self.hud.paint(
                    ui,
                    hud_rect,
                    self.interface_points,
                    &hud::HudState::StartPick {
                        mode: self.camera_mode,
                        pad_connected: controls.pad_connected,
                    },
                    hud_now,
                );
            } else if free_mode {
                let active = match self.camera_mode {
                    CameraMode::Walk => self.walk.state().map(|walker| {
                        let field = viewer_gpu::province_map::HeightField {
                            width: document.map.width,
                            height: document.map.height,
                            values: document.assets.heightmap_texels(),
                        };
                        let local_floor = field
                            .surface_height(walker.feet[0], walker.feet[2])
                            .max(viewer_gpu::camera::WATER_HEIGHT);
                        hud::ActiveHud::Walk {
                            view: walker.view,
                            gait: walker.gait,
                            scale: walker.scale,
                            scale_limit: walker.at_scale_limit(
                                self.walk.figure_height(),
                                local_floor,
                                defines.heights[1],
                            ),
                            figure_missing: self.walk.figure_missing(),
                        }
                    }),
                    CameraMode::Fly => {
                        if let Some(free) = &self.free_cam {
                            let aircraft = match self.fly_aircraft.choice() {
                                None => hud::AircraftHud::Free,
                                Some(name) if self.fly_aircraft.refusal().is_some() => {
                                    hud::AircraftHud::Refused(hud::model_label(name))
                                }
                                Some(name) => hud::AircraftHud::Loading(hud::model_label(name)),
                            };
                            Some(hud::ActiveHud::FlyFree {
                                speed: free.speed_multiplier(),
                                speed_limit: free.at_scale_limit(
                                    crate::controls::walker::FALLBACK_FIGURE,
                                    viewer_gpu::camera::WATER_HEIGHT,
                                    defines.heights[1],
                                ),
                                aircraft,
                            })
                        } else {
                            self.flight.as_ref().map(|flight| hud::ActiveHud::FlyAircraft {
                                title: self
                                    .fly_aircraft(document)
                                    .map(|aircraft| hud::model_label(&aircraft.name))
                                    .unwrap_or_else(|| "Aircraft".into()),
                                view: flight.view,
                            })
                        }
                    }
                    CameraMode::Map => None,
                };
                if let Some(active) = active {
                    self.hud.observe(&active, &controls, hud_now);
                    self.hud.paint(
                        ui,
                        hud_rect,
                        self.interface_points,
                        &hud::HudState::Active {
                            active,
                            captured: controls.captured,
                            device: controls.device,
                        },
                        hud_now,
                    );
                }
            }
        }
        self.complete_first_picture(&mut document.playback);
        if !self.shown_whole {
            let cover = rect.intersect(ui.clip_rect());
            ui.painter().rect_filled(cover, 0.0, ui.visuals().panel_fill);
            ui.put(egui::Rect::from_center_size(cover.center(), egui::vec2(32.0, 32.0)), egui::Spinner::new());
        }
        self.drew_frame = true;
        let overlay_fresh = match &now {
            Some(now) => self.prepared_overlay.as_ref().is_some_and(|prepared| !prepared.is_stale(now)),
            None => true,
        };
        if self.load_complete(document.icons.lazy.busy(), overlay_fresh) {
            if document.content.finish_loading()
                && let Some(source) = &self.source
                && std::env::var_os("BATTLEPLAN_SOURCE_ADMISSION_FILE").or_else(|| std::env::var_os("HOI4_SOURCE_ADMISSION_FILE")).is_some()
                && let Ok(options) = self.config.resolve_options()
            {
                crate::perf_log::source_admission_probe(source, &options);
            }
        }
        self.drawn_at = Some(crate::perf_log::epoch_ms());
        self.drawn_frame = crate::perf_log::frame_number();
        self.drawn_options.clone_from(&self.applying_options);
        self.map_hits.borrow_mut().end_frame();
        let (branch, layers_hour) = self.hour_on_screen(document);
        let (hour, fraction) = match shown_head {
            Some(head) => (head.hour, f64::from(head.fraction)),
            None => (layers_hour, f64::from(document.hour_fraction)),
        };
        self.rendered = Some(Playhead { branch, hour, fraction });
        Ok(())
    }
}

impl Viewer {
    fn free_mode_ground(&self) -> Option<[f32; 2]> {
        match self.camera_mode {
            CameraMode::Map => None,
            CameraMode::Walk => self.walk.standing(),
            CameraMode::Fly => self
                .free_cam
                .as_ref()
                .map(|free| [free.feet[0], free.feet[2]])
                .or_else(|| {
                    self.flight
                        .as_ref()
                        .map(|flight| [flight.position[0], flight.position[2]])
                }),
        }
    }

    pub(crate) fn set_camera_mode(
        &mut self,
        mode: CameraMode,
        overlays: bool,
        ctx: &egui::Context,
    ) {
        if mode != CameraMode::Map && !overlays {
            self.combat.close();
            self.picker.find.field.focused = false;
        }
        if (mode, overlays) != (self.camera_mode, self.free_overlays)
            && let Some(embedded) = &self.embedded
        {
            embedded.report_camera_mode(mode, overlays);
        }
        self.free_overlays = overlays;
        if mode == self.camera_mode {
            return;
        }
        let standing = self.free_mode_ground();
        let handover = if self.camera_mode == CameraMode::Walk && mode == CameraMode::Fly {
            self.walk.eye_pose()
        } else {
            None
        };
        self.camera_mode = mode;
        self.start_pick = match (mode, standing) {
            (CameraMode::Map, _) => drop::Pick::Idle,
            (_, Some(at)) => drop::Pick::At(at),
            (_, None) => drop::Pick::Choosing,
        };
        self.walk.leave();
        self.flight = None;
        self.free_cam = None;
        self.last_fly_eye = None;
        self.mode_handover = handover;
        if mode == CameraMode::Map {
            self.hud.leave();
        } else if standing.is_some() {
            self.hud.enter_mode();
        }
        self.input.release();
        self.grab_pointer(ctx);
    }

    fn click_units(&mut self, hit: Option<usize>, shift: bool, ctx: &egui::Context) {
        let hit = hit.map(|index| &self.counter_report.unit_hits[index]);
        let units_changed = self.unit_selection.click(hit, shift);
        if !shift {
            self.ground_selection = None;
        }
        let fleets_changed = if !shift {
            self.fleet_selection.clear()
        } else {
            false
        };
        let air_changed = if !shift || hit.is_some_and(|hit| hit.ours) {
            self.air_selection.clear()
        } else {
            false
        };
        if units_changed || air_changed || fleets_changed {
            self.counter_view.selected = Arc::clone(self.unit_selection.units());
            ctx.request_repaint();
        }
    }

    fn grab_pointer(&mut self, ctx: &egui::Context) {
        let captured = self.input.captured() && self.camera_mode != CameraMode::Map;
        if captured == self.pointer_grabbed {
            return;
        }
        self.pointer_grabbed = captured;
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(if captured {
            egui::CursorGrab::Confined
        } else {
            egui::CursorGrab::None
        }));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!captured));
    }
}

struct AudioInputs<'a> {
    volumes: dashboard_core::protocol::MapAudio,
    muted: bool,
    reset: bool,
    music: Option<Arc<viewer_audio::Chances>>,
    battles: Option<Arc<viewer_audio::Battles>>,
    interface: Vec<InterfaceSound>,
    interface_key: &'a mut u64,
}

fn audio_mix(
    document: &Document,
    volumes: dashboard_core::protocol::MapAudio,
    muted: bool,
) -> viewer_audio::Mix {
    let percent = |value: f32| value / 100.0;
    viewer_audio::Mix {
        master: percent(volumes.master),
        dev_master: document.game_dev_master,
        ambient: percent(volumes.ambient),
        units: percent(volumes.units),
        combat: percent(volumes.combat),
        interface: percent(volumes.interface),
        voice: percent(volumes.voice),
        music: percent(volumes.music),
        muted,
    }
}

enum Act {
    Picker(viewer_combat::picker::Control, bool),
    Combat(usize),
    Dismiss,
    Confirmation(viewer_combat::confirmation::Control),
    AirOrder([f32; 2], Option<air_orders::Under>),
    Counter(Unit),
    Map([f32; 2]),
    Box([f32; 2], [f32; 2]),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    Picker,
    Combat,
    Counters,
    Confirmation,
}

const INTERFACE_SOUND_KEYS: u64 = 1 << 63;
const INTERFACE_CLICK_KEYS: u64 = 3 << 62;

struct InterfaceSound {
    effect: Arc<str>,
    click: Option<u64>,
    order: usize,
}

impl InterfaceSound {
    fn new(producer: &str, played: &viewer_native_gui::native_gui::pointer::Played) -> Self {
        use std::hash::{Hash, Hasher};
        Self {
            effect: Arc::from(String::from_utf8_lossy(&played.effect)),
            click: played.click.as_ref().map(|control| {
                let mut hasher = std::hash::DefaultHasher::new();
                (producer, control).hash(&mut hasher);
                INTERFACE_CLICK_KEYS | (hasher.finish() >> 2)
            }),
            order: played.order,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PausedMotion {
    Off,
    Live,
    Held,
}

#[derive(Default)]
struct AmbientClock {
    last: Option<(f64, bool)>,
    value: Option<f64>,
}

impl AmbientClock {
    fn sample(&mut self, now: f64, hold: bool) -> f32 {
        let value = match (self.last, self.value) {
            (Some((last, false)), Some(value)) if !hold => {
                let next = value + (now - last).max(0.0);
                self.value = Some(next);
                next
            }
            (Some(_), Some(value)) => value,
            (_, None) if hold => {
                self.value = Some(now);
                now
            }
            _ => now,
        };
        self.last = Some((now, hold));
        value as f32
    }
}

impl Viewer {
    fn paused_motion(&self) -> PausedMotion {
        if self.playing || self.capture.is_some() {
            PausedMotion::Off
        } else if self.dashboard_map {
            PausedMotion::Held
        } else {
            PausedMotion::Live
        }
    }

    fn ambient_time_at(&mut self, presentation_time: f64) -> f32 {
        match self.capture.as_ref().and_then(|capture| capture.time) {
            Some(time) => time,
            None => self
                .ambient_clock
                .sample(presentation_time, !self.playing && self.dashboard_map),
        }
    }
}

fn schedule_after_render(
    ctx: &egui::Context,
    elapsed: Duration,
    needs_frame: bool,
    paused_motion: PausedMotion,
    next_arrival: impl FnOnce() -> f32,
) {
    if needs_frame {
        request_animation_frame(ctx, elapsed);
    } else if paused_motion == PausedMotion::Live
        && let Ok(delay) = Duration::try_from_secs_f32(next_arrival())
    {
        request_frame_after(ctx, delay, elapsed);
    }
}

fn request_animation_frame(ctx: &egui::Context, elapsed: Duration) {
    request_frame_after(ctx, Duration::ZERO, elapsed);
}

fn request_frame_after(ctx: &egui::Context, delay: Duration, elapsed: Duration) {
    let interval = Duration::from_secs_f64(1.0 / viewer_runtime::display_refresh::rate());
    let prediction =
        ctx.input(|input| Duration::try_from_secs_f32(input.predicted_dt).unwrap_or_default());
    ctx.request_repaint_after(delay.max(interval).saturating_sub(elapsed) + prediction);
}

fn build_audio_frame(
    document: &Document,
    scene: &Scene,
    camera: &Camera,
    inputs: AudioInputs<'_>,
) -> Option<viewer_audio::Frame> {
    let AudioInputs {
        volumes,
        muted,
        reset,
        music,
        battles,
        interface,
        interface_key,
    } = inputs;
    let source = document.audio.as_ref()?;
    let mut emitters = scene
        .sound_events
        .iter()
        .map(|event| viewer_audio::Emitter {
            key: event.key,
            effect: Arc::clone(&event.effect),
            position: event.position,
            age: event.age,
            group: if event.combat {
                viewer_audio::Group::Combat
            } else {
                viewer_audio::Group::Units
            },
            restart: false,
        })
        .collect::<Vec<_>>();
    for (at, sound) in interface.iter().enumerate() {
        if sound.click.is_some()
            && interface[at + 1..]
                .iter()
                .any(|later| later.click == sound.click)
        {
            continue;
        }
        let key = sound.click.unwrap_or_else(|| {
            *interface_key = interface_key.wrapping_add(1);
            INTERFACE_SOUND_KEYS | (*interface_key & ((1 << 62) - 1))
        });
        emitters.push(viewer_audio::Emitter {
            key,
            effect: Arc::clone(&sound.effect),
            position: camera.eye(),
            age: 0.0,
            group: viewer_audio::Group::Interface,
            restart: false,
        });
    }
    if let Some(battles) = &battles {
        viewer_audio::battle::emitters(battles, &source.content, camera.eye(), &mut emitters);
    }
    let forward = camera.look_direction();
    let right = [forward[2], 0.0, -forward[0]];
    let length = (right[0] * right[0] + right[2] * right[2]).sqrt();
    let right = if length > 0.0 {
        [right[0] / length, 0.0, right[2] / length]
    } else {
        [1.0, 0.0, 0.0]
    };
    Some(viewer_audio::Frame {
        listener: viewer_audio::Listener {
            position: camera.eye(),
            right,
            altitude: camera.height,
        },
        emitters,
        mix: audio_mix(document, volumes, muted),
        reset,
        elapsed: 0.0,
        hour: Some(document.playback.hour()),
        music,
    })
}

fn present_audio(
    audio: &mut Option<viewer_audio::Owner>,
    ctx: &egui::Context,
    document: &Document,
    scene: &Scene,
    camera: &Camera,
    wake: bool,
    inputs: AudioInputs<'_>,
) {
    let Some(source) = &document.audio else {
        if let Some(owner) = audio {
            owner.release();
        }
        return;
    };
    let Some(frame) = build_audio_frame(document, scene, camera, inputs) else {
        return;
    };
    let owner = audio.get_or_insert_with(|| {
        let mut owner = viewer_audio::System::shared().owner();
        let ctx = ctx.clone();
        owner.set_wake(move || request_animation_frame(&ctx, Duration::ZERO));
        owner
    });
    owner.set_wake_enabled(wake);
    owner.claim();
    owner.frame(source, frame);
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if self.capture.as_ref().is_some_and(|capture| capture.exit) {
            self.refused("the window closed before its picture was saved");
        }
    }
}

impl eframe::App for Viewer {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if self.options.window_dump {
            input.events.clear();
            input.events.push(egui::Event::PointerGone);
            input.hovered_files.clear();
            input.dropped_files.clear();
            input.focused = false;
            return;
        }
        crate::suite_input::apply(ctx, input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        crate::perf_log::frame_start();
        let _other_cost = replay_frame::cpu::scope("frame.logic", Some("other"));
        if (ctx.input(|input| input.viewport().visible()) == Some(false) || self.embedded.as_ref().is_some_and(|embedded| !embedded.visible())) && !self.holds_export() {
            self.not_drawn();
        }
        engine_executable::executable_tables::scope(self.tables(), || {
            self.embedded_logic(ctx, frame)
        });
        if self.embedded.is_none() {
            let _other_cost = replay_frame::cpu::scope("frame.load", Some("other"));
            self.poll(ctx);
        }
        self.follow_shown_maps(ctx, Some(frame));
        self.retire_hidden_cursor();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        crate::perf_log::frame_ui();
        let tables = self.tables();
        let pass = replay_frame::cpu::scope("frame.ui", Some("other"));
        engine_executable::executable_tables::scope(tables, || self.campaign_ui(ui, frame));
        drop(pass);
        self.trace_switch_frame();
        crate::perf_log::frame_end(self.drawn_stamp().filter(|_| self.drawn_frame == crate::perf_log::frame_number()));
    }
}

impl Viewer {
    pub(crate) fn tables(&self) -> Option<engine_executable::executable_tables::Tables> {
        self.document
            .as_ref()
            .and_then(|document| document.content.tables)
    }

    fn campaign_ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(recorder) = viewer_runtime::frame_writer::recorder() {
            let mut recording = Some(recorder.pass());
            let drawn = self.update(ui, frame, &mut recording);
            if let Some(recording) = recording {
                recording.record(
                    self.document
                        .as_ref()
                        .map(|document| (document.playback.hour(), document.hour_fraction)),
                    drawn,
                    "campaign",
                );
            }
        } else {
            self.update(ui, frame, &mut None);
        }
    }
}

impl Viewer {
    fn update(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        recording: &mut Option<viewer_runtime::frame_writer::Pass<'_>>,
    ) -> bool {
        if self
            .embedded
            .as_ref()
            .is_some_and(|embedded| !embedded.visible())
            && !self.holds_export()
        {
            self.not_drawn();
            if self.dashboard_active
                && let Some(recording) = recording.take()
            {
                recording.record(None, false, "-");
            }
            return false;
        }
        if ui.input(|input| input.events.iter().any(is_diagnostics_shortcut)) {
            self.toggle_diagnostics();
        }
        if self.dashboard_active {
            self.export_closed("its map was hidden before its picture was saved");
            self.release_audio();
            self.close_playhead();
            self.retire_hidden_cursor();
        } else if let Some(dashboard) = &mut self.dashboard {
            dashboard.release_audio();
            dashboard.not_drawn();
        }
        if self.dashboard_active
            && let Some(dashboard) = &mut self.dashboard
        {
            dashboard.ui_scale = self.ui_scale;
            dashboard.ui(ui, frame, recording.take());
            if let Some(embedded) = &mut self.embedded {
                embedded.report_maps(dashboard.states(), ui.ctx());
                for (view, event) in dashboard.take_view_events() {
                    match event {
                        picker::ViewEvent::Options(options) => {
                            embedded.report_view_options(&view, options)
                        }
                        picker::ViewEvent::Toggle(toggle) => {
                            embedded.report_view_toggle(&view, toggle)
                        }
                        picker::ViewEvent::Modes(modes) => embedded.report_view_modes(&view, modes),
                        picker::ViewEvent::Observe(country) => embedded.report_view_observe(&view, country),
                    }
                }
            }
            return false;
        }
        let events = self.take_view_events();
        if let Some(embedded) = &self.embedded {
            for event in events {
                match event {
                    picker::ViewEvent::Options(options) => embedded
                        .report_view_options(dashboard_core::protocol::CAMPAIGN_MAP_VIEW, options),
                    picker::ViewEvent::Toggle(toggle) => {
                        embedded.report_view_toggle(dashboard_core::protocol::CAMPAIGN_MAP_VIEW, toggle)
                    }
                    picker::ViewEvent::Modes(modes) => {
                        embedded.report_view_modes(dashboard_core::protocol::CAMPAIGN_MAP_VIEW, modes)
                    }
                    picker::ViewEvent::Observe(country) => {
                        embedded.report_view_observe(dashboard_core::protocol::CAMPAIGN_MAP_VIEW, country)
                    }
                }
            }
        }
        let waited = if profiling() && self.capture.is_some() {
            pace_profiled_frame()
        } else {
            Duration::ZERO
        };
        self.laps = Some(Laps::new(waited));
        {
            let _other_cost = replay_frame::cpu::scope("frame.load", Some("other"));
            self.poll(ui.ctx());
        }
        self.follow_shown_maps(ui.ctx(), Some(frame));
        self.lap("poll");
        let central_panel = egui::CentralPanel::default().frame(egui::Frame::NONE);
        if !profiling() || self.capture.is_none() {
            self.profile_playback.configure(None, false);
        }
        let drawn = if self.scene.is_some() && let Some(mut document) = self.document.take() {
            if profiling() && self.capture.is_some() {
                let mode = std::env::var("BATTLEPLAN_VIEWER_PROFILE").or_else(|_| std::env::var("HOI4_VIEWER_PROFILE")).unwrap_or_default();
                let sweep_frames = profiling_sweep();
                let middle = sweep_frames.start + sweep_frames.len() as u32 / 2;
                let mut sought = false;
                if let Some(playhead) = &mut self.playhead {
                    if mode == "hours"
                        && (self.profile_frame == sweep_frames.start
                            || self.profile_frame == middle)
                    {
                        sought = true;
                        playhead.hour += if self.profile_frame == sweep_frames.start {
                            1
                        } else {
                            -1
                        };
                        playhead.fraction = 0.0;
                    }
                    if self.profile_frame == sweep_frames.start
                        && mode == "seek_end"
                        && let Some((_, last)) = document.playback.range(playhead.branch)
                    {
                        sought = true;
                        playhead.hour = last;
                        playhead.fraction = 0.0;
                    }
                }
                if sought {
                    self.animation_seek = true;
                }
                let playing = profile_playback::Mode::parse(&mode);
                self.playing = playing.is_some() && profiling_sweep().contains(&self.profile_frame);
                if let Some(mode) = playing {
                    self.speed = mode.speed();
                }
                self.profile_playback.configure(playing, self.playing);
                if self.playing
                    && !self.profile_playback.wall_active()
                    && let Some(head) = self.playhead
                {
                    let last = document
                        .playback
                        .range(head.branch)
                        .map_or(head.hour, |(_, last)| last);
                    self.tick_playhead(1.0 / hz(), last);
                }
                if playing.is_none()
                    && !mode.starts_with("pan:")
                    && !mode.starts_with("zoom:")
                    && let Some(playhead) = &mut self.playhead
                {
                    playhead.fraction = if !profiling_sweep().contains(&self.profile_frame) {
                        f64::from(self.options.hour_fraction)
                    } else {
                        f64::from((self.profile_frame - sweep_frames.start) % 120) / 120.0
                    };
                }
                let sweep = mode
                    .strip_prefix("pan:")
                    .and_then(|rate| {
                        rate.parse::<f32>()
                            .ok()
                            .map(|rate| (rate / hz() as f32, 1.0))
                    })
                    .or_else(|| {
                        mode.strip_prefix("zoom:").and_then(|factor| {
                            factor
                                .parse::<f32>()
                                .ok()
                                .filter(|factor| *factor > 0.0)
                                .map(|factor| (0.0, factor.powf(1.0 / hz() as f32)))
                        })
                    });
                if let Some((dx, zoom)) = sweep
                    && profiling_sweep().contains(&self.profile_frame)
                    && let Some(capture) = self.capture.as_mut()
                    && let Some((_, camera)) = capture.pinned.as_mut()
                    && let Some(camera) = camera.as_mut()
                {
                    camera.center[0] += dx;
                    camera.height = (camera.height * zoom).clamp(40.0, 900.0);
                }
                if self.profile_frame == sweep_frames.start
                    && let Some(next) = mode
                        .strip_prefix("mode:")
                        .and_then(viewer_map_modes::map_mode::MapMode::parse)
                {
                    self.choose_map_mode(next);
                }
                if mode == "fly"
                    && profiling_sweep().contains(&self.profile_frame)
                    && let Some(capture) = self.capture.as_mut()
                {
                    capture.time = capture.time.map(|time| time + (1.0 / hz()) as f32);
                }
            }
            self.lap("profile_sweep");
            let drawn = self.window_frame(ui, frame.wgpu_render_state(), &mut document);
            self.document = Some(document);
            drawn
        } else {
            self.release_audio();
            central_panel.show(ui, |ui| {
                if self.loading.is_some() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(100.0);
                        ui.spinner();
                        ui.heading("Opening replay");
                        ui.label("Verifying content and loading the map...");
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                } else if self.building_scene() || !self.document_fallback_current() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(100.0);
                        ui.spinner();
                        ui.heading("Loading map");
                        ui.label("Building the map scene...");
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
            });
            false
        };
        {
            let _other_cost = replay_frame::cpu::scope("frame.report", Some("other"));
            self.report_embedded(ui.ctx());
        }
        mark_frame_end();
        drawn
    }

    fn drawn_stamp(&self) -> Option<(replay_data::BranchId, replay_data::Hour, f64)> {
        let head = self.rendered.filter(|_| self.shown_whole)?;
        Some((head.branch, head.hour, self.drawn_at?))
    }
}

pub(crate) fn date(hour: i32) -> String {
    let value = i64::from(hour) - 43_800_000;
    if value < 0 {
        return format!("Hour {hour}");
    }
    let days = value / 24;
    let year = days / 365;
    let mut day = days % 365;
    let mut month = 1;
    for length in [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31] {
        if day < length {
            break;
        }
        day -= length;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02}  {:02}:00",
        day + 1,
        value % 24 + 1
    )
}

fn check_overlay() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("BATTLEPLAN_VIEWER_CHECK_OVERLAY").or_else(|| std::env::var_os("HOI4_VIEWER_CHECK_OVERLAY")).is_some())
}

pub(crate) mod overlay_checks {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub(crate) static LOOKED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static COMPARED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static NOTHING_NEW: AtomicU64 = AtomicU64::new(0);
    pub(crate) static ART_MOVED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static ROWS_MISSED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static LAYERS_MOVED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static NO_OVERLAY: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn note(counter: &AtomicU64) {
        counter.fetch_add(1, Relaxed);
    }

    pub(crate) fn counted() -> [u64; 7] {
        [&LOOKED, &COMPARED, &NOTHING_NEW, &ART_MOVED, &ROWS_MISSED, &LAYERS_MOVED, &NO_OVERLAY].map(|counter| counter.load(Relaxed))
    }
}

fn check_anchor() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("BATTLEPLAN_VIEWER_CHECK_ANCHOR").or_else(|| std::env::var_os("HOI4_VIEWER_CHECK_ANCHOR")).is_some())
}

fn placed_anchor(target: Playhead, moving: bool, speed: f64, age: Duration, last: i32) -> Playhead {
    let mut placed = target;
    if moving && speed > 0.0 {
        let fraction = placed.fraction + age.as_secs_f64() * speed;
        advance_clock(&mut placed, fraction, last);
    }
    placed
}

fn forward_only(current: Option<Playhead>, placed: Playhead, moving: bool) -> Playhead {
    match current {
        Some(current) if moving && current.branch == placed.branch => {
            let lead = f64::from(current.hour) - f64::from(placed.hour) + current.fraction
                - placed.fraction;
            if lead > 0.0 { current } else { placed }
        }
        _ => placed,
    }
}

fn tick_clock(head: &mut Playhead, speed: f64, seconds: f64, last: i32) -> bool {
    let step = seconds.max(0.0) * speed;
    advance_clock(head, head.fraction + step, last)
}

fn advance_clock(playhead: &mut Playhead, fraction: f64, last: i32) -> bool {
    if fraction >= f64::from(last) - f64::from(playhead.hour) {
        playhead.hour = last;
        playhead.fraction = 0.0;
        return false;
    }
    playhead.hour = (f64::from(playhead.hour) + fraction.floor()) as i32;
    playhead.fraction = fraction.fract();
    true
}

fn whole_picture(
    on: &crate::playhead_host::OnScreen,
    overlay: Option<replay_data::Hour>,
    hour: i32,
    playing: bool,
) -> bool {
    if !layers_show(on, hour, playing) {
        return false;
    }
    match playing {
        false => overlay == Some(hour),
        true => overlay.is_none_or(|shown| on.drawn.is_none_or(|drawn| shown <= drawn) && on.sought.is_none_or(|sought| sought <= shown)),
    }
}

fn layers_show(on: &crate::playhead_host::OnScreen, hour: i32, playing: bool) -> bool {
    on.drawn.is_some_and(|drawn| match playing {
        true => on.sought.is_none_or(|sought| sought <= drawn && sought <= hour) && drawn <= on.head && drawn <= hour && !on.composing,
        false => drawn == hour && on.settled,
    })
}

fn refusal(error: &std::io::Error) -> String {
    format!("The map's playhead is refused: {error}")
}

fn layers_behind_reason(on: &crate::playhead_host::OnScreen) -> String {
    let holding = match on.furthest {
        Some(layer) => layer,
        None if on.composing => "composing a reset",
        None => "no layer drawn yet",
    };
    format!("the playhead's layers: {holding}")
}

pub(super) fn read_click<T>(lines: &mut Vec<String>, what: &str, read: Option<std::io::Result<T>>) -> Option<T> {
    match read.transpose() {
        Ok(value) => value,
        Err(error) => {
            lines.push(format!("{what}: {error}"));
            None
        }
    }
}

fn settle_models(side: &mut crate::playhead_host::MapSide, hour: replay_data::Hour, limit: Duration) -> Result<(), String> {
    let until = Instant::now() + limit;
    loop {
        let Some(units) = side.draws("units").then(|| viewer_layers_forces::units::of(side)).flatten() else { return Ok(()) };
        if units.figures_stand(hour) {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(format!("The unit figures at hour {hour}: {} after {limit:?}", units.figures_state()));
        }
        side.settle_all().map_err(|error| format!("The unit figures at hour {hour}: {error}"))?;
    }
}

fn settle_plan(side: &mut crate::playhead_host::MapSide, hour: replay_data::Hour, limit: Duration) -> Result<(), String> {
    let until = Instant::now() + limit;
    loop {
        let orders = side.draws("orders").then(|| side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<viewer_layers_forces::orders::Orders>()))).flatten();
        let Some(orders) = orders.filter(|orders| orders.perspective().is_some()) else { return Ok(()) };
        let made = orders.made_for();
        if made == Some(hour) {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(format!("The battle plan at hour {hour}: the orders layer's plan stands for the rows at {made:?} after {limit:?} (none: a build or a table it learned is still coming)"));
        }
        side.settle_all().map_err(|error| format!("The battle plan at hour {hour}: {error}"))?;
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn combat_left_out(interface: bool, out: Option<String>, art_moved: bool) -> (Option<String>, bool) {
    match (interface, out) {
        (false, _) => (Some("the map's interface is hidden in this camera mode, so the window's quads are not drawn".to_owned()), false),
        (true, Some(_)) if art_moved => (None, true),
        (true, out) => (out, false),
    }
}

fn missed_tables(missed: &[replay_data::rows::TableId]) -> Option<String> {
    (!missed.is_empty()).then(|| missed.iter().map(|table| format!("{table:?}")).collect::<Vec<_>>().join(", "))
}

fn live_of(map: &Option<viewer_playback::side::SideThread>) -> Option<&viewer_playback::side::SideThread> {
    map.as_ref().filter(|side| side.failure().is_none())
}

fn head_wanted(capture: Option<bool>, hour: Option<replay_data::Hour>, window_dump: bool, drawing: bool) -> crate::playhead_host::Head {
    use crate::playhead_host::Head;
    match (capture, hour) {
        (Some(paced), Some(hour)) if paced || !drawing => Head::Paced(hour),
        (Some(_), Some(hour)) => Head::Stepped(hour),
        (None, Some(hour)) if window_dump && drawing => Head::Stepped(hour),
        _ => Head::Clock,
    }
}

#[cfg(test)]
#[test]
fn a_frame_that_draws_no_map_waits_for_no_whole_picture() {
    use crate::playhead_host::Head;
    assert_eq!(head_wanted(Some(false), Some(7), false, true), Head::Stepped(7));
    assert_eq!(head_wanted(Some(false), Some(7), false, false), Head::Paced(7));
    assert_eq!(head_wanted(Some(true), Some(7), false, true), Head::Paced(7), "a paced capture waits for no picture");
    assert_eq!(head_wanted(None, Some(7), true, true), Head::Stepped(7));
    assert_eq!(head_wanted(None, Some(7), true, false), Head::Clock);
    assert_eq!(head_wanted(None, Some(7), false, true), Head::Clock, "a live window follows its clock");
    assert_eq!(head_wanted(None, None, true, true), Head::Clock);
}

fn builds_warnings(diagnostics: bool, capture: Option<&Capture>) -> bool {
    diagnostics || capture.is_some_and(|capture| capture.remaining == 0)
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn playing_the_layers_are_the_pictures_hour_and_stopped_the_overlay_is_too() {
        let on = |drawn: Option<i32>, sought: Option<i32>| crate::playhead_host::OnScreen {
            head: 10,
            drawn,
            sought,
            composing: false,
            settled: true,
            furthest: None,
            resets: 0,
        };
        let whole = |overlay: Option<i32>, hour: i32, playing: bool| super::whole_picture(&on(Some(8), None), overlay, hour, playing);
        assert!(whole(Some(8), 8, true), "the overlay of the layers' own hour");
        assert!(whole(Some(5), 8, true), "a layout a step behind: drawn over the layers, holding nothing");
        assert!(whole(None, 8, true), "before the first layout, the layers are the picture");
        assert!(!whole(Some(9), 8, true), "never an overlay of an hour the layers have not reached");
        assert!(!super::whole_picture(&on(Some(8), Some(7)), Some(6), 8, true));
        assert!(super::whole_picture(&on(Some(8), Some(7)), Some(7), 8, true));
        assert!(!super::whole_picture(&on(Some(8), Some(6)), None, 6, true), "a picture past the hour asked for");
        let composing = crate::playhead_host::OnScreen { composing: true, ..on(Some(6), Some(6)) };
        assert!(!super::whole_picture(&composing, None, 6, true), "a picture a reset is replacing");
        assert!(whole(Some(8), 8, false));
        assert!(!whole(Some(7), 8, false), "a paused picture is never left with an earlier overlay");
        assert!(!whole(None, 8, false));
    }

    #[test]
    fn window_dumps_discard_live_input_before_egui_sees_it() {
        use eframe::App;
        for window_dump in [false, true] {
            let mut viewer = super::Viewer::create(
                super::Config::default(),
                super::Options {
                    window_dump,
                    ..Default::default()
                },
                None,
                std::sync::Arc::new(std::sync::Mutex::new(None)),
                None,
                None,
            );
            let ctx = egui::Context::default();
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
            for time in [0.0, 1.0] {
                let mut input = egui::RawInput {
                    screen_rect: Some(rect),
                    time: Some(time),
                    focused: true,
                    events: vec![
                        egui::Event::PointerMoved(egui::pos2(639.0, 479.0)),
                        egui::Event::PointerButton {
                            pos: egui::pos2(639.0, 479.0),
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::CTRL,
                        },
                        egui::Event::Key {
                            key: egui::Key::F3,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::CTRL,
                        },
                        egui::Event::Text("live input".into()),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, 120.0),
                            phase: egui::TouchPhase::Move,
                            modifiers: egui::Modifiers::CTRL,
                        },
                    ],
                    hovered_files: vec![egui::HoveredFile::default()],
                    ..Default::default()
                };
                let events = input.events.clone();
                viewer.raw_input_hook(&ctx, &mut input);
                assert_eq!(input.screen_rect, Some(rect));
                assert_eq!(input.time, Some(time));
                assert_eq!(input.focused, !window_dump);
                assert_eq!(input.hovered_files.is_empty(), window_dump);
                if window_dump {
                    assert_eq!(input.events, vec![egui::Event::PointerGone]);
                } else {
                    assert_eq!(input.events, events);
                }
                let mut output = ctx.run_ui(input, |ui| {
                    ui.input(|input| {
                        assert_eq!(input.pointer.hover_pos().is_none(), window_dump);
                        assert_eq!(input.pointer.any_down(), !window_dump);
                        assert_eq!(input.key_down(egui::Key::F3), !window_dump);
                    });
                });
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn profile_pointer_overrides_hover_only_for_profile_captures() {
        use super::profile_hover_point;
        let real = Some([30.0, 60.0]);
        let synthetic = Some([120.0, 240.0]);
        assert_eq!(
            profile_hover_point(real, synthetic, true, [640, 480], 1.5),
            Some([80.0, 160.0])
        );
        assert_eq!(
            profile_hover_point(real, synthetic, false, [640, 480], 1.5),
            Some([20.0, 40.0])
        );
        assert_eq!(
            profile_hover_point(real, None, true, [640, 480], 1.5),
            Some([20.0, 40.0])
        );
        assert_eq!(
            profile_hover_point(None, synthetic, false, [640, 480], 1.5),
            None
        );
        assert_eq!(
            profile_hover_point(None, synthetic, true, [640, 480], 1.5),
            Some([80.0, 160.0])
        );
        for point in [[640.0, 20.0], [20.0, 480.0], [-1.0, 20.0], [f32::NAN, 20.0]] {
            assert_eq!(
                profile_hover_point(real, Some(point), true, [640, 480], 1.0),
                None
            );
        }
    }

    use super::*;

    #[test]
    fn a_capture_builds_warnings_only_on_the_frames_that_can_save_them() {
        let capture = |remaining, frames| Capture {
            path: "capture.png".into(),
            remaining,
            pinned: None,
            exit: true,
            time: None,
            step: 0.25,
            frames,
            index: 0,
            paced: false,
            export: None,
            video: None,
        };
        assert!(!builds_warnings(false, None));
        for remaining in [0, 1, 3_321] {
            assert!(builds_warnings(true, Some(&capture(remaining, 1))));
        }
        assert!(builds_warnings(true, None));
        for (remaining, frames) in [(3_321, 1), (1, 1), (0, 1), (2, 3)] {
            let mut capture = capture(remaining, frames);
            let mut built = Vec::new();
            loop {
                built.push(builds_warnings(false, Some(&capture)));
                if capture.remaining > 0 {
                    capture.remaining -= 1;
                    continue;
                }
                if capture.frames == 1 {
                    break;
                }
                capture.frames -= 1;
                capture.index += 1;
                capture.remaining = 0;
            }
            let saved: Vec<_> = (0..built.len())
                .map(|frame| frame >= remaining as usize)
                .collect();
            assert_eq!(built, saved, "remaining {remaining}, frames {frames}");
            assert_eq!(
                built.iter().filter(|&&built| built).count(),
                frames as usize,
                "one build per saved frame"
            );
        }
    }

    #[test]
    fn occupation_controls_keep_independent_hits_and_follow_free_icon_visibility() {
        use viewer_gpu::icon_tracks::IconFamily;
        use viewer_native_gui::{
            free_icons::{Looks, Present},
            native_gui::HitGeometry,
            sprites::layer,
        };
        use viewer_map_icons::occupation_icons::{Hit, Hover};
        let parent = layer(5, 0, 0);
        let hits = [
            Hit {
                geometry: HitGeometry::test_rectangle([0, 0], [20, 10], [100, 100]),
                layer: parent,
                hover: Hover::Resistance { state: 7 },
            },
            Hit {
                geometry: HitGeometry::test_rectangle([10, 12], [10, 10], [100, 100]),
                layer: parent,
                hover: Hover::Modifier {
                    state: 7,
                    token: "synthetic_badge".into(),
                },
            },
        ];
        let mut report = CounterReport::default();
        assert_eq!(
            top_map_hit([5., 5.], &report, &[], &hits, None, None),
            Some(MapHit::Occupation(0))
        );
        assert_eq!(
            top_map_hit([15., 17.], &report, &[], &hits, None, None),
            Some(MapHit::Occupation(1))
        );
        assert_eq!(
            top_map_hit([5., 17.], &report, &[], &hits, None, None),
            None
        );
        let mut looks = Looks::default();
        let present = |alpha| Present {
            pivot: [10., 10.],
            scale: 2.,
            alpha,
            rank: 4,
        };
        looks.show(IconFamily::Occupation, parent, present(1.));
        assert_eq!(
            top_map_hit([20., 24.], &report, &[], &hits, Some(&looks), None),
            Some(MapHit::Occupation(1))
        );
        let absent = Looks::default();
        assert_eq!(
            top_map_hit([20., 24.], &report, &[], &hits, Some(&absent), None),
            None
        );
        let mut faint = Looks::default();
        faint.show(IconFamily::Occupation, parent, present(0.2));
        assert_eq!(
            top_map_hit([20., 24.], &report, &[], &hits, Some(&faint), None),
            None
        );
        let base_layer = layer(20, 0, 0);
        report
            .base_hits
            .push(viewer_native_gui::counter_hovers::unit_hits::Blocker {
                rect: [0., 0., 40., 40.],
                layer: base_layer,
            });
        looks.show(
            IconFamily::Bases,
            base_layer,
            Present {
                pivot: [10., 10.],
                scale: 1.,
                alpha: 1.,
                rank: 3,
            },
        );
        assert_eq!(
            top_map_hit([20., 24.], &report, &[], &hits, Some(&looks), None),
            Some(MapHit::Occupation(1))
        );
        looks.show(
            IconFamily::Occupation,
            parent,
            Present {
                rank: 2,
                ..present(1.)
            },
        );
        assert_eq!(
            top_map_hit([20., 24.], &report, &[], &hits, Some(&looks), None),
            Some(MapHit::Base(0))
        );
    }

    #[test]
    fn naval_regional_controls_own_hover_footprints_and_preserve_unsupported_blocking() {
        use viewer_counter_kit::{CounterReport, NavalRegionalControl, NavalRegionalHover};
        let mut report = CounterReport::default();
        report.naval_region_hovers.push(NavalRegionalHover {
            region: 7,
            control: NavalRegionalControl::Mines,
            rect: [0., 0., 10., 10.],
            layer: 8,
            hit: None,
            supported: true,
            text: None,
        });
        assert_eq!(
            top_map_hit([5., 5.], &report, &[], &[], None, None),
            Some(MapHit::NavalRegionHover(0))
        );
        assert_eq!(top_map_hit([15., 5.], &report, &[], &[], None, None), None);
        report.naval_region_hovers[0].supported = false;
        assert_eq!(
            top_map_hit([5., 5.], &report, &[], &[], None, None),
            Some(MapHit::Blocked)
        );
    }

    #[test]
    fn naval_mission_buttons_intercept_at_their_drawn_layer() {
        use viewer_counter_kit::{CounterReport, NavalMissionHit};
        use viewer_native_gui::counter_hovers::unit_hits::Blocker;
        let mut report = CounterReport::default();
        report.naval_mission_hits.push(NavalMissionHit {
            region: 7,
            mission: 2,
            rect: [0., 0., 10., 10.],
            layer: 8,
            text: None,
        });
        assert_eq!(
            top_map_hit([5., 5.], &report, &[], &[], None, None),
            Some(MapHit::NavalMission(0))
        );
        assert_eq!(top_map_hit([11., 5.], &report, &[], &[], None, None), None);
        report.base_hits.push(Blocker {
            rect: [0., 0., 10., 10.],
            layer: 9,
        });
        assert_eq!(
            top_map_hit([5., 5.], &report, &[], &[], None, None),
            Some(MapHit::Base(0))
        );
    }

    #[test]
    fn prepared_naval_mission_hover_keeps_exact_text_and_missions_footprint() {
        use viewer_counter_kit::NavalMissionHit;
        use viewer_gpu::icon_tracks::IconFamily;
        use viewer_native_gui::free_icons::{Looks, Present};
        let mut report = CounterReport::default();
        let text = Arc::new(Tooltip {
            title: "Fleet\n§YStatus§!".into(),
            body: "\u{15}active\nprepared mission detail".into(),
            refused: BTreeMap::from([("missing source".into(), 2)]),
        });
        let rect = [10., 10., 20., 20.];
        report.naval_mission_hits.push(NavalMissionHit {
            region: 7,
            mission: 0,
            rect,
            layer: 8,
            text: Some(Arc::clone(&text)),
        });
        let mut looks = Looks::default();
        looks.show(
            IconFamily::Missions,
            8,
            Present {
                pivot: [0., 0.],
                scale: 2.,
                alpha: 1.,
                rank: 0,
            },
        );
        let target = top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None);
        assert_eq!(target, Some(MapHit::NavalMission(0)));
        let (shown, footprint) = prepared_naval_tooltip(&report, target).unwrap();
        assert!(Arc::ptr_eq(&shown, &text));
        assert_eq!(footprint, rect);
        assert_eq!(
            top_map_hit([65., 50.], &report, &[], &[], Some(&looks), None),
            None
        );
        report.naval_mission_hits[0].text = None;
        assert!(prepared_naval_tooltip(&report, target).is_none());
        assert_eq!(
            top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None),
            target
        );
    }

    #[test]
    fn prepared_naval_convoy_hover_uses_its_own_text_and_footprint() {
        use viewer_counter_kit::{NavalRegionalControl, NavalRegionalHover};
        let text = Arc::new(Tooltip {
            title: "Convoys".into(),
            body: "recorded client operands".into(),
            refused: BTreeMap::new(),
        });
        let rect = [4., 6., 10., 12.];
        let mut report = CounterReport::default();
        report.naval_region_hovers.push(NavalRegionalHover {
            region: 7,
            control: NavalRegionalControl::Convoy(19),
            rect,
            layer: 8,
            hit: None,
            supported: true,
            text: Some(Arc::clone(&text)),
        });
        let target = top_map_hit([5., 7.], &report, &[], &[], None, None);
        assert_eq!(target, Some(MapHit::NavalRegionHover(0)));
        let (shown, footprint) = prepared_naval_tooltip(&report, target).unwrap();
        assert!(Arc::ptr_eq(&shown, &text));
        assert_eq!(footprint, rect);
        report.naval_region_hovers[0].supported = false;
        let target = top_map_hit([5., 7.], &report, &[], &[], None, None);
        assert_eq!(target, Some(MapHit::Blocked));
        assert!(prepared_naval_tooltip(&report, target).is_none());
    }

    #[test]
    fn regional_naval_grid_units_use_the_missions_transform() {
        use viewer_native_gui::{
            counter_hovers::unit_hits::{Hit, Kind},
            free_icons::{Looks, Present},
        };
        use viewer_gpu::icon_tracks::IconFamily;
        let mut report = CounterReport::default();
        report.unit_hits.push(Hit {
            ours: true,
            port: true,
            rect: [10., 10., 20., 20.],
            layer: 8,
            kind: Kind::TaskForce,
            members: Arc::from([19]),
            plate: None,
        });
        report.regional_naval_unit_hits.insert(0);
        let mut looks = Looks::default();
        looks.show(
            IconFamily::Missions,
            8,
            Present {
                pivot: [0., 0.],
                scale: 2.,
                alpha: 1.,
                rank: 0,
            },
        );
        assert_eq!(
            top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None),
            Some(MapHit::Unit(0))
        );
        report.regional_naval_unit_hits.clear();
        assert_eq!(
            top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None),
            None
        );
    }

    #[test]
    fn radar_toggle_hover_uses_the_top_base_and_never_a_covered_station() {
        use viewer_native_gui::counter_hovers::unit_hits::Blocker;
        let mut report = viewer_counter_kit::CounterReport::default();
        report.base_hits.push(Blocker {
            rect: [0., 0., 20., 20.],
            layer: 1,
        });
        report.radar_hits.insert(0, 7);
        assert_eq!(
            radar_province(
                top_map_hit([5., 5.], &report, &[], &[], None, None),
                &report
            ),
            Some(7)
        );
        report.base_hits.push(Blocker {
            rect: [0., 0., 20., 20.],
            layer: 2,
        });
        assert_eq!(
            radar_province(
                top_map_hit([5., 5.], &report, &[], &[], None, None),
                &report
            ),
            None
        );
        report.radar_hits.insert(1, 8);
        assert_eq!(
            radar_province(
                top_map_hit([5., 5.], &report, &[], &[], None, None),
                &report
            ),
            Some(8)
        );
    }

    #[test]
    fn task_force_children_outside_the_plate_keep_its_priority_and_block_unknown_hit_forms() {
        content_reader_script::test_game::enter();
        use viewer_native_gui::{
            counter_hovers::{task_force::{Control, Hover}, unit_hits::{Blocker, Hit, Kind}},
            sprites::layer,
        };
        let mut report = viewer_counter_kit::CounterReport::default();
        report.unit_hits.push(Hit {
            ours: true,
            port: false,
            rect: [20., 20., 66., 33.],
            kind: Kind::TaskForce,
            members: Arc::from([7]),
            layer: layer(10, 0, 0),
            plate: None,
        });
        report.task_force_hovers.push(Hover {
            control: Control::Carrier,
            unit_hit: 0,
            rect: [4., 20., 16., 16.],
            hit: None,
            authored: None,
            supported: true,
            ours: true,
            coarse: false,
            hidden: false,
            members: Arc::from([7]),
        });
        let point = [8., 24.];
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Unit(0))
        );
        report.base_hits.push(Blocker {
            rect: [0., 0., 40., 40.],
            layer: layer(11, 0, 0),
        });
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Base(0))
        );
        report.base_hits.clear();
        report.task_force_hovers[0].supported = false;
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Blocked)
        );
        report.unit_hits[0].kind = Kind::Division;
        assert_eq!(top_map_hit(point, &report, &[], &[], None, None), None);

        use viewer_native_gui::counter_hovers::task_force_port as port;
        report.unit_hits[0].kind = Kind::TaskForce;
        report.unit_hits[0].port = true;
        report.task_force_port_hovers.push(port::Hover {
            control: port::Control::Unbound,
            unit_hit: 0,
            rect: [4., 20., 16., 16.],
            hit: None,
            authored: None,
            supported: true,
            members: Arc::from([7]),
            metadata: port::Metadata {
                country: 1,
                visibility: 250,
                enemy: Ok(false),
                hold_fire: Some(false),
                idle: Some(false),
                no_regions: Some(false),
                pride: Some(false),
            },
        });
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Blocked)
        );
        let gui = content_reader_gui::gui::native::read(
            br#"guiTypes = { iconType = {
            name = unbound pdx_tooltip = TITLE
        } }"#,
        );
        report.task_force_port_hovers[0].authored =
            viewer_native_gui::counter_hovers::gui_authored::Authored::read(
                gui.template("unbound").unwrap(),
                false,
            );
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::PortTooltip(0))
        );
    }

    #[test]
    fn map_hits_follow_signed_layers_compose_order_and_actual_combat_attachment() {
        use viewer_native_gui::{counter_hovers::unit_hits::{Blocker, Hit, Kind}, sprites::layer};
        let rect = [0.0, 0.0, 20.0, 20.0];
        let point = [5.0, 5.0];
        let mut report = viewer_counter_kit::CounterReport::default();
        report.unit_hits.push(Hit {
            ours: true,
            port: false,
            rect,
            kind: Kind::Division,
            members: Arc::from([7]),
            layer: layer(-3, 0, 0),
            plate: None,
        });
        report.base_hits.push(Blocker {
            rect,
            layer: layer(-3, 0, 0),
        });
        report
            .faction_pin_hovers
            .push(viewer_counter_kit::PinHover {
                rect,
                theatre: (0, 0),
                regions: Arc::from([1]),
                layer: layer(-3, 0, 0),
            });
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Unit(0))
        );
        report.base_hits[0].layer = layer(4, 0, 0);
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Base(0))
        );
        report.faction_pin_hovers[0].layer = layer(5, 0, 0);
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Pin(0))
        );
        report.faction_pin_hovers.clear();
        let raid = viewer_raids::map::raids::tooltips::Hit {
            origin: [0.0, 0.0],
            size: [20.0, 20.0],
            key: viewer_raids::map::raids::tooltips::Key::Raid {
                id: 1,
                reference_type: 4,
                incoming: false,
            },
            province: 0,
            layer: layer(-2, 0, 0),
            supported: true,
        };
        assert_eq!(
            top_map_hit(point, &report, std::slice::from_ref(&raid), &[], None, None),
            Some(MapHit::Base(0))
        );
        report.base_hits.clear();
        assert_eq!(
            top_map_hit(point, &report, std::slice::from_ref(&raid), &[], None, None),
            Some(MapHit::Raid)
        );
        let mut same_layer_raid = raid.clone();
        same_layer_raid.layer = report.unit_hits[0].layer;
        assert_eq!(
            top_map_hit(
                point,
                &report,
                std::slice::from_ref(&same_layer_raid),
                &[],
                None,
                None
            ),
            Some(MapHit::Unit(0))
        );
        report.unit_hits[0].port = true;
        assert_eq!(
            top_map_hit(
                point,
                &report,
                std::slice::from_ref(&same_layer_raid),
                &[],
                None,
                None
            ),
            Some(MapHit::Raid)
        );
        report.unit_hits[0].port = false;
        report.combat_hits.push(CombatHit {
            selection: Selection::Land(1),
            rect,
            layer: layer(-1, 0, 0),
            order: 0,
            supported: true,
        });
        assert_eq!(
            top_map_hit(point, &report, std::slice::from_ref(&raid), &[], None, None),
            Some(MapHit::Combat(0))
        );
        report.combat_hits[0].layer = layer(-4, 0, 0);
        assert_eq!(
            top_map_hit(point, &report, std::slice::from_ref(&raid), &[], None, None),
            Some(MapHit::Raid)
        );
        report.combat_hits[0] = CombatHit {
            selection: Selection::Naval(1),
            rect,
            layer: layer(5, 0, 0),
            order: 1,
            supported: true,
        };
        let mut observer = report.combat_hits[0].clone();
        observer.order = 2;
        report.naval_marker_hovers.push(observer);
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::NavalObserver(0))
        );
        report.combat_hits[0].order = 2;
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Combat(0))
        );
        report.combat_hits[0].supported = false;
        report.naval_marker_hovers[0].supported = false;
        report
            .faction_pin_hovers
            .push(viewer_counter_kit::PinHover {
                rect,
                theatre: (0, 0),
                regions: Arc::from([1]),
                layer: layer(4, 0, 0),
            });
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Blocked)
        );
        report.combat_hits[0].supported = true;
        report.naval_marker_hovers[0].supported = true;
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Combat(0))
        );
        report.faction_pin_hovers.clear();
        report.combat_hits.clear();
        report.naval_marker_hovers.clear();
        let mut refused_raid = raid.clone();
        refused_raid.supported = false;
        assert_eq!(
            top_map_hit(
                point,
                &report,
                std::slice::from_ref(&refused_raid),
                &[],
                None,
                None
            ),
            Some(MapHit::Blocked)
        );
        assert_eq!(
            top_map_hit(
                [20.0, 5.0],
                &report,
                std::slice::from_ref(&raid),
                &[],
                None,
                None
            ),
            None
        );
        let viewport = [2048, 1152];
        let mut intel = viewer_raids::map::intel_icons::IntelHit {
            shapes: vec![
                viewer_native_gui::native_gui::HitGeometry::test_rectangle(
                    [0, 0],
                    [20, 20],
                    viewport,
                ),
            ],
            layer: layer(6, 0, 0),
            tooltip: Arc::new(Tooltip::default()),
        };
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, Some(&intel)),
            Some(MapHit::Intel)
        );
        intel.shapes[0].form = viewer_native_gui::native_gui::Hit::Alpha;
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, Some(&intel)),
            Some(MapHit::Intel)
        );
        assert_eq!(
            top_map_hit([30.0, 30.0], &report, &[], &[], None, Some(&intel)),
            None
        );
    }

    #[test]
    fn a_free_view_hits_the_nearest_shown_icon_in_its_own_layout() {
        use viewer_native_gui::{
            counter_hovers::unit_hits::{Blocker, Hit, Kind},
            free_icons::{Looks, Present},
            sprites::layer,
        };
        use viewer_gpu::icon_tracks::IconFamily;
        let mut report = viewer_counter_kit::CounterReport::default();
        report.unit_hits.push(Hit {
            ours: true,
            port: false,
            rect: [0.0, 0.0, 20.0, 20.0],
            kind: Kind::Division,
            members: Arc::from([7]),
            layer: layer(10, 0, 0),
            plate: None,
        });
        report.base_hits.push(Blocker {
            rect: [10.0, 0.0, 20.0, 20.0],
            layer: layer(3, 0, 0),
        });
        let point = [15.0, 5.0];
        assert_eq!(
            top_map_hit(point, &report, &[], &[], None, None),
            Some(MapHit::Unit(0))
        );
        let at = |rank, scale, alpha| Present {
            pivot: [10.0, 10.0],
            scale,
            alpha,
            rank,
        };
        let mut looks = Looks::default();
        looks.show(IconFamily::Columns, layer(10, 0, 0), at(0, 1.0, 1.0));
        looks.show(IconFamily::Bases, layer(3, 0, 0), at(1, 1.0, 1.0));
        assert_eq!(
            top_map_hit(point, &report, &[], &[], Some(&looks), None),
            Some(MapHit::Base(0))
        );
        let mut looks = Looks::default();
        looks.show(IconFamily::Columns, layer(10, 0, 0), at(0, 1.0, 1.0));
        looks.show(IconFamily::Bases, layer(3, 0, 0), at(1, 0.5, 1.0));
        assert_eq!(
            top_map_hit([18.0, 3.0], &report, &[], &[], Some(&looks), None),
            Some(MapHit::Unit(0))
        );
        let mut looks = Looks::default();
        looks.show(IconFamily::Bases, layer(3, 0, 0), at(1, 1.0, 0.2));
        assert_eq!(
            top_map_hit(point, &report, &[], &[], Some(&looks), None),
            None
        );
        let mut looks = Looks::default();
        looks.show(IconFamily::Combats, layer(10, 0, 0), at(0, 1.0, 1.0));
        assert_eq!(
            top_map_hit(point, &report, &[], &[], Some(&looks), None),
            None
        );
    }

    #[test]
    fn carrier_wing_controls_take_the_columns_transform_and_win_over_their_parent() {
        use viewer_native_gui::{
            counter_hovers::unit_hits::{Blocker, Hit, Kind},
            free_icons::{Looks, Present},
        };
        use viewer_gpu::icon_tracks::IconFamily;
        let mut report = viewer_counter_kit::CounterReport::default();
        report.base_hits.push(Blocker {
            rect: [10., 10., 20., 20.],
            layer: 7,
        });
        report.air_grid_hits.insert(
            0,
            viewer_counter_kit::AirGridHover {
                family: IconFamily::Columns,
                members: Some(Arc::new(vec![19])),
                text: Arc::default(),
            },
        );
        report.unit_hits.push(Hit {
            ours: true,
            port: false,
            rect: [0., 0., 40., 40.],
            kind: Kind::TaskForce,
            members: Arc::from([8]),
            layer: 7,
            plate: None,
        });
        let mut looks = Looks::default();
        looks.show(
            IconFamily::Columns,
            7,
            Present {
                pivot: [0., 0.],
                scale: 2.,
                alpha: 1.,
                rank: 0,
            },
        );
        assert_eq!(
            top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None),
            Some(MapHit::Base(0))
        );
        report.air_grid_hits.get_mut(&0).unwrap().family = IconFamily::Bases;
        assert_eq!(
            top_map_hit([50., 50.], &report, &[], &[], Some(&looks), None),
            Some(MapHit::Unit(0))
        );
    }

    #[test]
    fn overlapping_land_and_naval_units_keep_separate_tooltip_namespaces() {
        use viewer_native_gui::counter_hovers::{
            land::{CounterControl, CounterHover},
            unit_hits::{Hit, Kind},
        };
        let members = Arc::from([7]);
        let hover = CounterHover {
            control: CounterControl::Normal,
            rect: [0.0, 0.0, 20.0, 20.0],
            authored: None,
            remaining: None,
            ours: true,
            coarse: false,
            hidden: false,
            railway_gun: false,
            members: Arc::clone(&members),
            timing: Arc::from([]),
        };
        let mut unit = Hit {
            ours: true,
            port: false,
            rect: hover.rect,
            kind: Kind::TaskForce,
            members,
            layer: 0,
            plate: None,
        };
        assert!(!land_hover_matches_unit(&hover, &unit));
        unit.kind = Kind::Division;
        assert!(land_hover_matches_unit(&hover, &unit));
        unit.kind = Kind::RailwayGun;
        assert!(!land_hover_matches_unit(&hover, &unit));
        let gun_hover = CounterHover {
            railway_gun: true,
            ..hover
        };
        assert!(land_hover_matches_unit(&gun_hover, &unit));
    }

    #[test]
    fn a_map_without_a_picture_stops_sounding() {
        viewer_audio::System::silent();
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        viewer.dashboard_map = true;
        viewer.drew_frame = true;
        let mut owner = viewer_audio::System::shared().owner();
        owner.claim();
        viewer.audio = Some(owner);
        let bounds = dashboard_core::protocol::ViewBounds {
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        };
        let target = dashboard_core::protocol::DashboardMapView {
            options: Default::default(),
            id: "widget".into(),
            source_id: "source".into(),
            path: "fixture".into(),
            revision: "revision".into(),
            branch_id: "1".into(),
            playing: false,
            hours_per_second: 1.0,
            hour: 0,
            fraction: 0.,
            sequence: 1,
            seek_sequence: None,
            composed_at: 0.0,
            clock_id: "page-1".into(),
            bounds,
            clip: bounds,
            visible: true,
            focus_province: None,
            battle: None,
        };
        let frame = eframe::Frame::_new_kittest();
        let mut output = egui::Context::default()
            .run_ui(Default::default(), |ui| viewer.map_ui(ui, &frame, &target));
        output.textures_delta.clear();
        assert!(viewer.error.is_some());
        assert!(viewer.drew_frame());
        assert!(!viewer.audio.as_ref().unwrap().owns());
        assert!(viewer.audio_reset);
        viewer.reset_drawn_frame();
        let mut output = egui::Context::default()
            .run_ui(Default::default(), |ui| viewer.map_ui(ui, &frame, &target));
        output.textures_delta.clear();
        assert!(!viewer.drew_frame());
    }

    #[test]
    fn host_reanchors_keep_playback_forward_but_seeks_and_pauses_exact() {
        let current = Playhead {
            branch: 1,
            hour: 20,
            fraction: 0.75,
        };
        for target in [
            Playhead {
                hour: 19,
                ..current
            },
            Playhead {
                fraction: 0.25,
                ..current
            },
        ] {
            assert_eq!(forward_only(Some(current), target, true), current);
            assert_eq!(forward_only(Some(current), target, false), target);
        }
        let later = Playhead {
            hour: 21,
            fraction: 0.125,
            ..current
        };
        assert_eq!(forward_only(Some(current), later, true), later);
        let branch = Playhead {
            branch: 2,
            hour: 10,
            ..current
        };
        assert_eq!(forward_only(Some(current), branch, true), branch);
        assert_eq!(forward_only(None, current, true), current);
    }

    #[test]
    fn a_playing_anchor_is_placed_by_the_time_the_page_composed_it() {
        let target = Playhead {
            branch: 1,
            hour: 20,
            fraction: 0.5,
        };
        let placed = placed_anchor(target, true, 1_440.0, Duration::from_millis(250), 100_000);
        assert_eq!(placed.hour, 380);
        assert!((placed.fraction - 0.5).abs() < 1e-9, "{placed:?}");
        assert_eq!(
            placed_anchor(target, true, 1_440.0, Duration::ZERO, 100_000),
            target,
            "an anchor that arrived at once stands where it was composed"
        );
        assert_eq!(
            placed_anchor(target, false, 1_440.0, Duration::from_millis(250), 100_000),
            target,
            "a seek and a pause stand exactly where the page composed them"
        );
        assert_eq!(
            placed_anchor(target, true, 1_440.0, Duration::from_secs(10), 300).hour,
            300,
            "never past the branch's last hour"
        );
    }

    #[test]
    fn late_anchors_do_not_freeze_frames_and_controls_remain_exact() {
        let now = Instant::now();
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        let host = Playhead {
            branch: 1,
            hour: 100,
            fraction: 0.0,
        };
        viewer.last_step = now;
        viewer.anchor_playback(host, true, 4.0, true, None, now, 1_000);
        for frame in 1..=60 {
            let at = now + Duration::from_secs_f64(f64::from(frame) / 60.0);
            if frame % 3 == 0 {
                let delayed = Playhead {
                    fraction: 0.0,
                    hour: 100 + (frame - 3) / 15,
                    ..host
                };
                viewer.anchor_playback(delayed, true, 4.0, false, None, at, 1_000);
            }
            viewer.tick_playhead(at.duration_since(viewer.last_step).as_secs_f64(), 1_000);
            viewer.last_step = at;
            let head = viewer.playhead.unwrap();
            let position = f64::from(head.hour) + head.fraction;
            assert!((position - (100.0 + f64::from(frame) / 15.0)).abs() < 1e-7);
        }
        viewer.anchor_playback(host, false, 4.0, false, None, now + Duration::from_secs(1), 1_000);
        assert_eq!(viewer.playhead, Some(host));
        assert!(!viewer.playing);
        viewer.anchor_playback(host, true, 0.25, false, None, now + Duration::from_secs(1), 1_000);
        assert_eq!(viewer.playhead, Some(host));
        viewer.tick_playhead(0.5, 1_000);
        assert_eq!(viewer.playhead.unwrap().fraction, 0.125);
        viewer.anchor_playback(host, true, 0.25, true, None, now + Duration::from_secs(1), 1_000);
        assert_eq!(viewer.playhead, Some(host));
        assert!(!tick_clock(
            viewer.playhead.as_mut().unwrap(),
            1440.0,
            1.0,
            1_000
        ));
        assert_eq!(viewer.playhead.unwrap().hour, 1_000);
    }

    #[test]
    fn late_playing_anchors_cannot_rewind_a_clock_at_the_branch_end() {
        let now = Instant::now();
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        viewer.last_step = now;
        let head = Playhead {
            branch: 1,
            hour: 99,
            fraction: 0.0,
        };
        viewer.anchor_playback(head, true, 4.0, true, None, now, 100);
        viewer.tick_playhead(0.5, 100);
        assert!(!viewer.playing);
        let delayed = Playhead {
            fraction: 0.5,
            ..head
        };
        viewer.anchor_playback(
            delayed,
            true,
            4.0,
            false,
            None,
            now + Duration::from_millis(500),
            100,
        );
        assert_eq!(viewer.playhead.unwrap().hour, 100);
        assert!(!viewer.playing);
    }

    #[test]
    fn every_transport_action_places_its_late_anchor_without_moving_the_map_backwards() {
        const LAST: i32 = 100_000;
        let now = Instant::now();
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        viewer.last_step = now;
        let head = |position: f64| Playhead {
            branch: 1,
            hour: position.floor() as i32,
            fraction: position - position.floor(),
        };
        let position = |viewer: &Viewer| {
            let head = viewer.playhead.expect("a clock");
            f64::from(head.hour) + head.fraction
        };
        let tick_to = |viewer: &mut Viewer, ms: u64| {
            let at = now + Duration::from_millis(ms);
            viewer.tick_playhead(at.duration_since(viewer.last_step).as_secs_f64(), LAST);
            viewer.last_step = at;
        };
        let send = |viewer: &mut Viewer, at: f64, playing, speed, sought, composed: u64, arrived: u64| {
            viewer.anchor_playback(
                head(at),
                playing,
                speed,
                sought,
                Some(crate::page_clock::ClockSample {
                    origin: "page-1",
                    composed_at: 5_000.0 + composed as f64,
                }),
                now + Duration::from_millis(arrived),
                LAST,
            );
        };
        send(&mut viewer, 1_000.0, true, 1_440.0, true, 0, 0);
        assert_eq!(position(&viewer), 1_000.0);
        assert!(viewer.playing);
        tick_to(&mut viewer, 120);
        assert!((position(&viewer) - 1_172.8).abs() < 1e-6, "{}", position(&viewer));
        send(&mut viewer, 1_144.0, true, 24.0, false, 100, 120);
        assert!((position(&viewer) - 1_172.8).abs() < 1e-6, "a speed change never steps back: {}", position(&viewer));
        assert_eq!(viewer.speed, 24.0);
        tick_to(&mut viewer, 1_120);
        send(&mut viewer, 1_168.48, false, 24.0, false, 1_120, 1_140);
        assert_eq!(viewer.playhead, Some(head(1_168.48)), "a pause stands where it was composed");
        assert!(!viewer.playing);
        send(&mut viewer, 1_168.48, true, 1_440.0, false, 1_200, 1_240);
        assert!((position(&viewer) - 1_226.08).abs() < 1e-4, "{}", position(&viewer));
        assert!(viewer.playing);
        send(&mut viewer, 500.0, true, 1_440.0, true, 1_300, 1_400);
        assert_eq!(viewer.playhead, Some(head(500.0)), "a seek stands where it was composed");
        send(&mut viewer, 501.0, false, 1_440.0, true, 1_500, 1_560);
        assert_eq!(viewer.playhead, Some(head(501.0)));
        assert!(!viewer.playing);
    }

    #[test]
    fn fractional_host_rates_advance_every_frame_and_finish_exactly() {
        for speed in [0.25, 24.0, 720.0, 1440.25] {
            let mut head = Playhead {
                branch: 1,
                hour: 100,
                fraction: 0.375,
            };
            for _ in 0..240 {
                let fraction = head.fraction + speed / 240.0;
                assert!(advance_clock(&mut head, fraction, 10_000));
            }
            let position = f64::from(head.hour) + head.fraction;
            assert!(
                (position - (100.375 + speed)).abs() < 0.0001,
                "{speed}: {position}"
            );
            assert!(!advance_clock(&mut head, 10_000.0, 10_000));
            assert_eq!((head.hour, head.fraction), (10_000, 0.0));
        }
    }

    #[test]
    fn the_playback_clock_runs_without_the_builders_and_stops_at_the_end() {
        let mut head = Playhead {
            branch: 1,
            hour: 10,
            fraction: 0.0,
        };
        assert!(advance_clock(&mut head, 6.25, 30));
        assert_eq!((head.hour, head.fraction), (16, 0.25));
        assert!(!advance_clock(&mut head, 100.0, 30));
        assert_eq!((head.hour, head.fraction), (30, 0.0));
        head.hour = i32::MIN;
        assert!(advance_clock(&mut head, 3_000_000_000.25, i32::MAX));
        assert_eq!((head.hour, head.fraction), (852_516_352, 0.25));
    }

    #[test]
    fn unload_releases_campaign_map_admission_and_discards_a_late_loader() {
        use replay::campaign::Replay;
        use replay_capture_spool::spool::Record;
        use replay_data::{
            identity::CaptureMode, identity::Identity, identity::Roster, identity::SampleBoundary,
            identity::SessionStart, identity::Verification,
        };
        let path = PathBuf::from("memory.battleplan");
        let session = SessionStart {
            id: 1,
            process_id: 1,
            process_start_time: 1,
            qpc_frequency: 10_000_000,
            sample_boundary: SampleBoundary::EndOfHour,
            capture_mode: CaptureMode::Full,
            identity: Identity {
                game_version: "synthetic".into(),
                exe_hash: [1; 32],
                exe_size: 1,
                vanilla_hash: [2; 32],
                dlc: vec![],
                mods: vec![],
                coverage_policy: 1,
                language: "l_english".into(),
                recorder_version: "test".into(),
                collector_hash: [3; 32],
                verification: Verification::Verified {
                    launcher: Roster::default(),
                    memory: Roster::default(),
                },
            },
        };
        let replay = Arc::new(
            Replay::from_records(
                recorder_derive::writers(),
                b"unload-test".to_vec(),
                &[vec![Record::SessionStart(session)]],
            )
            .unwrap(),
        );
        let source = Arc::new(
            dashboard_base::Source::from_replay(
                dashboard_base::SourceKey {
                    source_id: "source-test".into(),
                    revision: "revision-test".into(),
                },
                path.clone(),
                replay,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap(),
        );
        let witness = Arc::downgrade(&source);
        let mut viewer = Viewer::create(
            Config::default(),
            Options {
                replay: Some(path.clone()),
                ..Default::default()
            },
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        viewer.source = Some(source);
        assert!(!viewer.unload_recording(Path::new("another.battleplan")));
        assert_eq!(witness.strong_count(), 1);
        assert!(viewer.unload_recording(&path));
        assert_eq!(witness.strong_count(), 0);
        let (send, receive) = mpsc::channel();
        viewer.loading = Some(receive);
        send.send(Err("obsolete load failure".into())).unwrap();
        viewer.poll(&egui::Context::default());
        assert!(viewer.loading.is_none());
        assert!(viewer.error.is_none());
        assert!(viewer.options.replay.is_none());
    }

    fn export(path: &str) -> (Capture, crate::host::tests::Answers) {
        let (job, answers) = crate::host::tests::job(path);
        (Capture { path: path.into(), remaining: 0, pinned: None, exit: false, time: None, step: 0.0, frames: 1, index: 0, paced: false, export: Some(job), video: None }, answers)
    }

    use crate::host::tests::taken;

    fn closes(output: &egui::FullOutput) -> bool {
        output.viewport_output.values().any(|viewport| viewport.commands.iter().any(|command| matches!(command, egui::ViewportCommand::Close)))
    }

    #[test]
    fn failed_replay_load_ends_every_capture_by_name() {
        for disconnected in [false, true] {
            for window_dump in [false, true] {
                let failure = Arc::new(Mutex::new(None));
                let mut viewer = Viewer::create(
                    Config::default(),
                    Options { replay: Some("broken.battleplan".into()), out: Some("capture.png".into()), window_dump, ..Default::default() },
                    None,
                    Arc::clone(&failure),
                    None,
                    None,
                );
                let (capture, answers) = export("campaign-map.png");
                if !window_dump {
                    viewer.capture = Some(capture);
                }
                let (send, receive) = mpsc::channel();
                viewer.loading = Some(receive);
                let expected = if disconnected {
                    "Replay loader stopped before returning a result"
                } else {
                    send.send(Err("unsupported replay format".into())).unwrap();
                    "unsupported replay format"
                };
                drop(send);
                let ctx = egui::Context::default();
                let mut output = ctx.run_ui(Default::default(), |ui| viewer.poll(ui.ctx()));
                output.textures_delta.clear();
                assert!(viewer.loading.is_none());
                assert!(viewer.capture.is_none(), "no capture stays armed");
                assert_eq!(viewer.error.as_deref(), Some(expected));
                assert_eq!(closes(&output), window_dump);
                let reported = failure.lock().unwrap().clone();
                let answered = taken(&answers);
                if window_dump {
                    let message = reported.unwrap();
                    assert!(message.contains("capture.png") && message.contains("replay could not be loaded") && message.contains(expected), "{message}");
                    assert!(answered.is_empty());
                } else {
                    assert!(reported.is_none(), "an export fails no process");
                    assert!(matches!(answered.as_slice(), [Err(why)] if why.starts_with("Capture campaign-map.png failed: replay could not be loaded") && why.contains(expected)), "{answered:?}");
                }
            }
        }
    }

    #[test]
    fn a_capture_in_a_recorded_gap_is_refused_by_name() {
        for window_dump in [false, true] {
            let failure = Arc::new(Mutex::new(None));
            let mut viewer = Viewer::create(
                Config::default(),
                Options { out: Some("unavailable.png".into()), window_dump, ..Default::default() },
                None,
                Arc::clone(&failure),
                None,
                None,
            );
            let (capture, answers) = export("export.png");
            if !window_dump {
                viewer.capture = Some(capture);
            }
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                viewer.hour_error(Some("hour 12 was not recorded".into()), ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(closes(&output), window_dump);
            assert!(viewer.capture.is_none(), "no capture stays armed in the gap");
            assert_eq!(viewer.map_error.as_deref(), Some("hour 12 was not recorded"));
            let reported = failure.lock().unwrap().clone();
            let answered = taken(&answers);
            if window_dump {
                assert_eq!(reported.as_deref(), Some("Capture unavailable.png failed: hour 12 was not recorded"));
                assert!(answered.is_empty());
            } else {
                assert!(reported.is_none(), "an export fails no process");
                assert_eq!(answered, [Err("Capture export.png failed: hour 12 was not recorded".to_owned())]);
                assert!(viewer.error.is_none(), "nor becomes the map's error");
            }
        }
    }

    #[test]
    fn an_export_whose_recording_closes_answers_its_requester() {
        let mut viewer = Viewer::create(Config::default(), Options { replay: Some("campaign.battleplan".into()), ..Default::default() }, None, Arc::default(), None, None);
        let (capture, answers) = export("map.png");
        viewer.capture = Some(capture);
        assert!(viewer.unload_recording(Path::new("campaign.battleplan")));
        assert!(viewer.capture.is_none());
        assert_eq!(taken(&answers), [Err("Capture map.png failed: its recording closed".to_owned())]);
    }

    #[test]
    fn an_export_the_map_cannot_start_answers_its_requester() {
        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let (job, answers) = crate::host::tests::job("map.png");
        viewer.host_export(job, &ctx, &mut frame);
        assert_eq!(taken(&answers), [Err("Open a map and wait for any active export to finish".to_owned())]);
        assert!(viewer.error.is_none() && viewer.capture.is_none());
    }

    #[test]
    fn an_admitted_export_still_renders_once_battleplan_hides_its_window_and_the_exit_waits_for_its_picture() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        let (parent, _) = crate::host::Window::create(None, [64, 64], "BattlePlan (test host)").unwrap();
        // SAFETY: the window was created on this thread and is still alive.
        unsafe { ShowWindow(parent.hwnd() as _, SW_HIDE) };
        let (window, _inputs) = crate::host::Window::create(Some(parent.hwnd()), [1, 1], crate::host::MAP_WINDOW_TITLE).unwrap();
        let (_commands, received) = mpsc::channel();
        let ctx = egui::Context::default();
        let service = dashboard::Service::new(Arc::new(|_| true)).unwrap();
        let controller = crate::embedded::Parent::new(window.clone(), received, Arc::new(|_| {}), service).attach(&ctx).unwrap();
        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), Some(controller), None);
        assert!(!viewer.embedded.as_ref().unwrap().visible(), "the map’s window reports itself hidden, as it does once BattlePlan hides its own");
        let exit: &'static replay_settings::exit::Exit = Box::leak(Box::new(replay_settings::exit::Exit::new()));
        let (job, answers) = crate::host::tests::job_under(exit, "map.png");
        viewer.capture = Some(Capture { path: "map.png".into(), remaining: 0, pinned: None, exit: false, time: None, step: 0.0, frames: 1, index: 0, paced: false, export: Some(job), video: None });
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(Default::default(), |ui| eframe::App::logic(&mut viewer, ui.ctx(), &mut frame));
        output.textures_delta.clear();
        assert!(taken(&answers).is_empty(), "the export the host accepted isn’t refused because its window hid");
        assert!(viewer.capture.as_ref().is_some_and(|capture| capture.export.is_some()), "it stays armed to render");
        exit.seal();
        assert!(!exit.empty_within(std::time::Duration::ZERO), "and the exit waits for its picture");
        viewer.refused("the test ends");
        assert_eq!(taken(&answers).len(), 1, "the job is answered");
        assert!(exit.empty_within(std::time::Duration::ZERO), "and lets the exit go once it is");
        let (job, answers) = crate::host::tests::job("map.png");
        viewer.capture = Some(Capture { path: "map.png".into(), remaining: 0, pinned: None, exit: false, time: None, step: 0.0, frames: 1, index: 0, paced: false, export: Some(job), video: None });
        let mut output = ctx.run_ui(Default::default(), |ui| {
            eframe::App::logic(&mut viewer, ui.ctx(), &mut frame);
            eframe::App::ui(&mut viewer, ui, &mut frame);
        });
        output.textures_delta.clear();
        let answered = taken(&answers);
        assert!(answered.iter().all(|answer| answer.as_ref().err().is_none_or(|error| !error.contains("hidden"))), "the frame’s drawing pass doesn’t refuse it for the hidden window either: {answered:?}");
        window.destroy();
        parent.destroy();
    }

    fn window_dump_viewer() -> Viewer {
        Viewer::create(
            Config::default(),
            Options { out: Some("frame.png".into()), window_dump: true, ..Default::default() },
            None,
            Arc::default(),
            None,
            None,
        )
    }

    fn remaining(viewer: &Viewer) -> Option<u32> {
        viewer.capture.as_ref().map(|capture| capture.remaining)
    }

    #[test]
    fn a_saved_frame_whose_overlay_the_art_moved_under_paints_it_again() {
        let mut viewer = window_dump_viewer();
        viewer.capture.as_mut().expect("a window dump captures").remaining = 0;
        viewer.overlay_fraction = Some(0.0);
        viewer.hold_saved_frame(10, Some(true), None, &egui::Context::default()).unwrap();
        assert_eq!(remaining(&viewer), Some(1), "not saved over a layout older than its art");
        assert_eq!(viewer.overlay_fraction, None, "the next frame paints the overlay");
        assert!(viewer.overlay_art_moved, "and lays its hour out again over the art");

        viewer.capture.as_mut().expect("still capturing").remaining = 0;
        viewer.overlay_fraction = Some(0.0);
        viewer.overlay_art_moved = false;
        viewer.hold_saved_frame(10, Some(false), None, &egui::Context::default()).unwrap();
        assert_eq!(remaining(&viewer), Some(0), "laid out over its art, the frame is saved");
        assert_eq!(viewer.overlay_fraction, Some(0.0), "and nothing is painted again");
        assert!(!viewer.overlay_art_moved);

        viewer.hold_saved_frame(10, None, None, &egui::Context::default()).unwrap();
        assert_eq!(remaining(&viewer), Some(1), "no overlay yet: it waits for one");
    }

    #[test]
    fn a_saved_frame_whose_art_never_stops_moving_is_refused_by_name() {
        let mut viewer = window_dump_viewer();
        viewer.capture.as_mut().expect("a window dump captures").remaining = 0;
        let refused = (0..=SETTLES).find_map(|_| viewer.hold_saved_frame(10, Some(true), None, &egui::Context::default()).err());
        let refused = refused.expect("refused within its frames");
        assert!(refused.contains("hour 10"), "{refused}");
        assert!(viewer.capture.is_none(), "and the dump ends unsaved");
        assert!(viewer.failure.lock().unwrap().as_deref().is_some_and(|failure| failure.contains("hour 10")), "reporting why");

        let mut viewer = window_dump_viewer();
        for _ in 0..SETTLES * 3 {
            viewer.hold_saved_frame(10, Some(true), None, &egui::Context::default()).unwrap();
            viewer.hold_saved_frame(10, Some(false), None, &egui::Context::default()).unwrap();
        }
        assert_eq!(viewer.overlay_art_retries, 0, "a layout that caught up ends the run of retries");
    }

    #[test]
    fn a_refused_export_answers_its_requester_and_the_next_one_starts() {
        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        let (capture, answers) = export("first.png");
        viewer.capture = Some(capture);
        viewer.error = Some("an independent map error".into());
        let ctx = egui::Context::default();
        let mut frames = Vec::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            frames = (0..=SETTLES).map(|_| viewer.hold_saved_frame(10, Some(true), None, ui.ctx())).collect();
        });
        output.textures_delta.clear();
        assert!(frames.iter().all(Result::is_ok), "an export's refusal is no error of the map's frame");
        assert!(viewer.capture.is_none(), "the refused export is no longer armed");
        assert_eq!((viewer.overlay_art_retries, viewer.overlay_art_moved), (0, false), "nor its retries");
        let answered = taken(&answers);
        assert!(matches!(answered.as_slice(), [Err(why)] if why.starts_with("Capture first.png failed: ") && why.contains("hour 10")), "{answered:?}");
        assert_eq!(viewer.error.as_deref(), Some("an independent map error"), "the map's own error stays as it was");
        assert!(viewer.failure.lock().unwrap().is_none(), "an export fails no process");
        assert!(!closes(&output), "and closes no window");

        viewer.capture = Some(export("second.png").0);
        viewer.overlay_fraction = Some(0.0);
        viewer.hold_saved_frame(10, Some(true), None, &ctx).unwrap();
        assert!(viewer.capture.is_some(), "the next export starts");
        assert_eq!(viewer.overlay_art_retries, 1, "with retries of its own");
    }

    #[test]
    fn a_saved_frame_held_past_its_bound_is_refused_naming_what_it_waited_for() {
        let waiting = || Some("its raid offers had evaluated 100 of 616 targets".to_owned());
        let ctx = egui::Context::default();
        let mut viewer = window_dump_viewer();
        viewer.hold_saved_frame_within(10, Some(false), waiting(), HELD_GIVEN_UP, &ctx).unwrap();
        assert!(viewer.capture.is_some(), "held within its bound");
        let refused = viewer.hold_saved_frame_within(10, Some(false), waiting(), Duration::ZERO, &ctx).unwrap_err();
        assert!(refused.contains("hour 10") && refused.contains("100 of 616 targets"), "{refused}");
        assert!(viewer.capture.is_none(), "the dump ends unsaved");
        assert!(viewer.failure.lock().unwrap().as_deref().is_some_and(|failure| failure.contains("100 of 616 targets")));

        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        let (capture, answers) = export("map.png");
        viewer.capture = Some(capture);
        viewer.hold_saved_frame_within(10, Some(false), waiting(), Duration::ZERO, &ctx).unwrap();
        assert!(viewer.capture.is_none());
        let answered = taken(&answers);
        assert!(matches!(answered.as_slice(), [Err(why)] if why.contains("100 of 616 targets")), "{answered:?}");

        let mut viewer = window_dump_viewer();
        viewer.capture.as_mut().expect("a window dump captures").remaining = 1000;
        viewer.hold_saved_frame_within(10, Some(false), waiting(), Duration::ZERO, &ctx).unwrap();
        assert!(viewer.capture.is_some());
    }

    #[test]
    fn a_saved_frame_that_cannot_draw_its_hour_is_refused_by_name() {
        let ctx = egui::Context::default();
        let mut viewer = window_dump_viewer();
        let no_picture = || "no picture of it was composed".to_owned();
        viewer.wait_to_draw_within(10, no_picture, HELD_GIVEN_UP, &ctx).unwrap();
        assert!(viewer.capture.is_some(), "waited within its bound");
        let refused = viewer.wait_to_draw_within(10, no_picture, Duration::ZERO, &ctx).unwrap_err();
        assert!(refused.contains("hour 10") && refused.contains("no picture"), "{refused}");
        assert!(viewer.failure.lock().unwrap().as_deref().is_some_and(|failure| failure.contains("no picture")));

        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        let (capture, answers) = export("map.png");
        viewer.capture = Some(capture);
        let no_area = || "its map had no area to draw in (0 by 0 points)".to_owned();
        viewer.wait_to_draw_within(10, no_area, Duration::ZERO, &ctx).unwrap();
        assert!(viewer.capture.is_none());
        assert!(matches!(taken(&answers).as_slice(), [Err(why)] if why.contains("no area to draw in")));
    }

    #[test]
    fn an_export_answers_with_the_hour_its_frame_drew() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let mut viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        viewer.last_step = now;
        viewer.anchor_playback(Playhead { branch: 1, hour: 10, fraction: 0.0 }, false, 4.0, true, None, now, 1_000);
        let (capture, answers) = export("map.png");
        viewer.capture = Some(capture);
        viewer.anchor_playback(Playhead { branch: 1, hour: 40, fraction: 0.0 }, true, 4.0, true, None, now + Duration::from_secs(1), 1_000);
        assert!(viewer.capture.is_some() && taken(&answers).is_empty(), "a seek refuses no export");
        let capture = viewer.end_capture().expect("still armed");
        viewer.capture_written(capture, Path::new("map.png"), (1, 41), Ok(None), &ctx);
        assert_eq!(taken(&answers), [Ok("Saved map.png: branch 1, hour 41".to_owned())]);

        let (capture, answers) = export("unconfirmed.png");
        viewer.capture = Some(capture);
        let capture = viewer.end_capture().unwrap();
        viewer.capture_written(capture, Path::new("unconfirmed.png"), (1, 41), Ok(Some("Windows didn’t confirm it reached the disk".to_owned())), &ctx);
        assert_eq!(
            crate::host::tests::taken_saved(&answers),
            [Ok(crate::host::Saved { message: "Saved unconfirmed.png: branch 1, hour 41".to_owned(), warning: Some("Windows didn’t confirm it reached the disk".to_owned()) })],
            "a picture Windows replaced is reported saved, with why it may not last as a field of its own"
        );
        assert_eq!(
            viewer.capture_note.as_deref(),
            Some("Saved unconfirmed.png: branch 1, hour 41, but Windows didn’t confirm it reached the disk"),
            "and Map Diagnostics says so too"
        );

        let (capture, answers) = export("full.png");
        viewer.capture = Some(capture);
        let capture = viewer.end_capture().unwrap();
        viewer.capture_written(capture, Path::new("full.png"), (1, 41), Err("disk full".to_owned()), &ctx);
        assert_eq!(taken(&answers), [Err("full.png: disk full".to_owned())]);
    }

    #[test]
    fn a_window_dump_closed_before_it_saved_fails_by_name() {
        let viewer = window_dump_viewer();
        let failure = Arc::clone(&viewer.failure);
        drop(viewer);
        assert!(failure.lock().unwrap().as_deref().is_some_and(|failure| failure.contains("frame.png") && failure.contains("window closed")));
        let viewer = Viewer::create(Config::default(), Options::default(), None, Arc::default(), None, None);
        let failure = Arc::clone(&viewer.failure);
        drop(viewer);
        assert!(failure.lock().unwrap().is_none(), "no capture, no failure");
    }

    #[test]
    fn a_saved_frame_waits_for_its_combat_window_and_refuses_one_that_cannot_open() {
        let mut viewer = window_dump_viewer();
        assert_eq!(viewer.saved_combat(true, None), Ok(true), "no window asked for");
        viewer.combat_rows.pending = Some(viewer_combat::Selector::Id(5));
        assert_eq!(viewer.saved_combat(true, None), Ok(false), "the combat named at load is not resolved yet");
        viewer.combat_rows.pending = None;
        viewer.combat_rows.selector_refused = Some("no recorded combat matches Id(5) at hour 10".into());
        assert!(viewer.saved_combat(true, None).is_err_and(|why| why.contains("Id(5)")), "it matched nothing: refused by name");
    }

    #[test]
    fn a_window_named_at_load_that_reads_no_rows_opens_without_them() {
        use viewer_combat::Selector;
        for selector in [Selector::AirRegion(8), Selector::State(362), Selector::NavalRegion(4)] {
            let mut viewer = window_dump_viewer();
            viewer.combat_rows.pending = Some(selector);
            let _ = viewer.combat_prepared();
            assert!(viewer.combat_rows.pending.is_none(), "{selector:?} is not left waiting for rows");
            assert_eq!(viewer.combat.selection, selector.direct(), "{selector:?} opens its window");
        }
    }

    #[test]
    fn a_combat_named_at_load_that_cannot_be_looked_up_is_refused_within_a_bound() {
        let mut viewer = window_dump_viewer();
        viewer.combat_rows.pending = Some(viewer_combat::Selector::Province(362));
        for _ in 0..100 {
            assert_eq!(viewer.saved_combat(false, None), Ok(false), "no picture yet: it waits");
        }
        let refused = (0..=SETTLES).find_map(|_| viewer.saved_combat(true, None).err());
        assert!(refused.is_some_and(|why| why.contains("Province(362)") && why.contains("LandCombats")), "refused by name within its frames");
    }

    #[test]
    fn a_named_window_drawn_empty_while_its_art_arrives_is_drawn_again() {
        let empty = || Some("the window drew no quads".to_owned());
        assert_eq!(combat_left_out(true, empty(), true), (None, true), "its art moved: drawn again");
        assert_eq!(combat_left_out(true, empty(), false), (empty(), false), "its art stands still: refused");
        assert_eq!(combat_left_out(true, None, true), (None, false), "drawn: nothing to wait for");
        assert!(combat_left_out(false, None, true).0.is_some(), "a hidden interface is refused whatever the art");
    }

    #[test]
    fn unloading_a_recording_retires_what_the_combat_window_took_from_its_content() {
        let mut viewer = Viewer::create(Config::default(), Options { replay: Some("campaign.battleplan".into()), ..Default::default() }, None, Arc::default(), None, None);
        viewer.combat.set_navy_sources_of(Arc::new(|_: &replay_frame::frame::DecodedFrame, _, _| Ok(std::collections::BTreeMap::new())));
        viewer.combat_inputs = Some(1);
        assert!(viewer.unload_recording(Path::new("campaign.battleplan")));
        assert!(!viewer.combat.holds_inputs(), "the provider went with the recording");
        assert_eq!(viewer.combat_inputs, None, "and is given again for the next content");
        viewer.combat.set_navy_sources_of(Arc::new(|_: &replay_frame::frame::DecodedFrame, _, _| Ok(std::collections::BTreeMap::new())));
        viewer.combat_inputs = Some(2);
        drop(viewer.replace_document(None));
        assert!(!viewer.combat.holds_inputs(), "a reload lets go of the content before");
        assert_eq!(viewer.combat_inputs, None);
    }

    #[test]
    fn a_combat_named_at_load_that_is_not_drawn_is_refused() {
        let mut viewer = window_dump_viewer();
        viewer.combat.open(viewer_engagements::combat_selection::Selection::Naval(2));
        viewer.combat.close();
        assert_eq!(viewer.saved_combat(true, None), Ok(true), "not named at load: a closed window is no window");
        viewer.options.combat = Some(viewer_combat::Selector::Id(2));
        assert!(viewer.saved_combat(true, None).is_err(), "named at load and closed: refused");
    }

    #[test]
    fn a_capture_far_from_its_readback_lays_nothing_out_again() {
        let mut viewer = window_dump_viewer();
        viewer.capture.as_mut().expect("a window dump captures").remaining = 1000;
        viewer.overlay_fraction = Some(0.0);
        viewer.hold_saved_frame(10, Some(true), None, &egui::Context::default()).unwrap();
        assert_eq!(remaining(&viewer), Some(1000));
        assert_eq!(viewer.overlay_fraction, Some(0.0), "nothing painted again");
        assert!(!viewer.overlay_art_moved, "nor laid out again");
        assert_eq!(viewer.overlay_art_retries, 0);
    }

    #[test]
    fn seek_from_stale_host_state_reopens_its_explicit_snapshot() {
        let seek = crate::embedded::Seek {
            path: PathBuf::from("A.battleplan"),
            revision: "100:20:0".into(),
            branch: 1,
            hour: 12,
            playback: None,
        };
        let target = seek.open_request();
        let a = Path::new("A.battleplan");
        let b = Path::new("B.battleplan");

        assert_eq!(
            seek_open_target(&seek, Some((b, Some("100:20:0"))), None),
            Some(target.clone())
        );
        assert_eq!(
            seek_open_target(&seek, None, Some((b, None))),
            Some(target.clone())
        );
        assert_eq!(
            seek_open_target(
                &seek,
                Some((a, Some("100:20:0"))),
                Some((b, Some("120:30:0"))),
            ),
            Some(target.clone())
        );
        assert_eq!(seek_open_target(&seek, None, None), Some(target));
        assert_eq!(
            seek_open_target(&seek, Some((a, Some("100:20:0"))), None),
            None
        );
    }

    #[test]
    fn seek_reuses_only_the_exact_accepted_or_pending_revision() {
        let seek = crate::embedded::Seek {
            path: PathBuf::from("A.battleplan"),
            revision: "120:30:0".into(),
            branch: u64::MAX,
            hour: 12,
            playback: None,
        };
        let path = seek.path.as_path();
        for revision in [None, Some("100:20:0")] {
            assert_eq!(
                seek_open_target(&seek, Some((path, revision)), None),
                Some(seek.open_request())
            );
            assert_eq!(
                seek_open_target(
                    &seek,
                    Some((path, Some("120:30:0"))),
                    Some((path, revision)),
                ),
                Some(seek.open_request())
            );
            assert!(!seek.matches(path, revision));
        }
        assert_eq!(
            seek_open_target(&seek, None, Some((path, Some("120:30:0")))),
            None
        );
        assert!(seek.matches(path, Some("120:30:0")));
    }

    #[test]
    fn only_a_first_ctrl_f3_press_toggles_diagnostics() {
        let press = |pressed, repeat, modifiers| egui::Event::Key {
            key: egui::Key::F3,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        assert!(super::is_diagnostics_shortcut(&press(
            true,
            false,
            egui::Modifiers::CTRL
        )));
        assert!(!super::is_diagnostics_shortcut(&press(
            true,
            false,
            egui::Modifiers::NONE
        )));
        assert!(!super::is_diagnostics_shortcut(&press(
            false,
            false,
            egui::Modifiers::CTRL
        )));
        assert!(!super::is_diagnostics_shortcut(&press(
            true,
            true,
            egui::Modifiers::CTRL
        )));
        assert!(!super::is_diagnostics_shortcut(&press(
            true,
            false,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT
        )));
    }

    #[test]
    fn display_uses_the_save_calendar_at_midnight() {
        assert_eq!(date(60_765_882), "1936-09-29  19:00");
        assert_eq!(date(60_765_887), "1936-09-29  24:00");
        assert_eq!(date(60_765_888), "1936-09-30  01:00");
    }

    #[test]
    fn a_subsecond_reload_resets_anchor_age_and_ignores_delayed_old_anchors() {
        const LAST: i32 = 100_000;
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::new(Mutex::new(None)),
            None,
            None,
        );
        let now = viewer.last_step;
        let head = |position: f64| Playhead {
            branch: 1,
            hour: position.floor() as i32,
            fraction: position - position.floor(),
        };
        let sample = |origin, composed_at| {
            Some(crate::page_clock::ClockSample {
                origin,
                composed_at,
            })
        };
        viewer.anchor_playback(
            head(100.0),
            true,
            1_440.0,
            true,
            sample("page-1", 10_000.0),
            now + Duration::from_millis(4),
            LAST,
        );
        assert_eq!(viewer.playhead, Some(head(100.0)));
        viewer.anchor_playback(
            head(900.0),
            true,
            1_440.0,
            false,
            sample("page-2", 9_500.0),
            now + Duration::from_millis(504),
            LAST,
        );
        let after_reload = viewer.playhead.expect("reloaded anchor placed");
        assert_eq!(
            after_reload,
            head(900.0),
            "subsecond reload must not advance the playing anchor by the old page's offset"
        );
        viewer.anchor_playback(
            head(999.0),
            false,
            24.0,
            true,
            sample("page-1", 10_400.0),
            now + Duration::from_millis(520),
            LAST,
        );
        assert_eq!(
            viewer.playhead,
            Some(after_reload),
            "delayed anchor from retired origin must be ignored"
        );
        assert!(viewer.playing);
        assert_eq!(viewer.speed, 1_440.0);
        viewer.anchor_playback(
            head(200.0),
            true,
            1_440.0,
            false,
            sample("page-3", 9_100.0),
            now + Duration::from_millis(904),
            LAST,
        );
        assert_eq!(
            viewer.playhead,
            Some(head(200.0)),
            "a new page origin resets the playhead instead of pinning to the previous page's ticked head"
        );
    }

    #[test]
    fn place_painting_does_not_keep_prior_camera_placement_when_leaving_the_pad() {
        use viewer_gpu::{
            icon_tracks::{Head, IconAnchor, IconFamily},
            camera::{MapDefines, MapSize, Placement},
        };
        use viewer_native_gui::{anchored::Refused, sprites::{Batch, Quad}};
        let placed = Placement::map([1000.0, 500.0], 110.0).on(MapSize::TEST, MapDefines::test());
        let outside_pad = Placement::map([1020.0, 500.0], 110.0).on(MapSize::TEST, MapDefines::test());
        let zoomed = Placement::map([1000.0, 500.0], 140.0).on(MapSize::TEST, MapDefines::test());
        let make_batch = |x: f32| {
            let mut batch = Batch::default();
            batch.quads.push(Quad {
                texture: 0,
                rect: [x, 200.0, 4.0, 4.0],
                uv: [0.0; 4],
                color: [1.0; 4],
                rotation: None,
                layer: 0,
                shade: 0.0,
                exact: None,
                anchor: Some(IconAnchor {
                    world: [1000.0, 10.0, 500.0],
                    cutoff: f32::INFINITY,
                    family: IconFamily::Columns,
                    moves: None,
                }),
                present: None,
            });
            batch
        };
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::default(),
            None,
            None,
        );
        viewer.overlay_placed = Some(placed);
        viewer.overlay_placed_view = Some(([1600, 900], 1.0f32.to_bits()));
        viewer.overlay = Some(make_batch(100.0));
        assert_eq!(
            viewer
                .place_painting(outside_pad, [1600, 900], 1.0, Head::default())
                .unwrap(),
            super::PaintingPlaced::Refused(Refused::PastThePad)
        );
        assert_eq!(
            viewer
                .place_painting(zoomed, [1600, 900], 1.0, Head::default())
                .unwrap(),
            super::PaintingPlaced::Refused(Refused::NotAMove)
        );
        let mut repainted = false;
        let withheld = viewer
            .finish_painting(
                outside_pad,
                [1600, 900],
                1.0,
                Head::default(),
                0.25,
                false,
                |viewer| {
                    repainted = true;
                    viewer.overlay_placed = Some(outside_pad);
                    viewer.overlay_placed_view = Some(([1600, 900], 1.0f32.to_bits()));
                    viewer.overlay = Some(make_batch(42.0));
                    Ok(true)
                },
            )
            .unwrap();
        assert!(repainted, "leaving the placement pad triggers relayout and repaint");
        assert_eq!(withheld, None);
        assert_eq!(viewer.overlay_placed, Some(outside_pad));
        assert!(!viewer.overlay_unpainted);
        assert_eq!(viewer.overlay_fraction, Some(0.25));
        assert_eq!(viewer.overlay.as_ref().unwrap().quads[0].rect[0], 42.0);
        assert_eq!(
            viewer.saved_overlay_camera_waiting(true, outside_pad, [1600, 900], 1.0),
            None
        );
        assert!(
            viewer
                .saved_overlay_camera_waiting(true, zoomed, [1600, 900], 1.0)
                .is_some(),
            "saved frames require overlay geometry for the requested camera"
        );
        viewer.overlay_placed = Some(placed);
        viewer.overlay = Some(make_batch(100.0));
        let withheld = viewer
            .finish_painting(
                zoomed,
                [1600, 900],
                1.0,
                Head::default(),
                0.5,
                false,
                |_| Ok(false),
            )
            .unwrap();
        assert_eq!(withheld, Some(Refused::NotAMove));
        assert!(viewer.overlay.is_none(), "refused painting is withheld, never kept at prior camera");
        assert!(viewer.overlay_unpainted);
        assert_eq!(viewer.overlay_fraction, None);
        assert_eq!(viewer.overlay_drawn_fraction, None);
        assert!(
            viewer
                .overlay_loading
                .iter()
                .any(|line| line.contains(Refused::NotAMove.why())),
            "{:?}",
            viewer.overlay_loading
        );
        assert!(
            viewer
                .saved_overlay_camera_waiting(true, zoomed, [1600, 900], 1.0)
                .is_some()
        );
    }

    #[test]
    fn failed_reprojection_on_resize_or_scale_withholds_overlay_without_stamping_view() {
        use viewer_gpu::{icon_tracks::Head, camera::{MapDefines, MapSize, Placement}};
        use viewer_native_gui::{anchored::Refused, sprites::{Batch, Quad}};
        let placed = Placement::map([1000.0, 500.0], 110.0).on(MapSize::TEST, MapDefines::test());
        let make_batch = || {
            let mut batch = Batch::default();
            batch.quads.push(Quad {
                texture: 0,
                rect: [100.0, 200.0, 4.0, 4.0],
                uv: [0.0; 4],
                color: [1.0; 4],
                rotation: None,
                layer: 0,
                shade: 0.0,
                exact: None,
                anchor: None,
                present: None,
            });
            batch
        };
        let mut viewer = Viewer::create(
            Config::default(),
            Options::default(),
            None,
            Arc::default(),
            None,
            None,
        );
        let original_view = ([1600, 900], 1.0f32.to_bits());
        for (pixels, ui_scale) in [([1280, 720], 1.0f32), ([1600, 900], 1.5f32)] {
            viewer.overlay_placed = Some(placed);
            viewer.overlay_placed_view = Some(original_view);
            viewer.overlay = Some(make_batch());
            viewer.overlay_unpainted = false;
            viewer.overlay_fraction = Some(0.25);
            viewer.overlay_drawn_fraction = Some(0.25);
            viewer.overlay_loading.clear();
            assert_eq!(
                viewer
                    .place_painting(placed, pixels, ui_scale, Head::default())
                    .unwrap(),
                super::PaintingPlaced::Refused(Refused::NotAMove),
                "resize or scale change at the same camera must not treat prior viewport geometry as placed"
            );
            let withheld = viewer
                .finish_painting(
                    placed,
                    pixels,
                    ui_scale,
                    Head::default(),
                    0.25,
                    false,
                    |_| Ok(false),
                )
                .unwrap();
            assert_eq!(withheld, Some(Refused::NotAMove));
            assert_eq!(
                viewer.overlay_placed_view,
                Some(original_view),
                "failed reprojection must never stamp the requested viewport or scale"
            );
            assert!(viewer.overlay.is_none(), "stale batch is dropped on failed reprojection");
            assert!(viewer.overlay_unpainted);
            assert_eq!(viewer.overlay_fraction, None);
            assert_eq!(viewer.overlay_drawn_fraction, None);
            assert!(
                viewer
                    .saved_overlay_camera_waiting(true, placed, pixels, ui_scale)
                    .is_some(),
                "saved frame must refuse geometry whose viewport or scale failed to reproject"
            );
        }
        viewer.overlay_placed = Some(placed);
        viewer.overlay_placed_view = Some(original_view);
        viewer.overlay = Some(make_batch());
        viewer.overlay_unpainted = false;
        viewer.overlay_fraction = Some(0.0);
        viewer.overlay_drawn_fraction = Some(0.0);
        viewer.overlay_loading.clear();
        viewer.withhold_overlay("injected projection failure");
        assert!(viewer.overlay.is_none(), "paused projection failure clears stale overlay batch");
        assert!(viewer.overlay_unpainted);
        assert_eq!(viewer.overlay_fraction, None);
        assert_eq!(viewer.overlay_drawn_fraction, None);
        assert!(
            viewer
                .overlay_loading
                .iter()
                .any(|line| line.contains("injected projection failure"))
        );
    }

    #[test]
    fn paused_dashboard_widget_holds_ambient_animation_while_full_map_stays_live() {
        let tables = content_reader_script::test_game::executable().build();
        engine_executable::executable_tables::scope(Some(tables), || {
        use content_reader_gui::pdxshader::program::Stage;
        use shader_pipeline::{Buffer, Compiled, ScalarKind, Shader, VertexInput, binding};
        use viewer_gpu::gpu_contract::{DEPTH_FORMAT, SCENE_FORMAT, SeaBed, Targets, read_texture};
        use viewer_terrain::water_shader::{Inputs as WaterInputs, Water};

        let (_guard, device, queue) = viewer_gpu::test_support::gpu::device();
        let source = content_reader_gui::pdxshader::program::read_with(b"water.shader", &|path| {
            Some(if path == b"water.shader" {
                b"Effect water { }".to_vec()
            } else {
                Vec::new()
            })
        })
        .unwrap();
        let time_binding = binding(3, Stage::Pixel);
        let vertex = Shader {
            wgsl: "@vertex fn main(@location(0) pos: vec2<i32>) -> @builtin(position) vec4<f32> {
                return vec4f(f32(pos.x) * 2.0 - 1.0, f32(pos.y) * 2.0 - 1.0, 0.0, 1.0);
            }"
            .into(),
            buffers: Vec::new(),
            clock: Default::default(),
            independent_draws: false,
            textures: Vec::new(),
            samplers: Vec::new(),
            inputs: vec![VertexInput {
                semantic: "POSITION0".into(),
                kind: ScalarKind::Sint,
                components: 2,
                location: 0,
            }],
            sampling_pairs: Vec::new(),
        };
        let pixel = Shader {
            wgsl: format!(
                "@group(0) @binding({time_binding}) var<uniform> clock: vec4<f32>;
                @fragment fn main() -> @location(0) vec4<f32> {{
                    let phase = fract(clock.x * 0.25);
                    return vec4f(phase, 0.25, 0.75, 1.0);
                }}"
            ),
            buffers: vec![Buffer {
                binding: time_binding,
                size: 16,
                reads: std::iter::once(0..4).collect(),
            }],
            clock: Default::default(),
            independent_draws: false,
            textures: Vec::new(),
            samplers: Vec::new(),
            inputs: Vec::new(),
            sampling_pairs: Vec::new(),
        };
        let effect = Arc::new(shader_pipeline::library::Effect::from_compiled(
            Arc::new(source),
            Vec::new(),
            vec![Ok(Arc::new(Compiled { vertex, pixel }))],
        ));
        let water = Water::new(&device, &effect, [1, 1], 1, SCENE_FORMAT, DEPTH_FORMAT, "water").unwrap();

        let mut train_assets = viewer_rail::trains::test_support::assets();
        train_assets.library.index.insert("car".into(), 0);
        let train_mesh_camera = Placement::map([200.0, 0.0], 80.0).on(
            MapSize { width: 1024, height: 512 },
            viewer_gpu::camera::MapDefines::test(),
        );
        let motion_view = viewer_models::motion_view::MotionView::new(&train_mesh_camera, [64, 64]);

        struct Harness<'a> {
            viewer: Viewer,
            ctx: egui::Context,
            targets: Targets,
            arena: shader_pipeline::gpu::Arena,
            plan: viewer_rail::trains::TrainPlan,
            rail_fade: viewer_rail::railways::RailFade,
            unit_history: viewer_unit_history::unit_history::History,
            building_history: viewer_buildings::map_buildings::BuildingHistory,
            standing: Vec<(usize, viewer_models::ModelInstance)>,
            last_transition_time: Option<f32>,
            water: &'a Water,
            train_assets: &'a viewer_rail::trains::TrainAssets,
            motion_view: &'a viewer_models::motion_view::MotionView,
            camera: Camera,
        }

        impl<'a> Harness<'a> {
            fn new(
                device: &wgpu::Device,
                dashboard_map: bool,
                water: &'a Water,
                train_assets: &'a viewer_rail::trains::TrainAssets,
                motion_view: &'a viewer_models::motion_view::MotionView,
                camera: Camera,
            ) -> Self {
                let mut viewer = Viewer::create(
                    Config::default(),
                    Options::default(),
                    None,
                    Arc::default(),
                    None,
                    None,
                );
                viewer.dashboard_map = dashboard_map;
                viewer.playhead = Some(Playhead { branch: 1, hour: 10, fraction: 0.0 });
                let ctx = egui::Context::default();
                for step in 0..2 {
                    let mut warm = ctx.run_ui(
                        egui::RawInput {
                            time: Some(step as f64),
                            ..Default::default()
                        },
                        |_| {},
                    );
                    warm.textures_delta.clear();
                }
                let mut unit_history = viewer_unit_history::unit_history::History::default();
                let bounds = std::collections::BTreeMap::from([(1u32, [199.0, -1.0, 201.0, 1.0])]);
                unit_history.set(
                    vec![viewer_unit_history::unit_history::Target {
                        province: 1,
                        identity: 1,
                        part: 0,
                        order: 0,
                        occupant: 1,
                        kind: viewer_unit_history::unit_history::Kind::Land,
                        mesh: 0,
                        instance: viewer_models::ModelInstance {
                            position: [200.0, 0.0, 0.0],
                            scale: 1.0,
                            ..Default::default()
                        },
                        origin: [200.0, 0.0, 0.0],
                    }],
                    Arc::new(bounds),
                    false,
                );
                Self {
                    viewer,
                    ctx,
                    targets: Targets::new(device, [8, 8], [8, 8], 1, SeaBed::Full),
                    arena: shader_pipeline::gpu::Arena::new(device),
                    plan: viewer_rail::trains::test_support::plan(),
                    rail_fade: viewer_rail::railways::RailFade::default(),
                    unit_history,
                    building_history: Default::default(),
                    standing: Vec::new(),
                    last_transition_time: None,
                    water,
                    train_assets,
                    motion_view,
                    camera,
                }
            }

            fn present(
                &mut self,
                device: &wgpu::Device,
                queue: &wgpu::Queue,
                clock_s: f64,
            ) -> (Duration, Vec<u8>, [f32; 3], f32) {
                let paused_motion = self.viewer.paused_motion();
                let ambient_time = self.viewer.ambient_time_at(clock_s);
                let transition_time = clock_s as f32;
                let transition_delta = self
                    .last_transition_time
                    .map_or(0.0, |last| (transition_time - last).max(0.0));
                self.last_transition_time = Some(transition_time);
                let tracking = paused_motion == PausedMotion::Live;
                self.arena.reset();
                self.arena.track_motion(tracking);
                let rail_alpha = viewer_world::scene::step_rail_fade(
                    &mut self.rail_fade,
                    &self.camera,
                    true,
                    ambient_time,
                    Some(transition_time),
                    &self.arena,
                );
                let gates = viewer_unit_reads::model_admission::Gates {
                    units: self.camera.defines.railway_cutoff,
                    ships: self.camera.defines.railway_cutoff,
                    arrows: self.camera.defines.railway_cutoff,
                    scale_up: 4.0,
                    scale_down: 4.0,
                    ship_scale_down: 4.0,
                    ..viewer_unit_reads::model_admission::Gates::OFF
                };
                self.unit_history
                    .set_transition_step(Some(transition_delta), Some(transition_time));
                let models = self.unit_history.update_models(
                    self.camera,
                    [64, 64],
                    ambient_time,
                    gates,
                    i32::MAX,
                    &mut self.building_history,
                    true,
                );
                if let Some(standing) = models.units.standing {
                    self.standing = standing;
                }
                let field = self.plan.field(self.train_assets, ambient_time);
                assert!(!field.instances.is_empty(), "a recorded supply train is active on the route");
                let train_pos = field.instances[0].position;
                self.arena.check_motion(|| true);
                if tracking {
                    self.arena.request_frame();
                }

                self.targets.surface_motion.begin(device, false, 1, tracking);
                let mut encoder = device.create_command_encoder(&Default::default());
                let texture = &self.targets.refraction_view;
                self.water
                    .draw(
                        &self.arena,
                        &mut encoder,
                        &self.targets,
                        WaterInputs {
                            fog: false,
                            animated_sea_bed: false,
                            retained: None,
                            frames: &[&[0; 416]],
                            time: ambient_time,
                            size: [8, 8],
                            retained_pixel_b2:
                                viewer_scene_base::shader_constants::WorldPixelConstants::LightGrid(
                                    [0.0; 4],
                                ),
                            textures: [texture; 16],
                        },
                    )
                    .unwrap();
                self.targets.surface_motion.finish(&mut encoder);
                self.arena.flush(queue);
                queue.submit([encoder.finish()]);
                self.targets.surface_motion.submitted();
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                self.targets.surface_motion.begin(device, false, 1, tracking);

                let needs_frame = self.arena.needs_frame()
                    || self.unit_history.carrying_figures()
                    || self.unit_history.scaling_figures()
                    || self.building_history.growing()
                    || self.targets.surface_motion.live();
                let mut output = self.ctx.run_ui(
                    egui::RawInput {
                        time: Some(clock_s),
                        ..Default::default()
                    },
                    |ui| {
                        if self.viewer.playing {
                            ui.ctx().request_repaint();
                        }
                        super::schedule_after_render(
                            ui.ctx(),
                            Duration::ZERO,
                            needs_frame,
                            paused_motion,
                            || self.plan.next_arrival(self.train_assets, self.motion_view, ambient_time),
                        );
                    },
                );
                output.textures_delta.clear();
                let repaint_delay = output
                    .viewport_output
                    .get(&egui::ViewportId::ROOT)
                    .expect("root viewport output")
                    .repaint_delay;
                let water_pixels =
                    read_texture(device, queue, self.targets.scene_texture(), 8).unwrap();
                (repaint_delay, water_pixels, train_pos, rail_alpha)
            }
        }

        let diagnostics_mentions_held = |viewer: &mut Viewer| -> bool {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    viewer.diagnostics_ui(ui, 200.0);
                });
            });
            output.textures_delta.clear();
            output.shapes.iter().any(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => text
                    .galley
                    .text()
                    .contains("Ambient animation: held while paused"),
                _ => false,
            })
        };

        let far_camera = Placement::map(
            [200.0, 0.0],
            train_mesh_camera.defines.railway_cutoff + 20.0,
        )
        .on(
            MapSize { width: 1024, height: 512 },
            viewer_gpu::camera::MapDefines::test(),
        );
        let mut widget = Harness::new(&device, true, &water, &train_assets, &motion_view, far_camera);
        let mut full_map = Harness::new(&device, false, &water, &train_assets, &motion_view, train_mesh_camera);
        assert!(diagnostics_mentions_held(&mut widget.viewer));
        assert!(!diagnostics_mentions_held(&mut full_map.viewer));

        let (initial_delay, _, _, initial_alpha) = widget.present(&device, &queue, 45.0);
        assert_eq!(initial_alpha, 0.0);
        assert_eq!(initial_delay, Duration::MAX);

        widget.camera = train_mesh_camera;
        let (fade_start_delay, _, _, fade_start_alpha) = widget.present(&device, &queue, 45.1);
        assert!(fade_start_alpha > 0.0 && fade_start_alpha < 1.0, "{fade_start_alpha}");
        assert!(widget.arena.needs_frame(), "step_rail_fade itself requests a frame while rail fade is progressing");
        assert!(
            fade_start_delay < Duration::MAX,
            "a live rail fade still requests frames while paused"
        );
        let (paused_delay, paused_water, paused_train, settled_alpha) =
            widget.present(&device, &queue, 46.0);
        assert_eq!(settled_alpha, 1.0);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);
        assert_eq!(
            paused_delay,
            Duration::MAX,
            "a settled paused dashboard widget requests neither immediate nor delayed ambient repaints"
        );
        assert!(!widget.arena.needs_frame());
        assert!(!widget.unit_history.scaling_figures());
        assert!(!widget.targets.surface_motion.live());

        let (input_delay, hover_water, hover_train, _) = widget.present(&device, &queue, 47.0);
        assert_eq!(input_delay, Duration::MAX, "an input redraw at the same hour stays quiet after settling");
        assert_eq!(hover_water, paused_water, "redrawing for input preserves the frozen water phase");
        assert_eq!(hover_train, paused_train, "redrawing for input preserves the frozen train position");

        let seek_at = widget.viewer.started;
        widget.viewer.anchor_playback(
            Playhead { branch: 1, hour: 15, fraction: 0.0 },
            false,
            24.0,
            true,
            None,
            seek_at,
            100,
        );
        assert_eq!(widget.viewer.playhead.map(|head| head.hour), Some(15));
        let (seek_delay, seek_water, seek_train, _) = widget.present(&device, &queue, 48.0);
        assert_eq!(seek_delay, Duration::MAX);
        assert_eq!(seek_water, paused_water, "seeking while paused preserves the frozen water phase");
        assert_eq!(seek_train, paused_train, "seeking while paused preserves the frozen train position");

        let untimed_capture = || Capture {
            path: PathBuf::new(),
            remaining: 0,
            pinned: None,
            exit: false,
            time: None,
            step: 0.0,
            frames: 1,
            index: 0,
            paced: false,
            export: None,
            video: None,
        };
        widget.viewer.capture = Some(untimed_capture());
        let (_, export_water_1, export_train_1, _) = widget.present(&device, &queue, 60.0);
        assert_eq!(export_water_1, paused_water);
        assert_eq!(export_train_1, paused_train);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);
        widget.viewer.capture = None;
        let (after_capture_delay_1, after_capture_water_1, after_capture_train_1, _) =
            widget.present(&device, &queue, 61.0);
        assert_eq!(after_capture_delay_1, Duration::MAX);
        assert_eq!(after_capture_water_1, paused_water);
        assert_eq!(after_capture_train_1, paused_train);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);
        assert!(!widget.unit_history.scaling_figures());

        widget.viewer.capture = Some(untimed_capture());
        let (_, export_water_2, export_train_2, _) = widget.present(&device, &queue, 75.0);
        assert_eq!(export_water_2, export_water_1);
        assert_eq!(export_train_2, export_train_1);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);
        widget.viewer.capture = None;
        let (after_capture_delay_2, after_capture_water_2, after_capture_train_2, _) =
            widget.present(&device, &queue, 76.0);
        assert_eq!(after_capture_delay_2, Duration::MAX);
        assert_eq!(after_capture_water_2, paused_water);
        assert_eq!(after_capture_train_2, paused_train);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);
        assert!(!widget.unit_history.scaling_figures());

        widget.viewer.playing = true;
        assert!(!diagnostics_mentions_held(&mut widget.viewer));
        let (play_start_delay, _, _, _) = widget.present(&device, &queue, 77.0);
        assert!(play_start_delay < Duration::MAX, "playing wakes the widget immediately");
        let (playing_delay, playing_water, playing_train, _) = widget.present(&device, &queue, 78.0);
        assert!(playing_delay < Duration::MAX, "playing keeps advancing frames");
        assert_ne!(playing_water, paused_water, "playing advances the water phase");
        assert_ne!(playing_train, paused_train, "playing advances the train position");

        widget.viewer.capture = Some(untimed_capture());
        let (_, play_export_water, play_export_train, _) = widget.present(&device, &queue, 79.0);
        assert_ne!(play_export_water, playing_water, "untimed export during playback keeps advancing water");
        assert_ne!(play_export_train, playing_train, "untimed export during playback keeps advancing trains");
        widget.viewer.capture = None;
        let (_, after_play_export_water, after_play_export_train, _) =
            widget.present(&device, &queue, 80.0);
        assert_ne!(after_play_export_water, play_export_water);
        assert_ne!(after_play_export_train, play_export_train);
        assert_eq!(widget.standing.len(), 1);
        assert_eq!(widget.standing[0].1.scale, 1.0);

        let (full_delay_1, full_water_1, full_train_1, _) = full_map.present(&device, &queue, 45.0);
        assert!(full_delay_1 < Duration::MAX, "the same paused scene on the full map keeps requesting frames");
        assert!(full_map.targets.surface_motion.live());
        let (full_delay_2, full_water_2, full_train_2, _) = full_map.present(&device, &queue, 46.0);
        assert!(full_delay_2 < Duration::MAX);
        assert_ne!(full_water_2, full_water_1, "paused full map keeps animating water");
        assert_ne!(full_train_2, full_train_1, "paused full map keeps moving trains");
        });
    }
}
