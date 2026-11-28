
use viewer_layer_core::rebuilt::{Build, Seen, Whole};
use viewer_layer_core::scene_content::SceneContent;
use replay_data::rows::{TableId, TaskForceRow};
use replay_frame::frame::DecodedFrame;
use std::{
    io,
    sync::{Arc, Mutex},
};
use viewer_unit_reads::{counter_move::RouteWorld, task_force_visual::TaskForceMotions};
use viewer_runtime::presentation::Continuity;
use viewer_gpu::province_map::HeightField;
use viewer_aircraft::aircraft::{AircraftPlan, Kept, KeptModels, ModelsMade};
use viewer_unit_models::equipment_graphics::{Decided, LimitValues, Limits};

pub const TABLES: &[TableId] = &[
    TableId::AirWings,
    TableId::WingEquipment,
    TableId::AirRegionalMissions,
    TableId::AirNavalPatrolCombats,
    TableId::AirNavalPatrolHistories,
    TableId::NavalRegionalMines,
    TableId::NavalBases,
    TableId::Ships,
    TableId::TaskForces,
    TableId::TaskForceMapMotion,
    TableId::ProvinceUnitOrder,
    TableId::Provinces,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::WarRelations,
    TableId::AirCountryRelations,
    TableId::VolunteerHosts,
    TableId::SimulationFeatures,
    TableId::VariantDefinitions,
    TableId::LocalPlayer,
    TableId::AirReferenceWitnesses,
    TableId::AirBases,
    TableId::AirMissionInputs,
    TableId::AirSortieBaseCountries,
    TableId::AirSortieModifiers,
    TableId::AirSortieSpecialBases,
    TableId::AirSortieModifierMetadata,
    TableId::States,
    TableId::StateBuildings,
    TableId::ProvinceBuildings,
    TableId::Divisions,
    TableId::DivisionMovement,
    TableId::LandCombats,
    TableId::LandCombatSides,
    TableId::NavalCombats,
    TableId::NavalCombatDetails,
    TableId::NavalCombatShips,
    TableId::Politics,
    TableId::PoliticalParties,
    TableId::Factions,
    TableId::FactionMembers,
    TableId::Subjects,
    TableId::RelationLinks,
    TableId::CountryAppearance,
    TableId::LexicalTokens,
    TableId::ScriptVariables,
    TableId::ScriptVariableNames,
    TableId::Characters,
    TableId::CountryCounterInputs,
    TableId::GlobalSavedEventTargets,
];

pub type Aircraft = Whole<Flights>;



pub fn notes(aircraft: &Aircraft, statuses: &dyn viewer_foundation::tables::Statuses) -> Vec<String> {
    if !aircraft.drawn() {
        return Vec::new();
    }
    let mut out = viewer_foundation::tables::Limitations::default();
    out.note_aircraft(statuses);
    out.into_lines()
}

pub fn warning(aircraft: &Aircraft) -> Option<String> {
    aircraft.refusal().filter(|_| aircraft.shows()).map(|why| format!("aircraft: {why}"))
}

pub struct Flown {
    pub plan: AircraftPlan,
    decided: Decided,
    models: ModelsMade,
}

impl std::ops::Deref for Flown {
    type Target = AircraftPlan;

    fn deref(&self) -> &AircraftPlan {
        &self.plan
    }
}

pub trait Source: Send + Sync {
    fn scene(&self) -> &SceneContent;
    fn tables(&self) -> Option<engine_executable::executable_tables::Tables>;
    fn hour<'a>(&'a self, frame: &'a DecodedFrame) -> map_products::ProductHour<'a>;
}

pub struct Flights {
    content: Arc<dyn Source>,
    kept: Mutex<(Kept, KeptModels)>,
}

