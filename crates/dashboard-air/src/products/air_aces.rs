
use dashboard_base::products::LoadContext;
use dashboard_base::{Source, catalogue::{self, Catalogue, Refusals, View}, products::at_hour::{At, AtHour, Keys, Owner, Read}, queries::country_ids::{self, RunLinks}};
use dashboard_query::{Index, read};
use dashboard_text::{air_wing_names, variant_names};
use content_reader_localisation::localisation::Argument;
use replay::campaign::Replay;
use replay_columnar::summary;
use replay_data::rows::{AirAceRow, TableId, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::Entry},
    io,
    sync::{Arc, Mutex, PoisonError},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[cfg(test)]
mod tests;

const TABLES: &[TableId] = &[
    TableId::AirAces,
    TableId::CountryLinks,
    TableId::Countries,
    TableId::CountryAppearance,
    TableId::AirWings,
    TableId::WingEquipment,
    TableId::VariantDefinitions,
    TableId::LexicalTokens,
];

pub struct AirAcesAt;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AirAcesParams {
    pub branch_id: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AirAceDeath {
    #[doc = " The last read that saw it alive; null when it was first seen dead."]
    pub last_alive: Option<i32>,
    #[doc = " The first read that saw it dead after one that saw it alive."]
    pub by_hour: Option<i32>,
    #[doc = " The first read that saw it dead: the hour its killer's country is named at."]
    pub seen_dead: Option<i32>,
    pub killer_ace: Option<String>,
    #[doc = " The killer's name when the killer is on the roster at the hour."]
    pub killer_name: Option<String>,
    #[doc = " The killer's country tag at `seen_dead`; none when that hour is not known."]
    pub killer_tag: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AirAceAircraft {
    pub id: String,
    pub name: Option<String>,
    #[doc = " Why the name is not shown, without one."]
    pub missing: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AirAce {
    pub id: String,
    pub tag: Option<String>,
    pub name: Option<String>,
    pub callsign: Option<String>,
    #[doc = " The assigned wing's handle; null without one."]
    pub wing: Option<String>,
    pub wing_name: Option<String>,
    #[doc = " The wing's aircraft in its equipment order; null when not recorded."]
    pub aircraft: Option<Vec<AirAceAircraft>>,
    pub alive: bool,
    #[doc = " The first hour the timeline held its row: an upper bound of when it became an ace."]
    pub first_seen: Option<i32>,
    pub death: Option<AirAceDeath>,
    #[doc = " The ace's portrait through the recorded content stack; none with `portrait_missing` saying why."]
    pub portrait: Option<String>,
    pub portrait_missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AirAcesReply {
    #[doc = " The hour of the read the roster holds; none when not known."]
    pub read_hour: Option<i32>,
    #[doc = " Why the roster is not shown at the hour, when it is not."]
    pub missing: Option<String>,
    pub aces: Vec<AirAce>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(AirAcesReply);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Death {
    pub from: i32,
    pub last_alive: Option<i32>,
    pub dead_by: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Dates {
    pub first_seen: Option<i32>,
    pub deaths: Vec<Death>,
}

impl Dates {
    fn death_at(&self, hour: i32) -> Option<Death> {
        self.deaths.iter().rev().find(|death| death.from <= hour).copied()
    }
}

#[derive(Default)]
pub(crate) struct Fold {
    pub dates: Dates,
    last_alive: Option<i32>,
    dead: bool,
}

impl Fold {
    pub(crate) fn run(&mut self, first: i32, alive: Option<i64>, last_read: Option<i32>) {
        let Some(alive) = alive else { return };
        self.dates.first_seen.get_or_insert(first);
        if alive != 0 {
            if last_read.is_some() {
                self.last_alive = last_read;
            }
            self.dead = false;
        } else if !self.dead {
            self.dead = true;
            self.dates.deaths.push(Death { from: first, last_alive: self.last_alive, dead_by: self.last_alive.map(|_| first) });
        }
    }
}

pub(crate) fn last_read(reads: &[(i32, i32)], first: i32, last: i32) -> Option<i32> {
    let at = reads.partition_point(|run| run.0 <= last).checked_sub(1)?;
    let hour = reads[at].1.min(last);
    (hour >= first).then_some(hour)
}

type Slots = HashMap<u32, Vec<(String, i32, i32)>>;

pub struct History {
    aces: HashMap<u64, Dates>,
    slots: Slots,
    links: Vec<(i32, i32, Arc<RunLinks>)>,
}

impl History {
    fn tag(&self, raw: u32, hour: i32) -> Option<String> {
        let (.., links) = self.links.iter().find(|(first, last, _)| *first <= hour && hour <= *last)?;
        let slot = links.slot_at(raw, hour)?;
        self.slots.get(&slot)?.iter().find(|run| run.1 <= hour && hour <= run.2).map(|run| run.0.clone())
    }
}

fn build(query: &dashboard_query::Query<'_>) -> io::Result<History> {
    let recording = query.replay.recording();
    let alive = dashboard_declare::field(TableId::AirAces, "alive")?;
    let mut folds: HashMap<u64, Fold> = HashMap::new();
    let mut held: HashMap<u32, Arc<RunLinks>> = HashMap::new();
    let mut links = Vec::with_capacity(query.timeline.stretches.len());
    for stretch in &query.timeline.stretches {
        let run_links = match held.entry(stretch.run) {
            Entry::Occupied(found) => Arc::clone(found.get()),
            Entry::Vacant(slot) => Arc::clone(slot.insert(Arc::new(country_ids::run_links(recording, stretch.run)?))),
        };
        links.push((stretch.first, stretch.last, run_links));
        let runs = read::runs(recording, stretch.run, TableId::AirAces, summary::Source::Cell(alive), None, stretch.first, stretch.last)?;
        let reads = dashboard_query::cost::storage(|| replay_lanes::lanes::reads(recording, stretch.run, TableId::AirAces, stretch.first, stretch.last))?;
        for (key, runs) in runs {
            let fold = folds.entry(key as u64).or_default();
            for &(first, last, value) in &runs {
                fold.run(first, value, last_read(&reads, first, last));
            }
        }
    }
    let slots: Slots = super::air_ledger::slot_holders(query).into_iter().map(|(slot, runs)| (slot, runs.into_iter().map(|(tag, _, first, last)| (tag, first, last)).collect())).collect();
    Ok(History { aces: folds.into_iter().map(|(id, fold)| (id, fold.dates)).collect(), slots, links })
}

#[derive(Default)]
struct Histories(HashMap<u64, Arc<History>>);

fn held_history(held: &Mutex<Histories>, replay: &Replay, index: &Index, branch: u64) -> io::Result<Arc<History>> {
    if let Some(found) = held.lock().unwrap_or_else(PoisonError::into_inner).0.get(&branch) {
        return Ok(Arc::clone(found));
    }
    let made = Arc::new(build(&index.query(replay, branch)?)?);
    held.lock().unwrap_or_else(PoisonError::into_inner).0.insert(branch, Arc::clone(&made));
    Ok(made)
}

pub fn history(source: &Source, branch: u64) -> io::Result<Arc<History>> {
    held_history(&source.state::<Histories>(), source.replay_ref(), &dashboard_base::queries::index(source), branch)
}

pub struct Context {
    cat: Result<Arc<Catalogue>, String>,
    replay: Arc<Replay>,
    index: Arc<Index>,
    histories: Arc<Mutex<Histories>>,
    portraits: Arc<Mutex<PortraitCache>>,
}

impl Context {
    fn of(source: &Source, load: &LoadContext) -> Self {
        let cat = load
            .content()
            .and_then(|options| catalogue::catalogue(source, options))
            .map_err(|error| format!("Recorded content stack unavailable; choose matching game and user folders ({error})"));
        Self { cat, replay: source.replay(), index: dashboard_base::queries::index(source), histories: source.state::<Histories>(), portraits: source.state::<PortraitCache>() }
    }

    fn history(&self, branch: &str) -> io::Result<Arc<History>> {
        let branch = branch.parse().map_err(|_| dashboard_core::invalid("Invalid branch"))?;
        held_history(&self.histories, &self.replay, &self.index, branch)
    }

    fn warnings(&self, warnings: &mut Vec<String>) {
        match &self.cat {
            Err(why) => warnings.push(why.clone()),
            Ok(cat) => {
                warnings.extend(cat.verification.iter().cloned());
                warnings.extend(cat.warnings.iter().cloned());
            }
        }
    }
}

fn text(bytes: &[u8], identity: &str, refusals: &mut Refusals) -> Option<String> {
    catalogue::display_text(bytes).map_err(|reason| refusals.note(reason, identity)).ok()
}

fn ace_name(row: &AirAceRow, cat: Option<&Catalogue>, refusals: &mut Refusals) -> Option<String> {
    let identity = row.id.to_string();
    let Some(cat) = cat else {
        refusals.note("ACE_FULL_NAME content unavailable; name order unknown", identity);
        return None;
    };
    if cat.localisation.get("ACE_FULL_NAME").is_none() {
        refusals.note("ACE_FULL_NAME absent from recorded content", identity);
        return None;
    }
    let first = text(&row.name, &identity, refusals)?;
    let surname = text(&row.surname, &identity, refusals)?;
    cat.localised_with("ACE_FULL_NAME", &[("NAME", Argument::Text(&first)), ("SUR", Argument::Text(&surname))], View::Plain)
}

fn assigned(rows: &[AirAceRow]) -> BTreeSet<u64> {
    rows.iter().map(|row| row.assigned_wing).filter(|wing| *wing != 0).collect()
}

fn wing_variants(frame: &DecodedFrame, wings: &BTreeSet<u64>) -> HashMap<u64, Option<Vec<u64>>> {
    let held: Option<HashSet<u64>> = frame.complete::<tables::AirWings>().ok().map(|rows| rows.iter().map(|row| row.id).collect());
    let equipment = frame.complete::<tables::WingEquipment>().ok();
    let mut by_wing: HashMap<u64, Vec<(u32, u64)>> = HashMap::new();
    for row in equipment.into_iter().flatten().filter(|row| wings.contains(&row.wing)) {
        by_wing.entry(row.wing).or_default().push((row.ordinal, row.variant));
    }
    wings
        .iter()
        .map(|&wing| {
            let known = equipment.is_some() && held.as_ref().is_some_and(|held| held.contains(&wing));
            let variants = known.then(|| {
                let mut rows = by_wing.remove(&wing).unwrap_or_default();
                rows.sort_unstable();
                rows.into_iter().map(|(_, variant)| variant).collect()
            });
            (wing, variants)
        })
        .collect()
}

fn variant_names(frame: &DecodedFrame, variants: &BTreeSet<u64>, cat: Option<&Catalogue>) -> HashMap<u64, Result<String, String>> {
    let tables = (|| -> io::Result<_> {
        let countries = frame.complete::<tables::Countries>()?;
        let links = frame.complete::<tables::CountryLinks>()?;
        let definitions = frame.complete::<tables::VariantDefinitions>()?;
        let lexical = frame.complete::<tables::LexicalTokens>()?;
        Ok((countries, links, definitions, lexical))
    })();
    let (countries, links, definitions, lexical) = match tables {
        Ok(tables) => tables,
        Err(error) => return variants.iter().map(|&id| (id, Err(format!("Aircraft names are not recorded at this hour: {error}")))).collect(),
    };
    let appearance = frame.complete::<tables::CountryAppearance>().ok();
    let tags = variant_names::Tags::new(countries, links, appearance);
    let texts = cat.map(|cat| cat as &dyn variant_names::Texts);
    variants
        .iter()
        .map(|&id| {
            let name = match definitions.iter().find(|row| row.id == id) {
                None => Err(format!("variant definitions lacks aircraft {id}")),
                Some(definition) => {
                    let raw = variant_names::raw_variant(definition, frame.hour, &tags, lexical);
                    variant_names::variant_names(&raw, texts).0.ok_or_else(|| format!("Aircraft {id} name unsupported"))
                }
            };
            (id, name)
        })
        .collect()
}

pub(crate) fn roster(frame: &DecodedFrame, history: &History, cat: Option<&Catalogue>, warnings: &mut Vec<String>) -> (Option<String>, Vec<AirAce>) {
    let rows = match frame.complete::<tables::AirAces>() {
        Ok(rows) => rows,
        Err(error) => return (Some(format!("Aces are not recorded at this hour: {error}")), Vec::new()),
    };
    let hour = frame.hour;
    let mut refusals = Refusals::default();
    let mut wing_refusals = Refusals::default();
    let wings = assigned(rows);
    let variants = wing_variants(frame, &wings);
    let wanted: BTreeSet<u64> = variants.values().flatten().flatten().copied().collect();
    let names = if wanted.is_empty() { HashMap::new() } else { variant_names(frame, &wanted, cat) };
    let requested: HashSet<u64> = wings.iter().copied().collect();
    let wing_names = air_wing_names::recorded_names(frame, &requested, &mut wing_refusals);
    let names_by_id: HashMap<u64, Option<String>> = rows.iter().map(|row| (row.id, ace_name(row, cat, &mut refusals))).collect();
    let mut aces = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.id.to_string();
        let tag = history.tag(row.country, hour);
        if tag.is_none() {
            refusals.note("Ace owner country link has no resolved recorded tag", row.country.to_string());
        }
        let dates = history.aces.get(&row.id);
        let alive = row.alive != 0;
        let death = (!alive).then(|| {
            let death = dates.and_then(|dates| dates.death_at(hour));
            let killer = row.death.as_ref();
            AirAceDeath {
                last_alive: death.and_then(|death| death.last_alive),
                by_hour: death.and_then(|death| death.dead_by),
                killer_ace: killer.map(|death| death.killer_ace).filter(|id| *id != 0).map(|id| id.to_string()),
                killer_name: killer.and_then(|death| names_by_id.get(&death.killer_ace).cloned().flatten()),
                seen_dead: death.map(|death| death.from),
                killer_tag: killer.zip(death).and_then(|(killed, death)| history.tag(killed.killer_country, death.from)),
            }
        });
        let wing_name = wing_names.get(&row.assigned_wing).and_then(|bytes| match cat {
            Some(cat) => dashboard_text::scripted_text::stored_text(&cat.localisation, &cat.recorded_language, bytes, "Air wing name")
                .and_then(|name| catalogue::plain_text(&name))
                .map_err(|why| wing_refusals.note(why, row.assigned_wing.to_string()))
                .ok(),
            None => text(bytes, &row.assigned_wing.to_string(), &mut wing_refusals),
        });
        let aircraft = if row.assigned_wing == 0 {
            Some(Vec::new())
        } else {
            variants.get(&row.assigned_wing).cloned().flatten().map(|ids| {
                ids.into_iter()
                    .map(|variant| {
                        let (name, missing) = match names.get(&variant) {
                            Some(Ok(name)) => (Some(name.clone()), None),
                            Some(Err(why)) => (None, Some(why.clone())),
                            None => (None, Some(format!("Aircraft {variant} name not read"))),
                        };
                        AirAceAircraft { id: variant.to_string(), name, missing }
                    })
                    .collect()
            })
        };
        aces.push(AirAce {
            name: names_by_id.get(&row.id).cloned().flatten(),
            callsign: text(&row.callsign, &id, &mut refusals),
            id,
            tag,
            wing: (row.assigned_wing != 0).then(|| row.assigned_wing.to_string()),
            wing_name,
            aircraft,
            alive,
            first_seen: dates.and_then(|dates| dates.first_seen),
            death,
            portrait: None,
            portrait_missing: None,
        });
    }
    warnings.extend(refusals.counts("ace fields/reads"));
    warnings.extend(wing_refusals.counts("ace wing names"));
    (None, aces)
}

fn wing_reads(held: &DecodedFrame) -> Vec<Read> {
    let Ok(rows) = held.complete::<tables::AirAces>() else { return Vec::new() };
    let wings = assigned(rows);
    if wings.is_empty() {
        return Vec::new();
    }
    vec![
        Read::these(TableId::AirWings, wings.iter().map(|&wing| u128::from(wing))),
        Read { table: TableId::WingEquipment, keys: Keys::Ranges(wings.iter().map(|&wing| (u128::from(wing) << 32, u128::from(wing) << 32 | u128::from(u32::MAX))).collect()) },
    ]
}

fn variant_reads(held: &DecodedFrame) -> Vec<Read> {
    let Ok(equipment) = held.complete::<tables::WingEquipment>() else { return Vec::new() };
    let variants: BTreeSet<u64> = equipment.iter().map(|row| row.variant).collect();
    if variants.is_empty() {
        return Vec::new();
    }
    vec![
        Read::these(TableId::VariantDefinitions, variants.into_iter().map(u128::from)),
        Read::all(TableId::Countries),
        Read::all(TableId::CountryAppearance),
    ]
}

fn token_reads(held: &DecodedFrame) -> Vec<Read> {
    let Ok(definitions) = held.complete::<tables::VariantDefinitions>() else { return Vec::new() };
    let tokens: BTreeSet<u32> = definitions.iter().map(|row| row.equipment_token).collect();
    vec![Read::these(TableId::LexicalTokens, tokens.into_iter().map(u128::from))]
}

impl AtHour for AirAcesAt {
    const KEY: &'static str = "air-aces";
    const TITLE: &'static str = "Aces";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    type Params = AirAcesParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId];
    type Reply = AirAcesReply;
    type Context = Context;

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map(drop).map_err(|_| "Invalid branch")
    }

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![AirAcesParams::decl(), AirAcesReply::decl(), AirAce::decl(), AirAceAircraft::decl(), AirAceDeath::decl()]
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        TABLES.to_vec()
    }

    fn reads(_params: &Self::Params, _owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        match step {
            0 => [TableId::AirAces, TableId::CountryLinks].into_iter().map(Read::all).collect(),
            1 => wing_reads(held),
            2 => variant_reads(held),
            3 => token_reads(held),
            _ => Vec::new(),
        }
    }

    fn valid_reply_through(frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>, reply: &Self::Reply, _partial: bool) -> i32 {
        if reply.missing.is_some() { return frame.hour; }
        if !reply.aces.iter().any(|ace| ace.wing.is_some()) { return i32::MAX; }
        let Ok(wings) = frame.complete::<tables::AirWings>() else { return frame.hour };
        if wings.iter().any(|wing| matches!(wing.name, replay_data::rows::Recorded::NotRecorded)) { return frame.hour; }
        let names_missing = [TableId::Countries, TableId::CountryLinks, TableId::VariantDefinitions, TableId::LexicalTokens]
            .iter().any(|table| !matches!(frame.tables[table.index()].held(), replay_frame::frame::TableView::Complete(_)));
        if names_missing && reply.aces.iter().filter_map(|ace| ace.aircraft.as_deref()).flatten().any(|aircraft| aircraft.missing.is_some()) {
            return frame.hour;
        }
        i32::MAX
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], context: &Context) -> Option<Vec<usize>> {
        let fields: &[&str] = match table {
            TableId::AirAces if context.cat.is_err() => &["country", "callsign", "assigned_wing", "alive", "death"],
            TableId::AirAces => &["country", "name", "surname", "callsign", "assigned_wing", "alive", "death", "presentation"],
            TableId::Countries => &["canonical_slot", "tag"],
            TableId::CountryAppearance => &["name_tag"],
            TableId::AirWings => &["name"],
            TableId::WingEquipment => &["variant"],
            TableId::VariantDefinitions => &["equipment_token", "display_name", "variant_name", "origin_link", "creator_link", "append_variant_name"],
            _ => return None,
        };
        Some(dashboard_base::products::at_hour::field_columns(table, fields))
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        Ok(Context::of(source, load))
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let history = at.context.history(&params.branch_id)?;
        let mut warnings = Vec::new();
        let (missing, mut aces) = roster(frame, &history, at.context.cat.as_deref().ok(), &mut warnings);
        portraits(frame, &mut aces, at, &mut warnings)?;
        at.context.warnings(&mut warnings);
        let reply = AirAcesReply { read_hour: frame.tables[TableId::AirAces.index()].sampled_at(), missing, aces, warnings };
        dashboard_base::products::check_reply_budget(&reply).map_err(|_| dashboard_core::invalid("Ace roster reply exceeds transport budget; no aces were truncated"))?;
        Ok(reply)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PortraitOperands {
    tag: Option<String>,
    name: Vec<u8>,
    surname: Vec<u8>,
    fields: Result<Option<(u8, i32, Option<(Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)>)>, String>,
}

impl PortraitOperands {
    fn of(row: &AirAceRow, tag: Option<String>) -> Self {
        Self {
            tag,
            name: row.name.clone(),
            surname: row.surname.clone(),
            fields: row.presentation.require("Ace presentation").map(|fields| {
                fields.as_ref().map(|fields| (fields.gender, fields.pool_index, fields.portrait.as_ref().map(|pool| (pool.cached.clone(), pool.culture.clone(), pool.fallback.clone()))))
            }),
        }
    }
}

fn portrait(facts: &PortraitOperands, cat: &Catalogue) -> Result<String, String> {
    let tag = facts.tag.as_ref().ok_or("Ace country not recorded")?;
    let name = std::str::from_utf8(&facts.name).map_err(|_| "Ace name is not UTF-8")?;
    let surname = std::str::from_utf8(&facts.surname).map_err(|_| "Ace surname is not UTF-8")?;
    let sprite = match cat.sprites.find(&format!("GFX_{tag}_ace_{name}_{surname}"))? {
        Some(sprite) => sprite,
        None => {
            let (gender, pool_index, pool) = facts.fields.as_ref().map_err(Clone::clone)?.as_ref().ok_or("Ace presentation capture failed")?;
            let (cached, culture, fallback) = pool.as_ref().ok_or("Ace portrait pool capture failed")?;
            let mut culture_pool = Vec::new();
            if cached.is_none() {
                let culture = culture.as_ref().ok_or("Ace portrait culture not recorded")?;
                let culture = std::str::from_utf8(culture).map_err(|_| "Ace portrait culture is not UTF-8")?;
                let gender = if *gender == 0 { "m" } else { "f" };
                for index in 0..256 {
                    let Some(sprite) = cat.sprites.find(&format!("GFX_{culture}_ace_{gender}_{index}"))? else { break };
                    culture_pool.push(sprite);
                }
            }
            if culture_pool.is_empty() {
                let key = cached.as_ref().or(fallback.as_ref()).ok_or("Native ace global portrait unavailable or pool empty")?;
                let key = std::str::from_utf8(key).map_err(|_| "Ace portrait sprite key is not UTF-8")?;
                cat.sprites.find(key)?.ok_or_else(|| format!("Native ace portrait sprite {key} absent from recorded content"))?
            } else {
                let index = pool_index % (culture_pool.len() as i32);
                culture_pool
                    .get(usize::try_from(index).map_err(|_| "Ace portrait index has negative native remainder")?)
                    .copied()
                    .ok_or("Ace portrait index outside native pool")?
            }
        }
    };
    dashboard_base::products::artwork::resolve_sprite_type(cat, sprite, 0, 96)
        .into_result()?
        .ok_or_else(|| "Ace portrait artwork unavailable".to_owned())
}

#[derive(Default)]
struct PortraitCache {
    catalogue: Option<Arc<Catalogue>>,
    entries: HashMap<PortraitOperands, catalogue::Drawn<Result<String, String>>>,
}

fn cached_portrait(cache: &Mutex<PortraitCache>, facts: &PortraitOperands, cat: &Arc<Catalogue>) -> Result<String, String> {
    {
        let mut state = cache.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.catalogue.as_ref().is_some_and(|previous| Arc::ptr_eq(previous, cat)) {
            *state = PortraitCache { catalogue: Some(Arc::clone(cat)), ..Default::default() };
        }
        if let Some(found) = state.entries.get(facts) {
            return found.get().clone();
        }
    }
    let found = catalogue::draw(
        || format!("ace portrait {} {} {}", facts.tag.as_deref().unwrap_or(""), String::from_utf8_lossy(&facts.name), String::from_utf8_lossy(&facts.surname)),
        cat,
        |cat| portrait(facts, cat),
    );
    let mut state = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if state.catalogue.as_ref().is_some_and(|previous| Arc::ptr_eq(previous, cat)) {
        return state.entries.entry(facts.clone()).or_insert(found).get().clone();
    }
    found.get().clone()
}

fn portraits(frame: &DecodedFrame, aces: &mut [AirAce], at: &At<'_, Context>, warnings: &mut Vec<String>) -> io::Result<()> {
    let mut refusals = Refusals::default();
    let rows: BTreeMap<u64, &AirAceRow> = frame.complete::<tables::AirAces>().map(|rows| rows.iter().map(|row| (row.id, row)).collect()).unwrap_or_default();
    for ace in aces {
        dashboard_text::metadata::check_cancel(at.cancelled)?;
        let found = ace
            .id
            .parse::<u64>()
            .ok()
            .and_then(|id| rows.get(&id))
            .ok_or_else(|| "Ace not recorded at this hour".to_owned())
            .and_then(|row| {
                let cat = at.context.cat.as_ref().map_err(Clone::clone)?;
                cached_portrait(&at.context.portraits, &PortraitOperands::of(row, ace.tag.clone()), cat)
            });
        match found {
            Ok(image) => ace.portrait = Some(image),
            Err(reason) => {
                refusals.note(&reason, format!("Ace {}", ace.id));
                ace.portrait_missing = Some(reason);
            }
        }
    }
    warnings.extend(refusals.counts("ace portraits"));
    Ok(())
}
