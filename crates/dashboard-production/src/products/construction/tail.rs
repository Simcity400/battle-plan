use super::{
    COST_SCALE, Content, ProjectKind, SpendingGap,
    log::{
        ConstructionOutputRun, ConstructionProject, EndKind, LevelsBuilt, ProjectEnd, ProjectSource, UnknownEnd,
        join_project, window_projects,
    },
    missing,
};
use dashboard_base::{
    Source,
    catalogue::{self, Catalogue},
    products::{
        LoadContext, Product,
        at_hour::product_panel,
        occurrences::{self, OccurrenceCount, OccurrenceReply, OccurrenceSort},
        read_ahead::Preparation,
    },
};
use dashboard_follow::panel::{ExactReader, Panel, ProductReader};
use dashboard_query::{
    Query,
    identity::{Country, Holders},
    log::occurrence::{Ask, Direction, Index, Occurrence, Order, PlacedGap, Role, Selection, SortKey, SortValue},
    series::ranked::GroupSelection,
    timeline::Timeline,
};
use dashboard_world::products::place_name_runs::{self, PlacePoint, RowNames};
use replay::campaign::Replay;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    sync::{Arc, Mutex, OnceLock, PoisonError, atomic::AtomicBool},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ConstructionStatus {
    Building,
    Repairing,
    Waiting,
    Observed,
    Unread,
    Built,
    Repaired,
    RepairedAndBuilt,
    Completed,
    Removed,
    LeftQueue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstructionWorkKind {
    New,
    Repair,
    RepairThenNew,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ConstructionWorkFilter {
    All,
    New,
    Repair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ConstructionOrder {
    Change,
    Start,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionOwner {
    pub tag: Option<String>,
    pub slot: Option<u32>,
    pub missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionPlaceAt {
    pub kind: ProjectKind,
    pub state: Option<u32>,
    pub to_state: Option<u32>,
    pub province: Option<u32>,
    pub at_hour: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub name_missing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub to_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub to_name_missing: Option<String>,
}

impl ConstructionPlaceAt {
    fn points(&self) -> [Option<PlacePoint>; 2] {
        place_points(self.kind, (self.state, self.to_state, self.province), self.at_hour)
    }
}

fn place_points(kind: ProjectKind, (state, to_state, province): (Option<u32>, Option<u32>, Option<u32>), hour: i32) -> [Option<PlacePoint>; 2] {
    let at_state = |id| PlacePoint::State(id, hour);
    let first = state.map(at_state);
    if kind == ProjectKind::Railway {
        [first, to_state.filter(|&id| Some(id) != state).map(at_state)]
    } else {
        [first.or_else(|| province.map(|id| PlacePoint::Province(id, hour))), None]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionLevelsAt {
    pub built: u32,
    pub total: u32,
    pub unknown: u32,
    pub possible: u32,
    pub added: u32,
    pub earlier_unknown: bool,
    pub counters_unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionEndAt {
    pub kind: EndKind,
    pub hour: Option<i32>,
    pub known_at: i32,
    pub unknown: Option<UnknownEnd>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionOutputAt {
    pub construction_raw: Option<String>,
    pub repair_raw: Option<String>,
    pub reason: Option<SpendingGap>,
    pub before_observation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionTypeChip {
    pub token: String,
    pub first_hour: i32,
    pub name: Option<String>,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionLogRow {
    pub key: String,
    pub hour: i32,
    pub exact: bool,
    pub observed: i32,
    pub live: bool,
    pub kind: ProjectKind,
    pub source: ProjectSource,
    pub status: ConstructionStatus,
    pub owner: ConstructionOwner,
    pub building_token: Option<String>,
    pub building_name: Option<String>,
    pub building_missing: Option<String>,
    pub icon: Option<String>,
    pub place: Option<ConstructionPlaceAt>,
    pub place_missing: Option<String>,
    pub levels: Option<ConstructionLevelsAt>,
    pub levels_missing: Option<String>,
    pub factories: Option<i32>,
    pub last_factories: Option<i32>,
    pub factories_missing: Option<String>,
    pub created_hour: Option<i32>,
    pub first_read: i32,
    pub cost_raw: Option<String>,
    pub base_level: Option<i32>,
    pub target_level: Option<i32>,
    pub route_provinces: Option<u32>,
    pub end: Option<ConstructionEndAt>,
    pub output: Option<ConstructionOutputAt>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConstructionLogTailParams {
    pub branch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub hour: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub to: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub countries: Option<Vec<Country>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub sort: Option<OccurrenceSort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub offset: Option<String>,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub building_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub projects: Option<ConstructionWorkFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub in_queue: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub order: Option<ConstructionOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub place_names: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionLogTailReply {
    pub branch_id: String,
    pub total: String,
    pub offset: String,
    pub live: String,
    pub counts: Vec<OccurrenceCount>,
    pub building_types: Vec<ConstructionTypeChip>,
    pub repair_icon: Option<String>,
    #[doc = " The most civilian factories one construction line takes, from the recorded defines; null with `factory_cap_missing` when they cannot be read."]
    pub factory_cap: Option<i32>,
    pub factory_cap_missing: Option<String>,
    pub cost_scale: u32,
    pub rows: Vec<ConstructionLogRow>,
    pub missing: Vec<PlacedGap>,
    pub next_change: Option<i32>,
    pub unavailable: Option<String>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(ConstructionLogTailReply);

impl OccurrenceReply for ConstructionLogTailReply {
    fn next_change(&self) -> Option<i32> {
        self.next_change
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }
}

const LIVE_BIT: u8 = 1;
const NEW_BIT: u8 = 2;
const REPAIR_BIT: u8 = 4;
const TYPED_BIT: u8 = 8;
const ALL_STATES: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const LIVE_STATES: [u8; 8] = [1, 3, 5, 7, 9, 11, 13, 15];

struct ProjectState {
    source: ProjectSource,
    building: Option<u32>,
    state: Option<u32>,
    to_state: Option<u32>,
    province: Option<u32>,
    owner_slot: Option<u32>,
    created_hour: Option<i32>,
    cost_raw: Option<i64>,
    base_level: Option<i32>,
    target_level: Option<i32>,
    route_provinces: Option<u32>,
    earlier_unknown: bool,
    counters_unread: bool,
    status: ConstructionStatus,
    live: bool,
    work_kind: ConstructionWorkKind,
    built: u32,
    unknown: u32,
    possible: u32,
    total: u32,
    added: u32,
    has_count_evidence: bool,
    factories: Option<i32>,
    last_factories: Option<i32>,
    start_hour: i32,
    start_exact: bool,
    changed_hour: i32,
    changed_exact: bool,
    end: Option<ProjectEnd>,
    output: Option<ConstructionOutputRun>,
}

fn in_spans(spans: Option<&[(i32, i32)]>, hour: i32) -> bool {
    let Some(spans) = spans else { return false };
    let at = spans.partition_point(|&(_, last)| last < hour);
    spans.get(at).is_some_and(|&(first, _)| first <= hour)
}

fn sum_levels(list: &[LevelsBuilt], until: i32) -> u32 {
    list.iter().filter(|levels| levels.known_at <= until).fold(0u32, |acc, levels| acc.saturating_add(levels.count))
}

fn level_events(project: &ConstructionProject) -> impl Iterator<Item = &LevelsBuilt> {
    project.built.iter().chain(&project.unaccounted).chain(&project.possible)
}

fn place_hints(project: &ConstructionProject, order_start: bool) -> impl Iterator<Item = PlacePoint> + '_ {
    let origin = project.output_origin.as_ref();
    let start = project.created_hour.unwrap_or(project.first_read);
    let dates = [Some(start), origin.map(|_| project.first_read)].into_iter().flatten();
    let changes = (!order_start).then(|| {
        level_events(project).filter(|levels| !levels.carried).map(|levels| levels.hour.unwrap_or(levels.known_at))
            .chain(project.end.as_ref().map(|end| end.hour.unwrap_or(end.known_at)))
    });
    let locations = std::iter::once((project.state, project.to_state, project.province))
        .chain(origin.map(|origin| (origin.state, origin.to_state, origin.province)));
    dates.chain(changes.into_iter().flatten()).flat_map(move |hour| {
        locations.clone().flat_map(move |location| place_points(project.kind, location, hour).into_iter().flatten())
    })
}

fn project_state_at(project: &ConstructionProject, hour: i32) -> Option<ProjectState> {
    if hour < project.first_read {
        return None;
    }
    let before_census = project.output_origin.is_some() && project.census_read.is_none_or(|census| hour < census);
    let (source, building, state, to_state, province, owner_slot, repair, cost_raw, created_hour, route_provinces, base_level, target_level, earlier_unknown) =
        if let (true, Some(origin)) = (before_census, project.output_origin.as_ref()) {
            (ProjectSource::OutputAlone, origin.building, origin.state, origin.to_state, origin.province, origin.country_slot, None, None, None, None, None, None, true)
        } else {
            (
                project.source,
                project.building,
                project.state,
                project.to_state,
                project.province,
                project.country_slot,
                project.repair,
                project.cost_raw,
                project.created_hour,
                project.route_provinces,
                project.base_level,
                project.target_level,
                project.earlier_unknown,
            )
        };
    let runs = &project.runs[..project.runs.partition_point(|run| run.first_hour <= hour)];
    let last = runs.last();
    let end = project.end.as_ref().filter(|end| end.known_at <= hour).cloned();
    let run = if end.is_none() { last.filter(|run| run.last_hour >= hour) } else { None };
    let built = sum_levels(&project.built, hour);
    let unknown = sum_levels(&project.unaccounted, hour);
    let possible = sum_levels(&project.possible, hour);
    let added = sum_levels(&project.added, hour);
    let read_hour = hour.min(last.map_or(project.first_read, |r| r.last_hour));
    let before = sum_levels(&project.built, read_hour).saturating_add(sum_levels(&project.unaccounted, read_hour));
    let added_later = added.saturating_sub(sum_levels(&project.added, read_hour));
    let rem = last.map_or(0, |r| r.remaining.max(0) as u32);
    let rep = last.map_or(0, |r| r.repair_levels.max(0) as u32);
    let total = before.saturating_add(rem).saturating_add(rep).saturating_add(added_later);

    let known_iter = || level_events(project).filter(|levels| levels.known_at <= hour);
    let repaired = runs.iter().any(|r| r.repair_levels > 0)
        || known_iter().any(|levels| levels.repair)
        || project.first_repair_tick.is_some_and(|tick| tick <= hour);
    let built_new = runs.iter().any(|r| r.remaining > 0) || known_iter().any(|levels| !levels.repair);
    let work_kind = if matches!(source, ProjectSource::OutputAlone | ProjectSource::Unknown) {
        ConstructionWorkKind::Unknown
    } else if repaired && built_new {
        ConstructionWorkKind::RepairThenNew
    } else if repaired {
        ConstructionWorkKind::Repair
    } else if built_new || repair == Some(false) {
        ConstructionWorkKind::New
    } else {
        ConstructionWorkKind::Unknown
    };

    let built_unread = in_spans(project.built_unread.as_deref(), hour);
    let repaired_unread = in_spans(project.repaired_unread.as_deref(), hour);
    let counters_unread = match work_kind {
        ConstructionWorkKind::Repair => repaired_unread,
        ConstructionWorkKind::New => built_unread,
        ConstructionWorkKind::RepairThenNew | ConstructionWorkKind::Unknown => built_unread || repaired_unread,
    };

    let has_count_evidence = source == ProjectSource::Census
        || (source == ProjectSource::Unknown
            && (project.census_read.is_some_and(|c| c <= hour) || last.is_some() || built > 0 || unknown > 0 || possible > 0));
    let witnessed_new = project.built.iter().any(|levels| levels.known_at <= hour && !levels.repair && levels.count > 0);
    let witnessed_repair = project.built.iter().any(|levels| levels.known_at <= hour && levels.repair && levels.count > 0);

    let status = match &end {
        None => match run {
            Some(active) => {
                if active.factories <= 0 {
                    ConstructionStatus::Waiting
                } else if active.repair_levels > 0 {
                    ConstructionStatus::Repairing
                } else {
                    ConstructionStatus::Building
                }
            }
            None if source == ProjectSource::OutputAlone => ConstructionStatus::Observed,
            None => ConstructionStatus::Unread,
        },
        Some(done) => match done.kind {
            EndKind::Finished if !has_count_evidence => ConstructionStatus::Completed,
            EndKind::Finished => match work_kind {
                ConstructionWorkKind::Repair if witnessed_repair => ConstructionStatus::Repaired,
                ConstructionWorkKind::New if witnessed_new => ConstructionStatus::Built,
                ConstructionWorkKind::RepairThenNew if witnessed_repair && witnessed_new => ConstructionStatus::RepairedAndBuilt,
                _ => ConstructionStatus::Completed,
            },
            EndKind::Removed => ConstructionStatus::Removed,
            EndKind::Unknown => ConstructionStatus::LeftQueue,
        },
    };
    let live = end.is_none();

    let dated = |levels: &LevelsBuilt| levels.hour.unwrap_or(levels.known_at);
    let latest_event = known_iter()
        .filter(|levels| !levels.carried)
        .max_by_key(|levels| (levels.known_at, dated(levels)));
    let start_hour = created_hour.unwrap_or(project.first_read);
    let start_exact = created_hour.is_some();
    let (changed_hour, changed_exact) = if let Some(done) = &end {
        (done.hour.unwrap_or(done.known_at), done.hour.is_some())
    } else if let Some(latest) = latest_event {
        (latest.hour.unwrap_or(latest.known_at), latest.hour.is_some())
    } else {
        (start_hour, start_exact)
    };
    let factories = run.map(|r| r.factories);
    let last_factories = last.map(|r| r.factories);
    let output = project.outputs.iter().rev().find(|out| out.from <= hour).cloned();

    Some(ProjectState {
        source,
        building,
        state,
        to_state,
        province,
        owner_slot,
        created_hour,
        cost_raw,
        base_level,
        target_level,
        route_provinces,
        earlier_unknown,
        counters_unread,
        status,
        live,
        work_kind,
        built,
        unknown,
        possible,
        total,
        added,
        has_count_evidence,
        factories,
        last_factories,
        start_hour,
        start_exact,
        changed_hour,
        changed_exact,
        end,
        output,
    })
}

fn state_code(state: &ProjectState, has_token: bool) -> u8 {
    let mut bits = 0u8;
    if state.live {
        bits |= LIVE_BIT;
    }
    match state.work_kind {
        ConstructionWorkKind::New => bits |= NEW_BIT,
        ConstructionWorkKind::Repair => bits |= REPAIR_BIT,
        ConstructionWorkKind::RepairThenNew => bits |= NEW_BIT | REPAIR_BIT,
        ConstructionWorkKind::Unknown => {}
    }
    if has_token {
        bits |= TYPED_BIT;
    }
    bits
}

fn changes_of(project: &ConstructionProject, last: i32) -> Vec<i32> {
    let mut hours: Vec<i32> = [
        Some(project.first_read),
        project.census_read,
        project.first_repair_tick,
        project.end.as_ref().map(|end| end.known_at),
    ]
    .into_iter()
    .flatten()
    .chain(project.runs.iter().flat_map(|run| [run.first_hour, run.last_hour.saturating_add(1)]))
    .chain(project.built.iter().chain(&project.unaccounted).chain(&project.possible).chain(&project.added).map(|levels| levels.known_at))
    .chain(project.built_unread.iter().flatten().flat_map(|&(first, end)| [first, end.saturating_add(1)]))
    .chain(project.repaired_unread.iter().flatten().flat_map(|&(first, end)| [first, end.saturating_add(1)]))
    .chain(project.outputs.iter().map(|out| out.from))
    .filter(|&hour| hour >= project.first_read && hour <= last)
    .collect();
    hours.push(project.first_read);
    hours.sort_unstable();
    hours.dedup();
    hours
}

fn state_hours(project: &ConstructionProject, last: i32) -> Vec<i32> {
    let mut hours: Vec<i32> = [Some(project.first_read), project.census_read, project.first_repair_tick, project.end.as_ref().map(|end| end.known_at)]
        .into_iter()
        .flatten()
        .chain(project.runs.iter().map(|run| run.first_hour))
        .chain(project.built.iter().chain(&project.unaccounted).chain(&project.possible).map(|levels| levels.known_at))
        .filter(|&hour| hour >= project.first_read && hour <= last)
        .collect();
    hours.sort_unstable();
    hours.dedup();
    hours
}

struct IndexedProject {
    project: ConstructionProject,
    changes: Box<[i32]>,
}

struct TypeInfo {
    token: Option<String>,
    name: Option<String>,
    icon: Option<String>,
}

struct Tail {
    index: Index<IndexedProject>,
    spellings: Vec<String>,
    chips: Vec<ConstructionTypeChip>,
    types: HashMap<u32, TypeInfo>,
    repair_icon: Option<String>,
    factory_cap: Result<i32, String>,
    unavailable: Option<String>,
    warnings: Vec<String>,
}

#[derive(Clone)]
enum Made {
    Catalogue(Arc<Catalogue>),
    Refused(String),
}

impl Made {
    fn of(content: &Content) -> Self {
        match content.catalogue() {
            Ok(catalogue) => Self::Catalogue(Arc::clone(catalogue)),
            Err(reason) => Self::Refused(reason.to_owned()),
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Catalogue(a), Self::Catalogue(b)) => Arc::ptr_eq(a, b),
            (Self::Refused(a), Self::Refused(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Default)]
struct Kept(HashMap<u64, (Made, Arc<Tail>)>);

pub fn forget(source: &Source) {
    *source.state::<Kept>().lock().unwrap_or_else(PoisonError::into_inner) = Kept::default();
}

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

fn scan(source: &Source, query: &Query<'_>, content: &Content, cancelled: &AtomicBool) -> io::Result<Tail> {
    let (first, last) = (query.timeline.first, query.timeline.last);
    let recorded = super::recorded(query)?;
    let mut tokens: BTreeMap<u32, Option<String>> = BTreeMap::new();
    let mut by_id: HashMap<String, ConstructionProject> = HashMap::new();
    let mut warnings = Vec::new();
    for stretch in &query.timeline.stretches {
        dashboard_text::metadata::check_cancel(cancelled)?;
        let lines = super::cached_construction(source, stretch.run)?;
        let ids: BTreeSet<u32> = lines.within(stretch.first, stretch.last).filter_map(|line| line.building).collect();
        super::spellings(query, stretch.run, stretch.last, &ids, &mut tokens)?;
        let links = dashboard_base::queries::links(source, stretch.run)?;
        for project in window_projects(source, stretch.run, stretch.first, stretch.last, &lines, &query.holders, &links)? {
            match by_id.remove(&project.id) {
                Some(before) => {
                    let merged = join_project(before, project);
                    by_id.insert(merged.id.clone(), merged);
                }
                None => {
                    by_id.insert(project.id.clone(), project);
                }
            }
        }
    }
    let named = content.name(source, &tokens, &BTreeSet::new(), &mut warnings);
    let types: HashMap<u32, TypeInfo> = named
        .types
        .into_iter()
        .map(|item| (item.id, TypeInfo { token: item.token, name: item.name, icon: item.icon }))
        .collect();
    let mut first_spelled: BTreeMap<String, (i32, Option<String>, Option<String>)> = BTreeMap::new();
    let mut projects: Vec<ConstructionProject> = by_id.into_values().collect();
    for project in &mut projects {
        for list in [&mut project.built, &mut project.unaccounted, &mut project.possible, &mut project.added] {
            list.shrink_to_fit();
        }
        project.runs.shrink_to_fit();
        project.outputs.shrink_to_fit();
    }
    projects.sort_by(|a, b| (a.first_read, a.kind, a.id.len(), &a.id).cmp(&(b.first_read, b.kind, b.id.len(), &b.id)));

    for project in &projects {
        let mut record_building = |building: Option<u32>, hour: i32| {
            let Some(id) = building else { return };
            let Some(info) = types.get(&id) else { return };
            let Some(token) = info.token.as_ref() else { return };
            first_spelled
                .entry(token.clone())
                .and_modify(|held| {
                    if hour < held.0 {
                        held.0 = hour;
                    }
                })
                .or_insert_with(|| (hour, info.name.clone(), info.icon.clone()));
        };
        if let Some(origin) = &project.output_origin {
            record_building(origin.building, project.first_read);
            if let Some(census) = project.census_read {
                record_building(project.building, census);
            }
        } else {
            record_building(project.building, project.first_read);
        }
    }
    let spellings: Vec<String> = first_spelled.keys().cloned().collect();
    let chips: Vec<ConstructionTypeChip> = first_spelled
        .into_iter()
        .map(|(token, (first_hour, name, icon))| ConstructionTypeChip { token, first_hour, name, icon })
        .collect();

    let mut occurrences = Vec::with_capacity(projects.len());
    for project in projects {
        let id: u128 = project.id.parse().map_err(|_| invalid(format!("Invalid construction line id {}", project.id)))?;
        let changes = changes_of(&project, last);
        let observed = project.first_read;
        let date = project.created_hour.map_or(observed, |created| created.min(observed));
        let mut states: Vec<(i32, u8)> = Vec::new();
        let mut changed_steps: Vec<(i32, Option<SortValue>)> = Vec::new();
        for hour in state_hours(&project, last) {
            let Some(state) = project_state_at(&project, hour) else { continue };
            let has_token = state.building.and_then(|b| types.get(&b)).and_then(|info| info.token.as_ref()).is_some();
            let code = state_code(&state, has_token);
            if states.last().is_none_or(|&(_, held)| held != code) {
                states.push((hour, code));
            }
            let changed_val = Some(SortValue::Number(i64::from(state.changed_hour)));
            if changed_steps.last().is_none_or(|(_, held)| held != &changed_val) {
                changed_steps.push((hour, changed_val));
            }
        }
        let primary_building = project.building.or_else(|| project.output_origin.as_ref().and_then(|origin| origin.building));
        let primary_info = primary_building.and_then(|b| types.get(&b));
        let kind = primary_info
            .and_then(|info| info.token.as_deref())
            .and_then(|token| spellings.binary_search_by(|held| held.as_str().cmp(token)).ok())
            .map_or(0, |at| at as u16 + 1);
        let mut roles = Vec::new();
        if let Some(origin) = &project.output_origin {
            roles.push(Role::at(origin.country_slot, observed));
            if let Some(census) = project.census_read
                && project.country_slot != origin.country_slot
            {
                roles.push(Role::at(project.country_slot, census));
            }
        } else {
            roles.push(Role::at(project.country_slot, observed));
        }
        let mut text: Vec<&str> = Vec::new();
        if let Some(slot) = project.country_slot
            && let Some(tag) = query.holders.slot_tag(slot, observed)
        {
            text.push(tag);
        }
        if let Some(info) = primary_info {
            if let Some(token) = info.token.as_deref() {
                text.push(token);
            }
            if let Some(name) = info.name.as_deref() {
                text.push(name);
            }
        }
        occurrences.push(Occurrence {
            id,
            date,
            observed,
            kind,
            roles: roles.into_boxed_slice(),
            subjects: Box::new([]),
            states: states.into_boxed_slice(),
            sort_keys: Box::new([SortKey::Dated(changed_steps.into_boxed_slice())]),
            text: text.join(" ").into(),
            payload: IndexedProject { project, changes: changes.into_boxed_slice() },
        });
    }

    let mut seen_warnings = BTreeSet::new();
    warnings.retain(|warning| seen_warnings.insert(warning.clone()));
    Ok(Tail {
        index: Index::new(occurrences, missing(query, first, last), &LIVE_STATES)?,
        spellings,
        chips,
        types,
        repair_icon: named.repair_icon,
        factory_cap: recorded_factory_cap(content),
        unavailable: (!recorded).then(|| "Construction was not recorded.".to_owned()),
        warnings,
    })
}

fn recorded_factory_cap(content: &Content) -> Result<i32, String> {
    factory_cap(content.catalogue().map_err(str::to_owned).and_then(|catalogue| catalogue.defines()))
}

fn factory_cap(defines: Result<&content_reader_defines::defines::Defines, String>) -> Result<i32, String> {
    let missing = |why: String| format!("The most factories a construction line takes is not recorded: {why}");
    let cap = defines.and_then(|defines| defines.number("NProduction", "MAX_CIV_FACTORIES_PER_LINE").map_err(|why| why.to_string())).map_err(missing)?;
    if cap.fract() != 0.0 || cap < 1.0 || cap > f64::from(i32::MAX) {
        return Err(missing(format!("NProduction.MAX_CIV_FACTORIES_PER_LINE is {cap}, not a whole number from 1 to {}", i32::MAX)));
    }
    Ok(cap as i32)
}

fn tail(source: &Source, query: &Query<'_>, branch: u64, content: &Content, cancelled: &AtomicBool) -> io::Result<Arc<Tail>> {
    let made = Made::of(content);
    let kept = source.state::<Kept>();
    if let Some((held, found)) = kept.lock().unwrap_or_else(PoisonError::into_inner).0.get(&branch)
        && held.same(&made)
    {
        return Ok(Arc::clone(found));
    }
    let built = Arc::new(scan(source, query, content, cancelled)?);
    kept.lock().unwrap_or_else(PoisonError::into_inner).0.insert(branch, (made, Arc::clone(&built)));
    Ok(built)
}

struct Prepared {
    tail: Arc<Tail>,
    selection: Option<Selection>,
    places: Mutex<RowNames>,
}

fn prepare(source: &Source, query: &Query<'_>, branch: u64, load: &LoadContext, params: &ConstructionLogTailParams, cancelled: &AtomicBool) -> io::Result<Prepared> {
    let selection = dashboard_world::products::group_members::dated(source, query, load, params.countries.as_deref(), params.group.as_ref(), cancelled)?;
    Ok(Prepared { tail: tail(source, query, branch, &Content::load(source, load), cancelled)?, selection, places: Mutex::default() })
}

fn build_row(
    tail: &Tail,
    holders: &Holders,
    indexed: &IndexedProject,
    live: bool,
    hour: i32,
    order_start: bool,
) -> Option<ConstructionLogRow> {
    let project = &indexed.project;
    let state = project_state_at(project, hour)?;
    let (row_hour, row_exact) = if order_start {
        (state.start_hour, state.start_exact)
    } else {
        (state.changed_hour, state.changed_exact)
    };
    let owner = match state.owner_slot {
        None => ConstructionOwner { tag: None, slot: None, missing: Some("The construction line's owner is not recorded".to_owned()) },
        Some(slot) => match holders.slot_tag(slot, project.first_read) {
            Some(tag) => ConstructionOwner { tag: Some(tag.to_owned()), slot: Some(slot), missing: None },
            None => ConstructionOwner { tag: None, slot: Some(slot), missing: Some("The owner's country holds no tag at the project's first read".to_owned()) },
        },
    };
    let (building_token, building_name, building_missing, icon) = match state.building {
        None => (None, None, Some("Building type was not recorded".to_owned()), None),
        Some(id) => match tail.types.get(&id) {
            Some(info) => (
                info.token.clone(),
                info.name.clone(),
                (info.name.is_none() && info.token.is_none()).then(|| "Building type is not in the recorded dictionary".to_owned()),
                info.icon.clone(),
            ),
            None => (None, None, Some("Building type is not in the recorded dictionary".to_owned()), None),
        },
    };
    let place = (state.state.is_some() || state.to_state.is_some() || state.province.is_some()).then_some(ConstructionPlaceAt {
        kind: project.kind,
        state: state.state,
        to_state: state.to_state,
        province: state.province,
        at_hour: row_hour,
        name: None,
        name_missing: None,
        to_name: None,
        to_name_missing: None,
    });
    let place_missing = place.is_none().then(|| {
        if project.kind == ProjectKind::Railway {
            "Railway route was not recorded".to_owned()
        } else {
            "Construction place was not recorded".to_owned()
        }
    });
    let (levels, levels_missing) = if !state.has_count_evidence {
        let reason = if state.source == ProjectSource::OutputAlone {
            "Levels not recorded: line is known from factory output only".to_owned()
        } else {
            "Construction levels were not recorded".to_owned()
        };
        (None, Some(reason))
    } else if state.counters_unread && state.built == 0 && state.unknown == 0 && state.possible == 0 {
        (None, Some("Construction level counters were not recorded at this hour".to_owned()))
    } else {
        (
            Some(ConstructionLevelsAt {
                built: state.built,
                total: state.total,
                unknown: state.unknown,
                possible: state.possible,
                added: state.added,
                earlier_unknown: state.earlier_unknown,
                counters_unread: state.counters_unread,
            }),
            None,
        )
    };
    let factories_missing = (live && state.factories.is_none()).then(|| {
        if state.source == ProjectSource::OutputAlone {
            "Assigned factories not recorded: line is known from output only".to_owned()
        } else {
            "Assigned factories were not recorded at this hour".to_owned()
        }
    });
    Some(ConstructionLogRow {
        key: project.id.clone(),
        hour: row_hour,
        exact: row_exact,
        observed: project.first_read,
        live,
        kind: project.kind,
        source: state.source,
        status: state.status,
        owner,
        building_token,
        building_name,
        building_missing,
        icon,
        place,
        place_missing,
        levels,
        levels_missing,
        factories: state.factories,
        last_factories: state.last_factories,
        factories_missing,
        created_hour: state.created_hour,
        first_read: project.first_read,
        cost_raw: state.cost_raw.map(|cost| cost.to_string()),
        base_level: state.base_level,
        target_level: state.target_level,
        route_provinces: state.route_provinces,
        end: state.end.map(|end| ConstructionEndAt { kind: end.kind, hour: end.hour, known_at: end.known_at, unknown: end.unknown }),
        output: state.output.map(|out| ConstructionOutputAt {
            construction_raw: out.construction_raw.map(|amount| amount.to_string()),
            repair_raw: out.repair_raw.map(|amount| amount.to_string()),
            reason: out.reason,
            before_observation: out.before_observation,
        }),
    })
}

fn attach_places(
    reply: &mut ConstructionLogTailReply, prepared: &Prepared, query: &Query<'_>,
    (source, load): (&Source, &LoadContext), hour: i32, cancelled: &AtomicBool,
) -> io::Result<()> {
    let points: BTreeSet<_> = reply.rows.iter().filter_map(|row| row.place.as_ref()).filter(|place| place.at_hour <= hour)
        .flat_map(|place| place.points().into_iter().flatten()).collect();
    let mut held = prepared.places.lock().unwrap_or_else(PoisonError::into_inner);
    held.update(source, query, load, points, cancelled)?;
    for place in reply.rows.iter_mut().filter_map(|row| row.place.as_mut()) {
        let [first, last] = place.points();
        if place.at_hour > hour {
            let reason = || "Location is only recorded after this hour".to_owned();
            place.name_missing = first.map(|_| reason());
            place.to_name_missing = last.map(|_| reason());
            reply.next_change = reply.next_change.into_iter().chain([place.at_hour]).min();
        } else {
            let name = |point| held.get(&point);
            if let Some(value) = first.and_then(name) {
                place.name.clone_from(&value.name);
                place.name_missing.clone_from(&value.missing);
            }
            if let Some(value) = last.and_then(name) {
                place.to_name.clone_from(&value.name);
                place.to_name_missing.clone_from(&value.missing);
            }
        }
    }
    held.trim();
    Ok(())
}

fn load_tail_at(prepared: &Prepared, query: &Query<'_>, params: &ConstructionLogTailParams, hour: i32, cancelled: &AtomicBool, places: Option<(&Source, &LoadContext)>) -> io::Result<ConstructionLogTailReply> {
    dashboard_text::metadata::check_cancel(cancelled)?;
    let tail = &prepared.tail;
    let index = &tail.index;
    let order_start = params.order == Some(ConstructionOrder::Start);
    let (order, live_first) = match params.sort.as_ref() {
        Some(sort) => occurrences::order(Some(sort), &["changed"])?,
        None if order_start => (Order::Date(Direction::Descending), true),
        None => (Order::Key(0, Direction::Descending), true),
    };
    let (offset, limit) = occurrences::window(params.offset.as_deref(), params.limit)?;
    let kinds: Option<Vec<u16>> = params
        .building_types
        .as_ref()
        .map(|asked| asked.iter().filter_map(|token| tail.spellings.binary_search(token).ok()).map(|at| at as u16 + 1).collect());
    let work = params.projects.unwrap_or(ConstructionWorkFilter::All);
    let only_queue = params.in_queue == Some(true);
    let states: Option<Vec<u8>> = (work != ConstructionWorkFilter::All || only_queue || kinds.is_some()).then(|| {
        ALL_STATES
            .iter()
            .copied()
            .filter(|&code| {
                (!only_queue || (code & LIVE_BIT) != 0)
                    && (kinds.is_none() || (code & TYPED_BIT) != 0)
                    && match work {
                        ConstructionWorkFilter::All => true,
                        ConstructionWorkFilter::New => (code & NEW_BIT) != 0,
                        ConstructionWorkFilter::Repair => (code & REPAIR_BIT) != 0,
                    }
            })
            .collect()
    });
    let answer = index.ask(
        &query.holders,
        &Ask {
            hour,
            from: params.from,
            to: params.to,
            gap_end: None,
            selection: prepared.selection.as_ref(),
            subject: None,
            kinds: kinds.as_deref(),
            states: states.as_deref(),
            search: params.search.as_deref(),
            order,
            live_first,
            offset,
            limit,
            chips: &[],
        },
    )?;
    let mut next_change = answer.next_change;
    let mut rows = Vec::with_capacity(answer.rows.len());
    for shown in &answer.rows {
        let indexed = &index.get(shown.at).payload;
        rows.push(
            build_row(tail, &query.holders, indexed, shown.live, hour, order_start)
                .ok_or_else(|| invalid(format!("Construction line {} is listed before its first read", indexed.project.id)))?,
        );
        if let Some(&later) = indexed.changes.get(indexed.changes.partition_point(|&at| at <= hour)) {
            next_change = Some(next_change.map_or(later, |held| held.min(later)));
        }
    }
    if let Some(later) = tail.chips.iter().filter(|chip| chip.first_hour > hour).map(|chip| chip.first_hour).min() {
        next_change = Some(next_change.map_or(later, |held| held.min(later)));
    }
    let mut reply = ConstructionLogTailReply {
        branch_id: params.branch_id.clone(),
        total: answer.total.to_string(),
        offset: answer.offset.to_string(),
        live: answer.live.to_string(),
        counts: Vec::new(),
        building_types: tail.chips.iter().filter(|chip| chip.first_hour <= hour).cloned().collect(),
        repair_icon: tail.repair_icon.clone(),
        factory_cap: tail.factory_cap.as_ref().ok().copied(),
        factory_cap_missing: tail.factory_cap.as_ref().err().cloned(),
        cost_scale: COST_SCALE,
        rows,
        missing: answer.gaps,
        next_change,
        unavailable: tail.unavailable.clone(),
        warnings: tail.warnings.clone(),
    };
    if let Some(places) = places { attach_places(&mut reply, prepared, query, places, hour, cancelled)?; }
    dashboard_base::products::check_reply_budget(&reply)?;
    Ok(reply)
}

pub struct ConstructionLogTail;

impl Product for ConstructionLogTail {
    const PREPARE: Option<dashboard_base::products::read_ahead::Prepare<Self::Params>> = Some(|source, params, load| {
        let places = if params.place_names == Some(true) { place_name_runs::prepare(source, load)? } else { None };
        dashboard_base::products::read_ahead::together(places, super::prepare_naming(source, load)?)
    });
    const KEY: &'static str = "construction-log-tail";
    const CONTENT_DEPENDENT: bool = true;
    const INTERACTIVE: bool = true;
    type Params = ConstructionLogTailParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![
        branchId,
        hour: Read,
        from: Start,
        to: End,
        countries,
        "countries[]",
        group,
        search,
        sort,
        offset,
        limit,
        buildingTypes,
        "buildingTypes[]",
        projects,
        inQueue,
        order,
        placeNames
    ];
    type Reply = ConstructionLogTailReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            ConstructionLogTailParams::decl(),
            ProjectKind::decl(),
            ProjectSource::decl(),
            SpendingGap::decl(),
            EndKind::decl(),
            UnknownEnd::decl(),
            ConstructionStatus::decl(),
            ConstructionWorkFilter::decl(),
            ConstructionOrder::decl(),
            ConstructionOwner::decl(),
            ConstructionPlaceAt::decl(),
            ConstructionLevelsAt::decl(),
            ConstructionEndAt::decl(),
            ConstructionOutputAt::decl(),
            ConstructionTypeChip::decl(),
            ConstructionLogRow::decl(),
            ConstructionLogTailReply::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map(|_| ()).map_err(|_| "Invalid branch")?;
        params.countries.iter().flatten().try_for_each(dashboard_base::products::validate_country)
    }

    fn load(source: &Source, params: &Self::Params, load: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let branch = params.branch_id.parse().map_err(|_| invalid("Invalid branch"))?;
        let query = dashboard_base::queries::query(source, branch)?;
        let hour = occurrences::cutoff(&query, params.hour)?;
        let prepared = prepare(source, &query, branch, load, params, cancelled)?;
        load_tail_at(&prepared, &query, params, hour, cancelled, (params.place_names == Some(true)).then_some((source, load)))
    }
}

struct Owned {
    replay: Arc<Replay>,
    timeline: Arc<Timeline>,
    holders: Arc<Holders>,
}

pub fn dated_panel(source: &Arc<Source>, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    let mut request = asked.clone();
    request.as_object_mut().ok_or("Invalid construction-log-tail panel")?.remove("panel");
    let params: ConstructionLogTailParams = dashboard_query::named::named(&request).map_err(|error| format!("Invalid construction-log-tail panel: {error}"))?;
    ConstructionLogTail::validate(&params)?;
    let places = if params.place_names == Some(true) {
        place_name_runs::prepare(source, load).map_err(|error| error.to_string())?
    } else { None };
    let Some(artwork) = super::prepare_naming(source, load).map_err(|error| error.to_string())? else {
        return place_name_runs::prepared_panel(places, open(source, load, params, None)?);
    };
    let branch = params.branch_id.parse().map_err(|_| "Invalid branch")?;
    let query = dashboard_base::queries::query(source, branch).map_err(|error| error.to_string())?;
    let (selection, notes) = catalogue::capture(|| dashboard_world::products::group_members::dated(
        source, &query, load, params.countries.as_deref(), params.group.as_ref(), &AtomicBool::new(false),
    ));
    let selection = selection.map_err(|error| error.to_string())?;
    let ready = Arc::<Preparation>::default();
    let readers = Arc::<OnceLock<(ProductReader, ExactReader)>>::default();
    let (build, source, load) = (Arc::clone(&readers), Arc::downgrade(source), LoadContext { content: load.content.clone() });
    ready.start("construction log panel preparation", move || {
        artwork.wait()?;
        let source = source.upgrade().ok_or(io::ErrorKind::Interrupted)?;
        let panel = source.in_scope(|| open(&source, &load, params, Some((selection, notes)))).map_err(io::Error::other)?;
        let exact = panel.exact().expect("construction log has an exact reader");
        if let Some(places) = places { places.wait()?; }
        let _ = build.set((Arc::clone(&exact.advance), Arc::clone(&exact.cold)));
        Ok(())
    });
    let cold = Arc::clone(&readers);
    place_name_runs::prepared_panel(Some(ready), product_panel(
        Arc::new(move |hour, seed, cancelled, step| readers.get().expect("prepared construction log").0(hour, seed, cancelled, step)),
        Arc::new(move |hour, cancelled| cold.get().expect("prepared construction log").1(hour, cancelled)),
    ))
}

fn open(source: &Arc<Source>, load: &LoadContext, params: ConstructionLogTailParams, captured: Option<(Option<Selection>, catalogue::OwnRefusals)>) -> Result<Box<dyn Panel>, String> {
    let branch = params.branch_id.parse().map_err(|_| "Invalid branch")?;
    let query = dashboard_base::queries::query(source, branch).map_err(|error| error.to_string())?;
    let cancelled = AtomicBool::new(false);
    let (finish, prepared) = dashboard_base::products::Finish::open(source, load, ConstructionLogTail::CONTENT_DEPENDENT, || {
        if let Some((selection, notes)) = captured {
            catalogue::credit(&notes);
            let catalogue = catalogue::bound_catalogue(source, load.content()?).ok_or(io::ErrorKind::Interrupted)?;
            let content = Content { catalogue: catalogue.map_err(|error| error.to_string()) };
            Ok(Prepared { tail: tail(source, &query, branch, &content, &cancelled)?, selection, places: Mutex::default() })
        } else {
            prepare(source, &query, branch, load, &params, &cancelled)
        }
    });
    let mut prepared = prepared.map_err(|error| error.to_string())?;
    if params.place_names == Some(true) {
        let mut places = RowNames::following();
        let index = &prepared.tail.index;
        let order_start = params.order == Some(ConstructionOrder::Start);
        places.hints((0..index.len() as u32).flat_map(|at| place_hints(&index.get(at).payload.project, order_start)));
        prepared.places = Mutex::new(places);
    }
    let owned = Owned { replay: source.replay(), timeline: Arc::clone(&query.timeline), holders: Arc::clone(&query.holders) };
    let opened = (params.place_names == Some(true)).then(|| Arc::downgrade(source));
    let panel_load = LoadContext { content: load.content.clone() };
    Ok(occurrences::occurrence_panel(source.tables(), finish, move |hour, cancelled| {
        let query = Query { replay: &owned.replay, timeline: Arc::clone(&owned.timeline), holders: Arc::clone(&owned.holders) };
        let hour = occurrences::cutoff(&query, Some(hour))?;
        let source = opened.as_ref().map(|source| source.upgrade().ok_or_else(|| io::Error::other("Recording closed"))).transpose()?;
        load_tail_at(&prepared, &query, &params, hour, cancelled, source.as_deref().map(|source| (source, &panel_load)))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashboard_base::test_fixtures::{CAMPAIGN, session};
    use dashboard_world::products::place_name_runs::{PlaceNameRuns, PlaceNameRunsParams};
    use replay_data::rows::{BuildingConstructionRow, BuildingRow, CampaignRow, ClockRow, ConstructionCompletion, ConstructionLineOutputRow, CountryLinkRow, CountryRow, LexicalTokenRow, RailwayConstructionRow, Recorded, TableId};
    use replay_frame::branches::{BranchReason, BranchStart, Parentage};
    use replay_journal::synthetic::{Input, Sample};

    #[test]
    fn a_lines_factory_cap_is_the_recorded_define_and_one_that_cannot_be_read_is_named() {
        let defines = |text: &str| {
            let defines = content_reader_defines::defines::Defines::new().unwrap();
            defines.load("common/defines/00_defines.lua", text.as_bytes()).unwrap();
            defines
        };
        assert_eq!(factory_cap(Ok(&defines("NDefines = { NProduction = { MAX_CIV_FACTORIES_PER_LINE = 37 } }"))), Ok(37), "a mod's cap, not a built-in one");
        for (text, why) in [
            ("NDefines = { NProduction = { MAX_CIV_FACTORIES_PER_LINE = 2.5 } }", "is 2.5, not a whole number from 1 to 2147483647"),
            ("NDefines = { NProduction = { MAX_CIV_FACTORIES_PER_LINE = 0 } }", "is 0, not a whole number from 1 to 2147483647"),
            ("NDefines = { NProduction = {} }", "is not a number"),
        ] {
            let refused = factory_cap(Ok(&defines(text))).unwrap_err();
            assert!(refused.starts_with("The most factories a construction line takes is not recorded") && refused.ends_with(why), "{text}: {refused}");
        }
        assert!(factory_cap(Err("no recorded content".to_owned())).unwrap_err().ends_with("no recorded content"));
    }

    fn make_source() -> Source {
        let campaign = CampaignRow { id: CAMPAIGN.into() };
        let branch = BranchStart { id: 1, session: 1, loaded_hour: None, first_hour: 100, parent: Parentage::Unknown, reason: BranchReason::SessionStart };
        let countries = [CountryRow { id: 9001, canonical_slot: 7, tag: b"GER".to_vec(), ..Default::default() }];
        let links = [CountryLinkRow { id: 9001, canonical_slot: 7 }];
        let replay = Replay::from_synthetic(recorder_derive::writers(), CAMPAIGN.into(), session(1), branch, 125, |frame, hour| {
            frame.table(TableId::Clock, Input::Snapshot { rows: &[ClockRow { hour, ..Default::default() }], partial: None, read: true, sample: Sample::FromReadBit })?;
            frame.table(TableId::Campaign, Input::Snapshot { rows: std::slice::from_ref(&campaign), partial: None, read: true, sample: Sample::FromReadBit })?;
            frame.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit })?;
            frame.table(TableId::CountryLinks, Input::Snapshot { rows: &links, partial: None, read: true, sample: Sample::FromReadBit })?;
            let mut rows = Vec::new();
            if hour < 120 {
                rows.push(BuildingConstructionRow {
                    id: 10,
                    country: 9001,
                    queue_ordinal: 0,
                    active_factories: if hour < 105 { 0 } else { 10 },
                    cost: 1_080_000_000,
                    amount: 1,
                    created_hour: 95,
                    building_type: 300,
                    target_location: 64,
                    target_state: 64,
                    repair_levels: 0,
                    ..Default::default()
                });
            }
            if hour >= 122 {
                rows.push(BuildingConstructionRow {
                    id: 11,
                    country: 9001,
                    queue_ordinal: 0,
                    active_factories: 5,
                    cost: 720_000_000,
                    amount: 0,
                    created_hour: 122,
                    building_type: 301,
                    target_location: 65,
                    target_state: 65,
                    repair_levels: 1,
                    ..Default::default()
                });
            }
            if hour < 121 {
                rows.push(BuildingConstructionRow {
                    id: 12,
                    country: 9001,
                    queue_ordinal: 1,
                    active_factories: 10,
                    cost: 1_080_000_000,
                    amount: if hour < 120 { 2 } else { 1 },
                    created_hour: 96,
                    building_type: 300,
                    target_location: 66,
                    target_state: 66,
                    repair_levels: 0,
                    ..Default::default()
                });
            }
            frame.table(TableId::BuildingConstruction, Input::Snapshot { rows: &rows, partial: None, read: true, sample: Sample::FromReadBit })?;
            frame.table(TableId::RailwayConstruction, Input::Snapshot { rows: &[] as &[RailwayConstructionRow], partial: None, read: true, sample: Sample::FromReadBit })?;
            let state_buildings = [
                BuildingRow {
                    location: 64,
                    building_type: 300,
                    country: 9001,
                    level: if hour >= 120 { 3 } else { 2 },
                    healthy_level: if hour >= 120 { 3 } else { 2 },
                    active: true,
                    ..Default::default()
                },
                BuildingRow {
                    location: 66,
                    building_type: 300,
                    country: 9001,
                    level: if hour >= 120 { 3 } else { 2 },
                    healthy_level: if hour >= 120 { 3 } else { 2 },
                    active: true,
                    ..Default::default()
                },
            ];
            frame.table(TableId::StateBuildings, Input::Snapshot { rows: &state_buildings, partial: None, read: true, sample: Sample::FromReadBit })?;
            frame.table(TableId::ProvinceBuildings, Input::Snapshot { rows: &[] as &[BuildingRow], partial: None, read: true, sample: Sample::FromReadBit })?;
            let outputs: Vec<ConstructionLineOutputRow> = match hour {
                120 => vec![ConstructionLineOutputRow {
                    line: 10,
                    construction_raw: 1_080_000_000,
                    exact: true,
                    ended: true,
                    completion: Recorded::Value(Some(ConstructionCompletion { hour: 120, cancelled: false })),
                    country: Recorded::Value(Some(9001)),
                    building_type: Recorded::Value(Some(300)),
                    target_state: Recorded::Value(Some(64)),
                    observed_since_creation: true,
                    ..Default::default()
                }],
                121 => vec![ConstructionLineOutputRow {
                    line: 12,
                    construction_raw: 2_160_000_000,
                    exact: true,
                    ended: true,
                    completion: Recorded::Value(Some(ConstructionCompletion { hour: 121, cancelled: false })),
                    country: Recorded::Value(Some(9001)),
                    building_type: Recorded::Value(Some(300)),
                    target_state: Recorded::Value(Some(66)),
                    observed_since_creation: true,
                    ..Default::default()
                }],
                _ => Vec::new(),
            };
            frame.table(TableId::ConstructionLineOutput, Input::Snapshot { rows: &outputs, partial: None, read: true, sample: Sample::FromReadBit })?;
            let mut tokens = vec![LexicalTokenRow { token: 300, literal: b"industrial_complex".to_vec() }];
            if hour >= 122 {
                tokens.push(LexicalTokenRow { token: 301, literal: b"arms_factory".to_vec() });
            }
            frame.table(TableId::LexicalTokens, Input::Snapshot { rows: &tokens, partial: None, read: true, sample: Sample::FromReadBit })?;
            Ok(())
        })
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
        Source::from_replay(dashboard_base::SourceKey { source_id: "construction-tail".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap()
    }

    fn ask(hour: i32) -> ConstructionLogTailParams {
        ConstructionLogTailParams {
            branch_id: "1".into(),
            hour: Some(hour),
            from: None,
            to: None,
            countries: None,
            group: None,
            search: None,
            sort: None,
            offset: None,
            limit: 64,
            building_types: None,
            projects: None,
            in_queue: None,
            order: None,
            place_names: None,
        }
    }

    #[test]
    fn a_new_construction_location_reuses_the_followed_place_history() {
        use dashboard_base::test_fixtures::{source, source_with_content, visual_mods::Machine};
        use replay_data::rows::{StateRow, TableRows};
        use replay_frame::frame::TableView;
        use replay_recording::cells::count_reads;

        let machine = Machine::new("construction_place_history");
        let frames = (718..=720).map(|hour| {
            let mut frame = source(1, hour, Some(1));
            let project = |id, created_hour, state, queue_ordinal| BuildingConstructionRow {
                id, country: 9001, queue_ordinal, active_factories: 1, amount: 1, created_hour,
                building_type: 300, target_location: state, target_state: state, ..Default::default()
            };
            let mut buildings = vec![project(10, 718, 64, 0)];
            if hour >= 720 {
                buildings.push(project(11, 719, 65, 1));
            }
            for rows in [
                TableRows::Clock(vec![ClockRow { hour, ..Default::default() }]),
                TableRows::States(vec![
                    StateRow { id: 64, custom_name: b"North Reach".to_vec(), ..Default::default() },
                    StateRow { id: 65, custom_name: b"South Reach".to_vec(), ..Default::default() },
                ]),
                TableRows::BuildingConstruction(buildings),
                TableRows::RailwayConstruction(vec![]),
            ] {
                let table = rows.table();
                frame.tables[table.index()] = TableView::Complete(rows).into();
                frame.metrics.table_ns[table.index()] = 1;
            }
            frame
        }).collect();
        let (source, load) = source_with_content(frames, &machine.root);
        let source = Arc::new(source);
        let cancelled = AtomicBool::new(false);
        let panel = dated_panel(&source, &load, &serde_json::json!({ "panel": "construction-log-tail", "branchId": "1", "limit": 64, "placeNames": true })).unwrap();
        let reader = panel.exact().unwrap();
        let mut seed = (reader.advance)(718, true, &cancelled, None);
        if seed.pending {
            let wake = panel.pending_preparation().and_then(|prepare| prepare(718)).expect("pending construction panel has preparation");
            let (ready, waited) = std::sync::mpsc::sync_channel(1);
            wake(Arc::new(move || { let _ = ready.send(()); }));
            waited.recv().unwrap();
            seed = (reader.advance)(718, true, &cancelled, None);
        }
        let seed = seed.value.as_ref().as_ref().unwrap();
        assert_eq!(seed["rows"][0]["place"]["name"], "North Reach");
        let (followed, warm) = count_reads(|| (reader.advance)(720, false, &cancelled, None));
        let params = ConstructionLogTailParams { place_names: Some(true), ..ask(720) };
        let (exact, cold) = count_reads(|| ConstructionLogTail::load(&source, &params, &load, &cancelled).unwrap());
        assert!(exact.rows.iter().any(|row| row.place.as_ref().and_then(|place| place.name.as_deref()) == Some("South Reach")));
        assert_eq!(followed.value.as_ref().as_ref().unwrap()["rows"], serde_json::to_value(&exact.rows).unwrap());
        let state_reads = |reads: &replay_recording::cells::ReadCounts| reads.get(&TableId::States).map_or((0, 0), |stats| (stats.opened, stats.restores));
        assert!(state_reads(&cold).0 > 0, "the exact lookup needs recorded state operands");
        assert_eq!(state_reads(&warm), (0, 0), "the newly shown location was in the retained broker window");
    }

    #[test]
    fn construction_tail_transitions_stepped_equals_cold_and_withholds_future_chips() {
        let source = Arc::new(make_source());
        let load = LoadContext { content: None };
        let panel = dated_panel(&source, &load, &serde_json::json!({ "panel": "construction-log-tail", "branchId": "1", "limit": 64, "placeNames": true })).unwrap();
        let exact = panel.exact().unwrap();
        let cancelled = AtomicBool::new(false);
        let name_asked = PlaceNameRunsParams { branch_id: "1".into(), land: false, from: 100, to: 100, states: vec![64, 66], provinces: Vec::new() };
        let missing = PlaceNameRuns::load(&source, &name_asked, &load, &cancelled).unwrap_err().to_string();
        for (hour, seed) in [(100, true), (104, false), (105, false), (119, false), (120, false), (121, false), (122, false), (110, true)] {
            let stepped = (exact.advance)(hour, seed, &cancelled, None);
            let cold = (exact.cold)(hour, &cancelled);
            assert_eq!(*stepped.value, cold, "stepped equals cold at hour {hour}");
            let params = ConstructionLogTailParams { place_names: Some(true), ..ask(hour) };
            let named = ConstructionLogTail::load(&source, &params, &load, &cancelled).unwrap();
            assert_eq!(cold.unwrap()["rows"], serde_json::to_value(&named.rows).unwrap());
            for place in named.rows.iter().filter_map(|row| row.place.as_ref()) {
                let expected = if place.at_hour < 100 { "Place name not answered" } else { missing.as_str() };
                assert_eq!(place.name, None);
                assert_eq!(place.name_missing.as_deref(), Some(expected), "place hour {}", place.at_hour);
            }
        }
        let at100 = ConstructionLogTail::load(&source, &ask(100), &load, &AtomicBool::new(false)).unwrap();
        assert_eq!(at100.rows.len(), 2);
        fn row_of<'a>(reply: &'a ConstructionLogTailReply, key: &str) -> &'a ConstructionLogRow {
            reply.rows.iter().find(|row| row.key == key).expect("row")
        }
        assert_eq!(row_of(&at100, "10").status, ConstructionStatus::Waiting);
        assert!(row_of(&at100, "10").place.as_ref().unwrap().name_missing.is_none(), "legacy queries omit place-name fields");
        assert_eq!(row_of(&at100, "10").factories, Some(0));
        assert_eq!(at100.building_types.iter().map(|c| c.token.as_str()).collect::<Vec<_>>(), ["industrial_complex"]);
        assert_eq!(at100.next_change, Some(105));

        let at105 = ConstructionLogTail::load(&source, &ask(105), &load, &AtomicBool::new(false)).unwrap();
        assert_eq!(row_of(&at105, "10").status, ConstructionStatus::Building);
        assert_eq!(row_of(&at105, "10").factories, Some(10));
        assert_eq!(at105.next_change, Some(120));

        let at122 = ConstructionLogTail::load(&source, &ask(122), &load, &AtomicBool::new(false)).unwrap();
        assert_eq!(at122.rows.len(), 3);
        assert_eq!(at122.rows[0].key, "11", "live repairing project leads above completed projects");
        assert_eq!(at122.rows[0].status, ConstructionStatus::Repairing);
        let unwitnessed = row_of(&at122, "10");
        assert_eq!(unwitnessed.status, ConstructionStatus::Completed, "0 witnessed and 1 unaccounted level must not claim Built");
        assert_eq!(unwitnessed.levels.as_ref().map(|levels| (levels.built, levels.unknown)), Some((0, 1)));
        assert_eq!(unwitnessed.hour, 120, "completed project displays its end hour");
        let witnessed = row_of(&at122, "12");
        assert_eq!(witnessed.status, ConstructionStatus::Built, "a finished project with a witnessed built level is Built");
        assert_eq!(witnessed.levels.as_ref().map(|levels| levels.built), Some(1));
        assert_eq!(at122.building_types.iter().map(|c| c.token.as_str()).collect::<Vec<_>>(), ["arms_factory", "industrial_complex"]);
    }
}
