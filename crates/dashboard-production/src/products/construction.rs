
pub mod log;
pub mod tail;
pub mod breakdown;
pub mod spending;
#[cfg(test)]
mod tests;

use dashboard_base::Source;
use dashboard_base::catalogue::{self, Catalogue, View};
use dashboard_core::config::LoadContext;
use dashboard_query::{Query, read, series::MissingSpan};
use content_reader_layers::content::diagnostics::Diagnostics;
use content_reader_gui::gui::native;
use replay_lanes::lanes::Cell;
use replay_data::rows::{ConstructionLineEndRow, ConstructionProjectRow as P, TableId, column};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    sync::{Arc, Mutex, PoisonError},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub(crate) mod col {
    pub const KIND: usize = 0;
    pub const OUTPUT_ALONE: usize = 1;
    pub const COUNTRY: usize = 2;
    pub const BUILDING_TYPE: usize = 3;
    pub const TARGET_STATE: usize = 4;
    pub const TO_STATE: usize = 5;
    pub const PROVINCE: usize = 6;
    pub const ROUTE_PROVINCES: usize = 7;
    pub const BASE_LEVEL: usize = 8;
    pub const TARGET_LEVEL: usize = 9;
    pub const COST: usize = 10;
    pub const CREATED_HOUR: usize = 11;
    pub const FIRST_READ: usize = 12;
    pub const CENSUS_READ: usize = 13;
    pub const QUEUE_ORDINAL: usize = 14;
    pub const EARLIER_UNKNOWN: usize = 15;
    pub const FACTORIES: usize = 16;
    pub const REMAINING: usize = 17;
    pub const REPAIR_LEVELS: usize = 18;
    pub const BUILT: usize = 19;
    pub const BUILT_RISES: usize = 20;
    pub const REPAIRED: usize = 21;
    pub const REPAIRED_RISES: usize = 22;
    pub const ADDED_NEW: usize = 23;
    pub const ADDED_REPAIR: usize = 24;
    pub const POSSIBLE: usize = 25;
    pub const UNACCOUNTED_NEW: usize = 26;
    pub const UNACCOUNTED_REPAIR: usize = 27;
    pub const END: usize = 28;
    pub const END_REASON: usize = 29;
    pub const END_HOUR: usize = 30;
    pub const END_KNOWN_AT: usize = 31;
    pub const CONSTRUCTION_RAW: usize = 32;
    pub const REPAIR_RAW: usize = 33;
    pub const OUTPUT_SINCE: usize = 34;
    pub const OBSERVED_SINCE_CREATION: usize = 35;
    pub const OUTPUT_REASON: usize = 36;
    pub const FIRST_REPAIR_TICK: usize = 37;
    pub const ALL: [usize; 38] = {
        let mut all = [0; 38];
        let mut at = 0;
        while at < 38 {
            all[at] = at;
            at += 1;
        }
        all
    };
}