impl Flights {
    fn make(&self, frame: &DecodedFrame, choice: &viewer_foundation::perspective::Choice, (kept, models): &mut (Kept, KeptModels)) -> io::Result<Flown> {
        let content = self.content.scene();
        let hour = self.content.hour(frame);
        let ours = super::intel_icons::ours(frame, choice)?;
        let heights = HeightField { width: content.assets.heightmap.width, height: content.assets.heightmap.height, values: content.assets.heightmap_texels() };
        let movement = content.movement.as_deref();
        let world = movement.map(|movement| RouteWorld { map: &movement.map, heights: content.heights(), width: content.map.width as f32 });
        let motions = TaskForceMotions::read(frame)?;
        let land = |province| {
            movement.and_then(|movement| movement.map.terrain(province)).map_or_else(
                || content.map.definitions.get(&province).is_some_and(|definition| definition.kind == content_reader_map::map::ProvinceKind::Land),
                |terrain| terrain.flags & 1 != 0,
            )
        };
        let origin = |force: &TaskForceRow| motions.settled_origin(force, movement, world.as_ref(), &|province, slot| content.counters.anchor(province, slot), &land);
        let evaluate = |items: &[content_reader_parser::parser::Item], scopes| hour.trigger_world().evaluate(items, scopes);
        let values = LimitValues::default();
        let (plan, decided, made) = models.session(&content.unit_models, frame, Limits::new(&values, &evaluate), |model| {
            AircraftPlan::make(frame, &content.aircraft, &content.counters.region_of, &content.province_geometry, &heights, ours, model, &origin, &|province| content.products.impact_place(province), kept)
        });
        Ok(Flown { plan: plan?, decided, models: made })
    }
}

impl Build for Flights {
    type Product = Flown;

    fn exact_columns(&self) -> &[TableId] {
        &[TableId::VariantDefinitions]
    }

    fn hourly(&self) -> bool {
        true
    }

    fn tables(&self) -> Option<engine_executable::executable_tables::Tables> {
        self.content.tables()
    }

    fn build(&self, frame: &DecodedFrame, (choice, _): &Seen) -> io::Result<Flown> {
        let mut kept = self.kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.make(frame, choice, &mut kept)
    }

    fn anew(&self, frame: &DecodedFrame, (choice, _): &Seen) -> io::Result<Flown> {
        self.make(frame, choice, &mut Default::default())
    }

    fn stands(&self, flown: &Flown, frame: &DecodedFrame, _: &Seen) -> bool {
        if flown.counts()[2] > 0 {
            return false;
        }
        let hour = self.content.hour(frame);
        let evaluate = |items: &[content_reader_parser::parser::Item], scopes| hour.trigger_world().evaluate(items, scopes);
        let values = LimitValues::default();
        Limits::new(&values, &evaluate).decide_the_same(&flown.decided)
    }

    fn parts(&self, flown: &Flown, each: &mut dyn FnMut(&'static str, u64)) {
        for (name, ns) in flown.marks.parts() {
            each(name, ns);
        }
    }

    fn counts(&self, flown: &Flown, each: &mut dyn FnMut(&'static str, u64)) {
        let marks = flown.marks;
        for (name, count) in [
            ("wings admitted", marks.wings),
            ("scenarios placed", marks.scenarios),
            ("scenarios kept", marks.kept),
            ("flights placed", marks.flights_placed),
            ("flights kept", marks.flights_kept),
            ("models kept", flown.models.kept),
            ("models chosen", flown.models.chosen),
        ] {
            each(name, count as u64);
        }
    }

    fn install(&self, gpu: &mut viewer_layer_core::layers::Gpu<'_>, flown: Option<&Flown>, continuity: Continuity) -> io::Result<Option<Flown>> {
        let plan = flown.map_or_else(AircraftPlan::default, |flown| flown.plan.clone());
        let replaced = gpu.scene.set_aircraft(Arc::clone(&gpu.content.aircraft), plan, continuity)?;
        gpu.scene.warm_added_aircraft(gpu.device)?;
        Ok(replaced.map(|plan| Flown { plan, decided: Decided::new(), models: ModelsMade::default() }))
    }
}

fn drawn(flights: &Flights, view: &viewer_layer_core::layers::View) -> bool {
    view.camera.is_none_or(|camera| camera.height < flights.content.scene().map_defines.map_objects_cutoff)
}

pub fn new(content: Arc<dyn Source>, timed: bool) -> io::Result<Aircraft> {
    Whole::new("aircraft", Flights { content, kept: Mutex::default() }, drawn, |flown| flown.plan.bytes(), TABLES, timed)
}

pub fn summary(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> serde_json::Value {
    let Some(layer) = side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<Aircraft>())) else { return serde_json::Value::Null };
    let builds = layer.rebuilt().builds();
    let count = builds.count.max(1) as f64;
    let seconds = builds.span.map(|(first, last)| last.duration_since(first).as_secs_f64()).filter(|&seconds| seconds > 0.0);
    let parts_ns: u64 = builds.parts.iter().map(|(_, ns)| ns).sum();
    let mut parts = serde_json::Map::new();
    for (name, ns) in builds.parts.iter().copied().chain([("rest", builds.total_ns.saturating_sub(parts_ns))]) {
        parts.insert(name.into(), serde_json::json!({"ms_mean": ns as f64 / 1e6 / count, "share": ns as f64 / builds.total_ns.max(1) as f64}));
    }
    let counts: serde_json::Map<String, serde_json::Value> = builds.counts.iter().map(|&(name, count)| (name.to_owned(), serde_json::json!({"total": count, "a_build": count as f64 / count_of(&builds)}))).collect();
    let behind: serde_json::Map<String, serde_json::Value> = viewer_layer_core::rebuilt::BEHIND.iter().zip(builds.behind).map(|(bound, n)| (if *bound == u64::MAX { "more".to_owned() } else { format!("up to {bound}") }, n.into())).collect();
    let shown = layer.product().and_then(|built| built.product.as_ref().ok().map(|flown| (flown.counts(), flown.marks, built.hour, built.current())));
    serde_json::json!({
        "builds": builds.count,
        "builds_a_second": seconds.map(|seconds| builds.count.saturating_sub(1) as f64 / seconds),
        "build_ms": {"mean": builds.total_ns as f64 / 1e6 / count, "most": builds.most_ns as f64 / 1e6},
        "cores": seconds.map(|seconds| (builds.total_ns + builds.confirm_ns) as f64 / 1e9 / seconds),
        "stands": {"count": builds.confirmed, "ms_mean": builds.confirm_ns as f64 / 1e6 / builds.confirmed.max(1) as f64},
        "parts": parts,
        "counts": counts,
        "rows_behind_head_hours": {"buckets": behind, "mean": builds.behind_sum as f64 / count, "most": builds.behind_most},
        "passed_over": builds.skipped, "discovered": builds.discovered, "tables": layer.rebuilt().tables().len(),
        "shown": shown.map(|([flights, launches, diagnostics], marks, made, current)| serde_json::json!({"flights": flights, "launches": launches, "diagnostics": diagnostics, "wings_admitted": marks.wings, "scenarios_placed": marks.scenarios, "scenarios_kept": marks.kept, "made_at": made, "current_at": current})),
        "scope": "The aircraft worker over the run: its builds, made from the shared rows at the head when a table it reads changed, at the step, and the rows it found the shown plan stands at without a build (stands: the pools its scripts decided decide the same); cores counts both against the span from the first build to the last. Per part of a build the mean milliseconds and its share of build time (models: each wing's model, kept or chosen by the content's scripts; placement: routes and terrain of the scenarios placed anew; rest: the layer's reads before the maker); the wings, scenarios and models made anew and kept from the build before; the game hours the rows of each build were behind the map's head when it began; frames of rows published to the worker it never took.",
    })
}

fn count_of(builds: &viewer_layer_core::rebuilt::Builds) -> f64 {
    builds.count.max(1) as f64
}