pub const COST_SCALE: u32 = 100_000;
const ICON_EDGE: u32 = 48;
const LINE_ENTRY: &str = "production_building_line_entry";
const LINE_PICTURE: &str = "picture";
const LINE_REPAIR: &str = "repair";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ProjectKind {
    Building,
    Railway,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum SpendingGap {
    NotRecorded,
    LinkedOutput,
    InexactOutput,
    ObservationRestart,
    FinalOutputMissing,
    OwnerUnknown,
}

pub fn gap_of(reason: u8) -> Option<SpendingGap> {
    Some(match reason {
        P::OUTPUT_KNOWN => return None,
        P::OUTPUT_LINKED => SpendingGap::LinkedOutput,
        P::OUTPUT_INEXACT => SpendingGap::InexactOutput,
        P::OUTPUT_RESTARTED => SpendingGap::ObservationRestart,
        P::OUTPUT_FINAL_MISSING => SpendingGap::FinalOutputMissing,
        P::OUTPUT_OWNER_UNKNOWN => SpendingGap::OwnerUnknown,
        _ => SpendingGap::NotRecorded,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionType {
    pub id: u32,
    #[doc = " Its script name; null when the recorded dictionary lacks it."]
    pub token: Option<String>,
    pub name: Option<String>,
    #[doc = " The construction queue's picture of it."]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionStateName {
    pub id: u32,
    pub name: Option<String>,
}

pub(crate) fn number(cells: &[Cell], column: usize) -> Option<i64> {
    match cells.get(column) {
        Some(Cell::Number(value)) => Some(*value),
        _ => None,
    }
}

pub(crate) fn hour(cells: &[Cell], column: usize) -> Option<i32> {
    number(cells, column).and_then(|value| i32::try_from(value).ok())
}

pub(crate) fn id32(cells: &[Cell], column: usize) -> Option<u32> {
    number(cells, column).and_then(|value| u32::try_from(value).ok())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub id: u64,
    pub first: i32,
    pub last: i32,
    pub railway: bool,
    pub output_alone: bool,
    pub country: Option<u32>,
    pub first_read: i32,
    pub building: Option<u32>,
    pub state: Option<u32>,
    pub to_state: Option<u32>,
    pub new_work: bool,
    pub repair_work: bool,
    pub before_observation: bool,
    pub had_output: bool,
    pub output_reason: u8,
}

const LINE_COLUMNS: [usize; 22] = [
    col::KIND,
    col::OUTPUT_ALONE,
    col::COUNTRY,
    col::BUILDING_TYPE,
    col::TARGET_STATE,
    col::TO_STATE,
    col::FIRST_READ,
    col::REMAINING,
    col::REPAIR_LEVELS,
    col::BUILT,
    col::REPAIRED,
    col::ADDED_NEW,
    col::ADDED_REPAIR,
    col::POSSIBLE,
    col::UNACCOUNTED_NEW,
    col::UNACCOUNTED_REPAIR,
    col::CONSTRUCTION_RAW,
    col::REPAIR_RAW,
    col::OUTPUT_SINCE,
    col::OBSERVED_SINCE_CREATION,
    col::OUTPUT_REASON,
    col::FIRST_REPAIR_TICK,
];

#[derive(Default)]
pub struct RunLines {
    pub lines: Vec<Line>,
}

impl RunLines {
    pub fn within(&self, from: i32, to: i32) -> impl Iterator<Item = &Line> + '_ {
        let end = self.lines.partition_point(|line| line.first <= to);
        self.lines[..end].iter().filter(move |line| line.last >= from)
    }
}

pub(crate) fn line_of(id: u64, first: i32, last: i32, value: &dyn Fn(usize) -> Option<i64>) -> Line {
    let positive = |column: usize| value(column).is_some_and(|count| count > 0);
    let output_since = value(col::OUTPUT_SINCE);
    Line {
        id,
        first,
        last,
        railway: value(col::KIND) == Some(i64::from(P::RAILWAY)),
        output_alone: value(col::OUTPUT_ALONE) == Some(1),
        country: value(col::COUNTRY).and_then(|country| u32::try_from(country).ok()),
        first_read: value(col::FIRST_READ).and_then(|hour| i32::try_from(hour).ok()).unwrap_or(first),
        building: value(col::BUILDING_TYPE).and_then(|id| u32::try_from(id).ok()),
        state: value(col::TARGET_STATE).and_then(|id| u32::try_from(id).ok()),
        to_state: value(col::TO_STATE).and_then(|id| u32::try_from(id).ok()),
        new_work: [col::BUILT, col::ADDED_NEW, col::POSSIBLE, col::UNACCOUNTED_NEW, col::REMAINING, col::CONSTRUCTION_RAW].into_iter().any(positive),
        repair_work: [col::REPAIRED, col::ADDED_REPAIR, col::UNACCOUNTED_REPAIR, col::REPAIR_LEVELS, col::REPAIR_RAW].into_iter().any(positive)
            || value(col::FIRST_REPAIR_TICK).is_some(),
        before_observation: output_since.is_some() && value(col::OBSERVED_SINCE_CREATION) == Some(0),
        had_output: output_since.is_some(),
        output_reason: value(col::OUTPUT_REASON).and_then(|reason| u8::try_from(reason).ok()).unwrap_or(P::OUTPUT_NOT_RECORDED),
    }
}

mod end {
    use super::{ConstructionLineEndRow as E, column};
    pub const KIND: usize = column::<E>("kind");
    pub const OUTPUT_ALONE: usize = column::<E>("output_alone");
    pub const COUNTRY: usize = column::<E>("country");
    pub const BUILDING_TYPE: usize = column::<E>("building_type");
    pub const TARGET_STATE: usize = column::<E>("target_state");
    pub const TO_STATE: usize = column::<E>("to_state");
    pub const FIRST_READ: usize = column::<E>("first_read");
    pub const NEW_WORK: usize = column::<E>("new_work");
    pub const REPAIR_WORK: usize = column::<E>("repair_work");
    pub const BEFORE_OBSERVATION: usize = column::<E>("before_observation");
    pub const HAD_OUTPUT: usize = column::<E>("had_output");
    pub const OUTPUT_REASON: usize = column::<E>("output_reason");
}

pub(crate) fn line_of_end(id: u64, first: i32, last: i32, value: &dyn Fn(usize) -> Option<i64>) -> Line {
    let yes = |column: usize| value(column) == Some(1);
    Line {
        id,
        first,
        last,
        railway: value(end::KIND) == Some(i64::from(P::RAILWAY)),
        output_alone: yes(end::OUTPUT_ALONE),
        country: value(end::COUNTRY).and_then(|country| u32::try_from(country).ok()),
        first_read: value(end::FIRST_READ).and_then(|hour| i32::try_from(hour).ok()).unwrap_or(first),
        building: value(end::BUILDING_TYPE).and_then(|id| u32::try_from(id).ok()),
        state: value(end::TARGET_STATE).and_then(|id| u32::try_from(id).ok()),
        to_state: value(end::TO_STATE).and_then(|id| u32::try_from(id).ok()),
        new_work: yes(end::NEW_WORK),
        repair_work: yes(end::REPAIR_WORK),
        before_observation: yes(end::BEFORE_OBSERVATION),
        had_output: yes(end::HAD_OUTPUT),
        output_reason: value(end::OUTPUT_REASON).and_then(|reason| u8::try_from(reason).ok()).unwrap_or(P::OUTPUT_NOT_RECORDED),
    }
}

pub fn run_lines(source: &Source, run: u32) -> io::Result<RunLines> {
    let recording = source.replay_ref().recording();
    let lives = dashboard_base::queries::lifetimes(source, run, TableId::ConstructionProjects)?;
    let spans: HashMap<u64, (i32, i32)> = lives
        .iter()
        .filter_map(|&(key, first, last)| Some((u64::try_from(key).ok()?, (i32::try_from(first).ok()?, i32::try_from(last).ok()?))))
        .collect();
    let ends: Vec<(i32, u128)> = spans.iter().map(|(&id, &(_, last))| (last, ConstructionLineEndRow::key_of(last, id))).collect();
    let mut lines = Vec::with_capacity(ends.len());
    let mut open: Vec<(i32, u128)> = Vec::new();
    dashboard_query::cost::storage(|| {
        replay_lanes::lanes::at_each(recording, run, TableId::ConstructionLineEnds, None, &ends, &mut |hour, key, row| {
            let id = key as u64;
            let Some(&(first, last)) = spans.get(&id) else { return Ok(()) };
            match row {
                Some((view, slot)) => lines.push(line_of_end(id, first, last, &|column| view.number(column, slot))),
                None => open.push((hour, u128::from(id))),
            }
            Ok(())
        })
    })?;
    dashboard_query::cost::storage(|| {
        replay_lanes::lanes::at_each(recording, run, TableId::ConstructionProjects, Some(&LINE_COLUMNS[..]), &open, &mut |_, key, row| {
            if let (Some((view, slot)), Some(&(first, last))) = (row, spans.get(&(key as u64))) {
                lines.push(line_of(key as u64, first, last, &|column| view.number(column, slot)));
            }
            Ok(())
        })
    })?;
    lines.sort_unstable_by_key(|line| (line.first, line.id));
    Ok(RunLines { lines })
}

pub fn recorded(query: &Query<'_>) -> io::Result<bool> {
    let recording = query.replay.recording();
    for stretch in &query.timeline.stretches {
        for table in [TableId::BuildingConstruction, TableId::RailwayConstruction] {
            let statuses = dashboard_query::cost::storage(|| replay_lanes::lanes::statuses(recording, stretch.run, table, stretch.first, stretch.last))?;
            if statuses.iter().any(|held| replay_recording::columnar_frames::available(held.status)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn missing(query: &Query<'_>, from: i32, to: i32) -> Vec<MissingSpan> {
    query
        .timeline
        .unheld_within(from, to)
        .into_iter()
        .map(|gap| MissingSpan { first_hour: gap.first.max(from), last_hour: gap.last.min(to), reason: gap.reason })
        .collect()
}

pub fn spellings(query: &Query<'_>, run: u32, hour: i32, ids: &BTreeSet<u32>, into: &mut BTreeMap<u32, Option<String>>) -> io::Result<()> {
    let wanted: Vec<u128> = ids.iter().filter(|id| into.get(id).is_none_or(Option::is_none)).map(|&id| u128::from(id)).collect();
    for &id in ids {
        into.entry(id).or_insert(None);
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let held = read::at(query.replay.recording(), run, &[read::Read { table: TableId::LexicalTokens, columns: None, keys: read::Keys::These(&wanted) }], hour)?;
    let view = held[0].view();
    for &(key, slot) in &held[0].rows {
        let bytes = view.list(0, slot).or_else(|| read::text(&view, 0, slot));
        if let Some(text) = bytes.and_then(|bytes| std::str::from_utf8(bytes).ok()) {
            into.insert(key as u32, Some(text.to_owned()));
        }
    }
    Ok(())
}

pub struct Content {
    catalogue: Result<Arc<Catalogue>, String>,
}

pub struct Named {
    pub types: Vec<ConstructionType>,
    pub repair_icon: Option<String>,
    pub states: Vec<ConstructionStateName>,
}

impl Content {
    pub fn load(source: &Source, context: &LoadContext) -> Self {
        Self { catalogue: context.content().and_then(|options| catalogue::catalogue(source, options)).map_err(|why| why.to_string()) }
    }

    pub(crate) fn catalogue(&self) -> Result<&Arc<Catalogue>, &str> {
        self.catalogue.as_ref().map_err(String::as_str)
    }

    pub fn name(&self, source: &Source, types: &BTreeMap<u32, Option<String>>, states: &BTreeSet<u32>, warnings: &mut Vec<String>) -> Named {
        let catalogue = match &self.catalogue {
            Ok(catalogue) => catalogue,
            Err(why) => {
                warnings.push(format!("Building and state names are unavailable: {why}"));
                return Named {
                    types: types.iter().map(|(&id, token)| ConstructionType { id, token: token.clone(), name: None, icon: None }).collect(),
                    repair_icon: None,
                    states: states.iter().map(|&id| ConstructionStateName { id, name: None }).collect(),
                };
            }
        };
        let naming = naming(source, catalogue);
        let mut naming = naming.lock().unwrap_or_else(PoisonError::into_inner);
        naming.named(catalogue, types, states, warnings)
    }
}

#[derive(Default)]
struct NamingCache(Option<(Arc<Catalogue>, Arc<Mutex<Naming>>)>);

fn naming(source: &Source, catalogue: &Arc<Catalogue>) -> Arc<Mutex<Naming>> {
    let state = source.state::<NamingCache>();
    let mut held = state.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((from, naming)) = &held.0
        && Arc::ptr_eq(from, catalogue)
    {
        return Arc::clone(naming);
    }
    let naming = Arc::new(Mutex::new(Naming::read(catalogue)));
    held.0 = Some((Arc::clone(catalogue), Arc::clone(&naming)));
    naming
}

pub(crate) fn prepare_naming(
    source: &Arc<Source>, load: &LoadContext,
) -> io::Result<Option<Arc<dashboard_base::products::read_ahead::Preparation>>> {
    dashboard_base::products::artwork::prepare_artwork_for::<Naming>(source, load, |source, catalogue| {
        catalogue::prepare_sprite_renderer(catalogue);
        drop(naming(source, catalogue));
    })
}

struct LineSprite {
    sprite: content_reader_gfx::gfx::SpriteType,
    frame: i32,
}

fn line_sprite(catalogue: &Catalogue, element: &str) -> io::Result<LineSprite> {
    let entry = catalogue
        .guis()?
        .template(LINE_ENTRY)
        .filter(|entry| entry.kind == native::Kind::Container)
        .ok_or_else(|| dashboard_core::invalid(format!("no .gui file declares {LINE_ENTRY}")))?;
    let icon = entry.find(native::Kind::Icon, element).ok_or_else(|| dashboard_core::invalid(format!("{LINE_ENTRY} declares no {element} icon")))?;
    if let Some(refusal) = entry.refusals().iter().chain(icon.refusals()).next() {
        return Err(dashboard_core::invalid(format!("{LINE_ENTRY}: {refusal}")));
    }
    let icon = icon.icon().ok_or_else(|| dashboard_core::invalid(format!("{LINE_ENTRY}.{element} is not an icon")))?;
    let name = std::str::from_utf8(&icon.sprite).map_err(|_| dashboard_core::invalid(format!("{LINE_ENTRY}.{element} sprite is not UTF-8")))?;
    if name.is_empty() {
        return Err(dashboard_core::invalid(format!("{LINE_ENTRY}.{element} has no sprite")));
    }
    let sprite = catalogue::created_gui_sprite(&catalogue.sprites, name)?;
    Ok(LineSprite { sprite: sprite.clone(), frame: icon.frame })
}

#[derive(Clone, Copy)]
enum Listing {
    Undictionaried,
    Undeclared,
    Unnamed,
    Frameless,
    Unusable,
    Ambiguous,
    Unreadable,
}

const LISTINGS: [&str; 7] = [
    "building types are not in the recorded lexical dictionary; their titles are unavailable",
    "building types are not declared in the recorded content's buildings; their icons are unavailable",
    "building types have no name in the recorded localisation; their titles are unavailable",
    "building types have icon_frame 0 (none declared), whose picture is not verified, so they show a placeholder",
    "building types give a negative icon_frame; it is refused and they show a placeholder",
    "building types are declared more than once with differing fields; which declaration the game shows is not verified, so they show a placeholder",
    "building icons could not be read from the queue's building strip and show a placeholder",
];

const LISTED: usize = 12;

fn counted(warnings: &mut Vec<String>, what: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let shown = names.iter().take(LISTED).cloned().collect::<Vec<_>>().join(", ");
    let more = names.len().saturating_sub(LISTED);
    let rest = if more > 0 { format!(" and {more} more") } else { String::new() };
    warnings.push(format!("{} {what}: {shown}{rest}.", names.len()));
}

struct NamedType {
    name: Option<String>,
    icon: Option<String>,
    listed: Vec<(Listing, String)>,
    notes: Vec<String>,
    fallbacks: Diagnostics,
}

struct Naming {
    buildings: Result<content_reader_key_names::buildings::Buildings, String>,
    strip: Result<LineSprite, String>,
    repair_icon: Result<String, String>,
    fallbacks: Diagnostics,
    types: HashMap<String, NamedType>,
}

impl Naming {
    fn read(catalogue: &Catalogue) -> Self {
        let layers = &catalogue.content.layers;
        let ((buildings, strip, repair_icon), fallbacks) = content_reader_layers::content::diagnostics::observe(|| {
            let buildings = content_reader_key_names::buildings::read(layers).map_err(|error| format!("Building types are unavailable in the recorded content: {error}"));
            let strip = line_sprite(catalogue, LINE_PICTURE).map_err(|error| format!("Building icons are unavailable: {error}"));
            let repair_icon = line_sprite(catalogue, LINE_REPAIR)
                .and_then(|line| {
                    dashboard_base::products::artwork::resolve_sprite_type(
                        catalogue,
                        &line.sprite,
                        catalogue::authored_frame(line.frame),
                        ICON_EDGE,
                    )
                    .into_io_result()?
                    .ok_or_else(|| dashboard_core::invalid("repair marker artwork unavailable"))
                })
                .map_err(|error| format!("The repair marker is unavailable, so repairs are marked in words only: {LINE_ENTRY}.{LINE_REPAIR}: {error}"));
            (buildings, strip, repair_icon)
        });
        Self { buildings, strip, repair_icon, fallbacks, types: HashMap::new() }
    }

    fn named(&mut self, catalogue: &Catalogue, types: &BTreeMap<u32, Option<String>>, states: &BTreeSet<u32>, warnings: &mut Vec<String>) -> Named {
        self.fallbacks.credit();
        warnings.extend(self.buildings.as_ref().err().cloned());
        warnings.extend(self.strip.as_ref().err().cloned());
        let mut listed: [Vec<String>; 7] = Default::default();
        let mut out = Vec::with_capacity(types.len());
        for (&id, token) in types {
            let Some(token) = token else {
                listed[Listing::Undictionaried as usize].push(id.to_string());
                out.push(ConstructionType { id, token: None, name: None, icon: None });
                continue;
            };
            if !self.types.contains_key(token) {
                let named = self.name_type(catalogue, token);
                self.types.insert(token.clone(), named);
            }
            let named = &self.types[token];
            named.fallbacks.credit();
            for (listing, entry) in &named.listed {
                listed[*listing as usize].push(entry.clone());
            }
            warnings.extend(named.notes.iter().cloned());
            out.push(ConstructionType { id, token: Some(token.clone()), name: named.name.clone(), icon: named.icon.clone() });
        }
        for (what, names) in LISTINGS.iter().zip(&listed) {
            counted(warnings, what, names);
        }
        let repair_icon = match &self.repair_icon {
            Ok(icon) => Some(icon.clone()),
            Err(why) => {
                warnings.push(why.clone());
                None
            }
        };
        let state_content = catalogue.cached_states();
        let mut unnamed = Vec::new();
        let mut contextual_states = Vec::new();
        let states = states
            .iter()
            .map(|&id| {
                let name = match state_content.as_ref().ok().and_then(|states| states.names.get(&id)) {
                    Some(Ok(key)) => match catalogue.localised_or_why(key, View::Scripted) {
                        Ok(name) => Some(name),
                        Err(why) => {
                            if catalogue.localisation.get(key).is_none() {
                                unnamed.push(format!("{id} ({key})"));
                            } else {
                                contextual_states.push(format!("State {id}: {why}"));
                            }
                            None
                        }
                    },
                    Some(Err(_)) | None => {
                        if state_content.is_ok() {
                            unnamed.push(id.to_string());
                        }
                        None
                    }
                };
                ConstructionStateName { id, name }
            })
            .collect();
        match state_content {
            Ok(states) => warnings.extend(dashboard_text::scripted_text::live_state_warnings(&states.warnings).map(|why| format!("State names: {why}"))),
            Err(why) => warnings.push(format!("State names are unavailable: {why}")),
        }
        warnings.extend(contextual_states);
        counted(warnings, "states have no name in the recorded content; their titles are unavailable", &unnamed);
        if out.iter().any(|named| named.icon.is_some()) {
            warnings.push(dashboard_base::products::artwork::CURRENT_INSTALL_ART_NOTICE.to_owned());
        }
        Named { types: out, repair_icon, states }
    }

    fn name_type(&self, catalogue: &Catalogue, token: &str) -> NamedType {
        let mut listed = Vec::new();
        let mut notes = Vec::new();
        let ((name, icon), fallbacks) = content_reader_layers::content::diagnostics::observe(|| {
            let declaration = self.buildings.as_ref().ok().and_then(|buildings| match buildings.by_token(token) {
                Ok(Some(building)) => Some(building),
                Ok(None) => {
                    listed.push((Listing::Undeclared, token.to_owned()));
                    None
                }
                Err(_) => {
                    listed.push((Listing::Ambiguous, token.to_owned()));
                    None
                }
            });
            let key = declaration.map_or(token, |building| building.name.as_str());
            let name = match catalogue.localised_or_why(key, View::Scripted) {
                Ok(name) => Some(name),
                Err(why) => {
                    if catalogue.localisation.get(key).is_none() {
                        listed.push((Listing::Unnamed, format!("{token} ({key})")));
                    } else {
                        notes.push(format!("building {token}: {why}"));
                    }
                    None
                }
            };
            let frame = declaration.and_then(|building| match building.icon_frame {
                frame if frame < 0 => {
                    listed.push((Listing::Unusable, token.to_owned()));
                    None
                }
                0 => {
                    listed.push((Listing::Frameless, token.to_owned()));
                    None
                }
                frame => u32::try_from(frame).ok(),
            });
            let icon = match (&self.strip, frame) {
                (Ok(strip), Some(frame)) => {
                    let drawn = dashboard_base::products::artwork::resolve_sprite_type(
                        catalogue,
                        &strip.sprite,
                        frame - 1,
                        ICON_EDGE,
                    )
                    .into_result();
                    match drawn {
                        Ok(Some(icon)) => Some(icon),
                        Ok(None) => None,
                        Err(error) => {
                            listed.push((Listing::Unreadable, format!("{token} (frame {frame}: {error})")));
                            None
                        }
                    }
                }
                _ => None,
            };
            (name, icon)
        });
        NamedType { name, icon, listed, notes, fallbacks }
    }
}

enum RunKey {}

type Runs = dashboard_base::queries::RunCache<RunKey, RunLines>;

pub(crate) fn forget(source: &dashboard_base::Source) {
    Runs::forget(source);
    tail::forget(source);
}

pub fn cached_construction(source: &dashboard_base::Source, run: u32) -> io::Result<std::sync::Arc<RunLines>> {
    Runs::held(&source.state::<Runs>(), run, || run_lines(source, run))
}
