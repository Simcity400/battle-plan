
use dashboard_base::products::Country;
use super::{
    Art,
    country_goals::{GoalFilter, GoalState},
    goals::{self, FactionGoalDefinition, Texts},
    owned,
};
use dashboard_base::{
    Source,
    catalogue::{self, Catalogue},
    products::{
        LoadContext,
        at_hour::{At, AtHour, DateSelection, Edits, Held, Keys, Owner, Read, ReadsAt, ScriptedInputs, before, field_columns, warnings_through},
    },
};
use dashboard_world::products::territory::slot_tags;
use dashboard_base::products::resources::{Learned, ResourceNaming};
use dashboard_follow::live::{Cells, Source as Cell};
use replay_data::rows::{FactionGoalEntryRow, FactionResourceRow, TableId, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    sync::{Arc, atomic::AtomicBool},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct FactionDetails;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactionDetailParams {
    pub branch_id: String,
    #[doc = " The recorded faction id, as a decimal string."]
    pub faction: String,
    #[doc = " None follows the recording client's playing or observer country."]
    pub viewpoint: Option<Country>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionHeldGoal {
    pub ordinal: u32,
    #[doc = " An index into the reply's `definitions`."]
    pub definition: u32,
    #[doc = " The native state: 0 in progress, 1 completed, 2 cancelled, 3 completing, 4 cancelling."]
    pub status: i32,
    #[doc = " The progress sections the game holds active, by index into the definition's `bands`."]
    pub active_sections: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionResourceDetail {
    pub ordinal: u32,
    pub token: Option<String>,
    pub name: Option<String>,
    pub icon: Option<String>,
    #[doc = " Why its name or icon is unavailable."]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionManpowerDetail {
    pub ordinal: u32,
    #[doc = " The contributing country's tag at the hour; none when it is not known."]
    pub tag: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionDoctrineDetail {
    pub ordinal: u32,
    pub token: Option<String>,
    pub name: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionRefusalDetail {
    pub fact: String,
    pub count: u32,
    pub reason: String,
}

#[derive(Clone, Copy, Debug)]
struct Through(i32);

impl PartialEq for Through {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FactionDetailReply {
    #[serde(skip)]
    #[cfg_attr(any(test, feature = "export-types"), ts(skip))]
    through: Through,
    pub branch_id: String,
    pub faction: String,
    #[doc = " The faction's logo as the game draws it; none when it could not be drawn (a refusal says why)."]
    pub logo: Option<String>,
    #[doc = " Every goal the reply names, once each: the lists below index it."]
    pub definitions: Vec<FactionGoalDefinition>,
    #[doc = " The manifest the faction holds, an index into `definitions`."]
    pub manifest: Option<u32>,
    pub goals: Vec<FactionHeldGoal>,
    pub completed: Vec<u32>,
    pub cancelled: Vec<u32>,
    #[doc = " Why the goal lists are not known at the hour; they are then empty."]
    pub goal_reason: Option<String>,
    #[doc = " None when the pool was not read whole at the hour."]
    pub resources: Option<Vec<FactionResourceDetail>>,
    #[doc = " None when the contributions were not read whole at the hour; the total is the live cell"]
    #[doc = " `faction` · `manpower`."]
    pub manpower: Option<Vec<FactionManpowerDetail>>,
    pub manpower_unlocked: Option<bool>,
    pub research_unlocked: Option<bool>,
    #[doc = " The countries sharing research; none when not read. The scientist base is the live cell"]
    #[doc = " `faction` · `research`."]
    pub research_countries: Option<Vec<String>>,
    pub doctrine_folders: Option<Vec<FactionDoctrineDetail>>,
    pub refusals: Vec<FactionRefusalDetail>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(FactionDetailReply);

pub const FACTION_ROW: &str = "faction";

pub struct DetailContent {
    catalogue: Result<Arc<Catalogue>, String>,
    naming: ResourceNaming,
    art: Arc<Art>,
    inputs: ScriptedInputs,
}

#[derive(Default)]
struct HeldArt(Option<(usize, Arc<Art>)>);

const DETAIL_TABLES: [TableId; 11] = [
    TableId::ResourceDefinitions,
    TableId::CountryLinks,
    TableId::ReservedCountryNaming,
    TableId::FactionGoalStatus,
    TableId::FactionUnlocks,
    TableId::FactionProjection,
    TableId::FactionManpower,
    TableId::FactionResearch,
    TableId::FactionDoctrine,
    TableId::FactionResources,
    TableId::FactionGoalEntries,
];

fn detail_tables(faction: u64) -> Vec<Read> {
    let (low, high) = super::ordinals(faction);
    let goals = u128::from(faction) << 40;
    let mut reads: Vec<Read> = [TableId::ResourceDefinitions, TableId::CountryLinks, TableId::ReservedCountryNaming].into_iter().map(Read::all).collect();
    reads.extend([TableId::FactionGoalStatus, TableId::FactionUnlocks, TableId::FactionProjection].map(|table| Read::these(table, [u128::from(faction)])));
    reads.extend(
        [TableId::FactionManpower, TableId::FactionResearch, TableId::FactionDoctrine, TableId::FactionResources]
            .map(|table| Read { table, keys: Keys::Range(low, high) }),
    );
    reads.push(Read { table: TableId::FactionGoalEntries, keys: Keys::Range(goals, goals | ((1u128 << 40) - 1)) });
    reads
}

fn empty_detail_tables() -> Vec<Read> {
    DETAIL_TABLES.into_iter().map(|table| Read::these(table, [])).collect()
}

fn faction_absent(frame: &DecodedFrame) -> bool {
    frame.complete::<tables::Factions>().is_ok_and(|rows| rows.is_empty())
}

fn detail_tokens(frame: &DecodedFrame, faction: u64) -> Vec<u128> {
    let mut tokens = BTreeSet::new();
    for row in frame.complete::<tables::Factions>().ok().into_iter().flatten() {
        if row.id == faction && let Some(token) = row.template_token {
            tokens.insert(u128::from(token));
        }
    }
    for row in frame.complete::<tables::FactionGoalStatus>().ok().into_iter().flat_map(|rows| owned(rows, faction, |row| row.faction)) {
        if let Some(token) = row.manifest {
            tokens.insert(u128::from(token));
        }
    }
    for row in frame.complete::<tables::FactionGoalEntries>().ok().into_iter().flat_map(|rows| owned(rows, faction, |row| row.faction)) {
        if row.kind <= 2 && let Some(token) = row.token {
            tokens.insert(u128::from(token));
        }
    }
    for row in frame.complete::<tables::FactionDoctrine>().ok().into_iter().flat_map(|rows| owned(rows, faction, |row| row.faction)) {
        tokens.insert(u128::from(row.token));
    }
    for row in frame.complete::<tables::ResourceDefinitions>().ok().into_iter().flatten() {
        tokens.insert(u128::from(row.token));
    }
    tokens.into_iter().collect()
}

fn scripted_tables() -> Vec<TableId> {
    let mut tables: Vec<TableId> = goals::text_tables().chain([TableId::LocalPlayer, TableId::Countries, TableId::CountryLinks]).collect();
    tables.sort_unstable();
    tables.dedup();
    tables
}

fn faction_reads(faction: u64) -> Vec<Read> {
    let mut reads = vec![Read::all(TableId::Factions), Read::all(TableId::LexicalTokens), Read::all(TableId::LocalPlayer)];
    reads.extend(detail_tables(faction));
    reads
}

pub(super) fn country_goal_reads(faction: u64) -> Vec<Read> {
    let goals = u128::from(faction) << 40;
    vec![
        Read::these(TableId::FactionGoalStatus, [u128::from(faction)]),
        Read { table: TableId::FactionGoalEntries, keys: Keys::Range(goals, goals | ((1u128 << 40) - 1)) },
    ]
}

impl AtHour for FactionDetails {
    const KEY: &'static str = "faction-details";
    const TITLE: &'static str = "Faction details";
    const CONTENT_DEPENDENT: bool = true;
    type Params = FactionDetailParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, faction, viewpoint];
    type Reply = FactionDetailReply;
    type Context = DetailContent;

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid faction branch")?;
        super::faction_id(&params.faction).map(|_| ())
    }

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            FactionDetailParams::decl(),
            goals::FactionProgressBand::decl(),
            FactionGoalDefinition::decl(),
            FactionHeldGoal::decl(),
            FactionResourceDetail::decl(),
            FactionManpowerDetail::decl(),
            FactionDoctrineDetail::decl(),
            FactionRefusalDetail::decl(),
            FactionDetailReply::decl(),
        ]
    }

    fn tables(params: &Self::Params) -> Vec<TableId> {
        let faction = super::faction_id(&params.faction).unwrap_or(0);
        let mut tables: Vec<TableId> = faction_reads(faction).into_iter().map(|read| read.table).chain(goals::text_tables()).collect();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    fn reads(params: &Self::Params, _owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let Ok(faction) = super::faction_id(&params.faction) else { return Vec::new() };
        match step {
            0 => vec![Read::these(TableId::Factions, [u128::from(faction)])],
            1 => if faction_absent(held) { empty_detail_tables() } else { detail_tables(faction) },
            2 => vec![Read { table: TableId::LexicalTokens, keys: Keys::These(if faction_absent(held) { Vec::new() } else { detail_tokens(held, faction) }) }],
            _ => Vec::new(),
        }
    }

    fn reads_in(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        if step == 3 {
            let (Ok(faction), Ok(cat)) = (super::faction_id(&params.faction), at.context.catalogue()) else { return Vec::new() };
            if !faction_absent(held) && goals::needs_lexicon_lookup_all(held, faction, cat) {
                return vec![Read::all(TableId::LexicalTokens)];
            }
            return Vec::new();
        }
        Self::reads(params, owner, step, held)
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        match table {
            TableId::Factions => Some(field_columns(table, &["template_token"])),
            TableId::FactionGoalStatus => Some(field_columns(table, &["manifest"])),
            TableId::FactionGoalEntries => Some(field_columns(table, &["token", "status", "active_sections", "progress_raw"])),
            TableId::FactionUnlocks => Some(field_columns(table, &["research", "manpower"])),
            TableId::FactionProjection => Some(field_columns(table, &["projection_raw"])),
            TableId::FactionManpower => Some(field_columns(table, &["country", "value"])),
            TableId::FactionResearch => Some(field_columns(table, &["countries", "scientist_base_raw"])),
            TableId::FactionDoctrine => Some(field_columns(table, &["token"])),
            TableId::FactionResources => Some(field_columns(table, &["token", "raw"])),
            TableId::ResourceDefinitions => Some(field_columns(table, &["token"])),
            TableId::CountryLinks => Some(field_columns(table, &["canonical_slot"])),
            TableId::ReservedCountryNaming => Some(field_columns(table, &["tag"])),
            TableId::LexicalTokens => Some(field_columns(table, &["literal"])),
            _ => None,
        }
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> {
        match table {
            TableId::FactionGoalEntries => Some(field_columns(table, &["token", "status", "active_sections"])),
            TableId::FactionProjection => Some(Vec::new()),
            TableId::FactionManpower => Some(field_columns(table, &["country"])),
            TableId::FactionResearch => Some(field_columns(table, &["countries"])),
            _ => Self::columns(table),
        }
    }

    fn compares_previous(table: TableId) -> bool {
        table == TableId::FactionResources
    }

    fn plan_changes(stage: usize, edits: &Edits, _held: &DecodedFrame, _at: &ReadsAt<'_, Self::Context>) -> bool {
        let touched = |tables: &[TableId]| edits.replaced.iter().chain(edits.tables.keys()).any(|table| tables.contains(table));
        const TOKEN_SOURCES: &[TableId] = &[
            TableId::Factions,
            TableId::FactionGoalStatus,
            TableId::FactionGoalEntries,
            TableId::FactionDoctrine,
            TableId::ResourceDefinitions,
        ];
        match stage {
            0 => false,
            1 => touched(&[TableId::Factions]),
            2 => touched(TOKEN_SOURCES),
            _ => touched(TOKEN_SOURCES) || touched(&[TableId::LexicalTokens]),
        }
    }

    fn holds_refusal(frame: &DecodedFrame) -> bool {
        faction_absent(frame)
    }

    fn advance(frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>, edits: &Edits, _live: &dashboard_follow::live::Live, _data: &mut Held<'_>) -> io::Result<bool> {
        if !edits.replaced.is_empty() || edits.tables.len() != 1 {
            return Ok(false);
        }
        let Some(changed) = edits.tables.get(&TableId::FactionResources) else {
            return Ok(false);
        };
        let raw_mask = 1u64 << dashboard_declare::field(TableId::FactionResources, "raw")?;
        if changed.values().any(|&mask| mask & !raw_mask != 0) {
            return Ok(false);
        }
        let Some(replay_frame::frame::TableView::Complete(old)) = edits.previous.get(&TableId::FactionResources).map(|view| view.held()) else {
            return Ok(false);
        };
        let (Some(old), Ok(new)) = (old.rows::<tables::FactionResources>(), frame.complete::<tables::FactionResources>()) else {
            return Ok(false);
        };
        fn find(rows: &[FactionResourceRow], key: u128) -> Option<&FactionResourceRow> {
            rows.binary_search_by_key(&key, |row| u128::from(row.faction) << 32 | u128::from(row.ordinal)).ok().map(|at| &rows[at])
        }
        for &key in changed.keys() {
            let (Some(a), Some(b)) = (find(old, key), find(new, key)) else {
                return Ok(false);
            };
            if b.ordinal == 0 && (a.raw == 0) != (b.raw == 0) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn valid_reply_through(frame: &DecodedFrame, _params: &Self::Params, at: &At<'_, Self::Context>, reply: &Self::Reply, partial: bool) -> i32 {
        if partial {
            return frame.hour;
        }
        reply
            .through
            .0
            .min(before(at.holders.next_change(frame.hour)))
            .min(warnings_through(
                frame.hour,
                reply
                    .warnings
                    .iter()
                    .map(String::as_str)
                    .chain(reply.goal_reason.as_deref())
                    .chain(reply.refusals.iter().map(|refusal| refusal.reason.as_str())),
            ))
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        let catalogue = load.content().and_then(|options| catalogue::catalogue(source, options)).map_err(|why| why.to_string());
        let shown = catalogue.as_ref().map_or(0, |cat| Arc::as_ptr(cat) as usize);
        let held = source.state::<HeldArt>();
        let mut held = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let art = match &held.0 {
            Some((at, art)) if *at == shown => Arc::clone(art),
            _ => {
                let art = Arc::new(Art::default());
                held.0 = Some((shown, Arc::clone(&art)));
                art
            }
        };
        Ok(DetailContent { catalogue, naming: ResourceNaming::stored(source, load), art, inputs: ScriptedInputs::new(source.replay()) })
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let faction = super::faction_id(&params.faction).map_err(dashboard_core::invalid)?;
        reply(frame, params, faction, at)
    }
}

impl DetailContent {
    pub(super) fn with_scripted_playback(mut self, source: &Source, dates: &'static [DateSelection]) -> Self {
        self.inputs = self.inputs.with_lexical(source).with_prepared_block_dates(source, dates).with_retained_world(source);
        self
    }

    pub(super) fn catalogue(&self) -> Result<&Catalogue, &String> {
        self.catalogue.as_deref()
    }

    pub(super) fn naming(&self) -> &ResourceNaming {
        &self.naming
    }

    pub(super) fn art(&self) -> &Arc<Art> {
        &self.art
    }

    pub(super) fn inputs(&self) -> &ScriptedInputs {
        &self.inputs
    }
}

pub(super) fn goals_only_in(frame: &DecodedFrame, world: &DecodedFrame, params: &FactionDetailParams, faction: u64, filter: GoalFilter, at: &At<'_, DetailContent>) -> io::Result<FactionDetailReply> {
    let mut reply = blank(params);
    let cat = at.context.catalogue.as_ref().map(Arc::as_ref);
    let texts = cat.ok().map(|cat| Texts::new_in(frame, world, faction, cat, &at.context.art, params.viewpoint.as_ref()));
    goal_lists(frame, faction, texts.as_ref(), cat.err().map(String::as_str), &mut reply, at.live, Some(filter))?;
    if let Some(texts) = &texts {
        reply.warnings.extend(texts.warnings());
    }
    reply.warnings.extend(goals::counted(&reply.definitions));
    Ok(reply)
}

pub(super) fn reply(frame: &DecodedFrame, params: &FactionDetailParams, faction: u64, at: &At<'_, DetailContent>) -> io::Result<FactionDetailReply> {
    if let Ok(rows) = frame.complete::<tables::Factions>()
        && !rows.iter().any(|row| row.id == faction)
    {
        return Err(dashboard_core::refused("Faction is absent at this hour"));
    }
    let mut reply = blank(params);
    let cat = at.context.catalogue.as_ref().map(Arc::as_ref);
    if let Ok(cat) = cat && goals::scripted(frame, faction, cat) {
        let watched = if params.viewpoint.is_some() {
            vec![dashboard_text::triggers::inputs::TableRead::fields(TableId::Countries, &["canonical_slot", "tag"])]
        } else {
            vec![
                dashboard_text::triggers::inputs::TableRead::fields(TableId::LocalPlayer, &["country", "observer_country"]),
                dashboard_text::triggers::inputs::TableRead::fields(TableId::CountryLinks, &["canonical_slot"]),
            ]
        };
        let (done, through) = at.context.inputs.evaluate_with_reads(frame, &scripted_tables(), at.cancelled, |world| {
            let texts = Texts::new_in(frame, world, faction, cat, &at.context.art, params.viewpoint.as_ref());
            let done = goal_lists(frame, faction, Some(&texts), None, &mut reply, at.live, None);
            reply.warnings.extend(texts.warnings());
            doctrines(frame, faction, Some(&texts), &mut reply);
            (done, watched)
        })?;
        done?;
        reply.through = Through(through);
    } else {
        let texts = cat.ok().map(|cat| Texts::new(frame, faction, cat, &at.context.art, params.viewpoint.as_ref()));
        goal_lists(frame, faction, texts.as_ref(), cat.err().map(String::as_str), &mut reply, at.live, None)?;
        if let Some(texts) = &texts {
            reply.warnings.extend(texts.warnings());
        }
        doctrines(frame, faction, texts.as_ref(), &mut reply);
    }
    let tags = Tags::new(frame, slot_tags(at.holders, at.hour));
    projection(frame, faction, &mut reply, at.live)?;
    resources(frame, faction, &mut reply, &at.context.naming, at.cancelled, at.live)?;
    manpower(frame, faction, &tags, &mut reply, at.live)?;
    research(frame, faction, &tags, &mut reply, at.live)?;
    match frame.complete::<tables::FactionUnlocks>() {
        Ok(rows) => match owned(rows, faction, |row| row.faction).first() {
            Some(row) => {
                reply.research_unlocked = Some(row.research != 0);
                reply.manpower_unlocked = Some(row.manpower != 0);
            }
            None => reply.warnings.push("Faction unlocks: the faction has no row at this hour".into()),
        },
        Err(why) => reply.warnings.push(format!("Faction unlocks: {why}")),
    }
    match cat {
        Ok(cat) => match at.context.art.logo(frame, faction, cat) {
            Ok(logo) => reply.logo = Some(logo),
            Err(reason) => reply.refusals.push(FactionRefusalDetail { fact: "faction logo".into(), count: 1, reason }),
        },
        Err(why) => reply.refusals.push(FactionRefusalDetail { fact: "faction logo".into(), count: 1, reason: why.clone() }),
    }
    let mut art = BTreeMap::<&str, u32>::new();
    for definition in &reply.definitions {
        if let Some(reason) = definition.reason.as_deref().filter(|reason| reason.starts_with("Faction goal artwork")) {
            *art.entry(reason).or_default() += 1;
        }
    }
    let art: Vec<FactionRefusalDetail> =
        art.into_iter().map(|(reason, count)| FactionRefusalDetail { fact: "goal artwork".into(), count, reason: reason.to_owned() }).collect();
    reply.refusals.extend(art);
    reply.warnings.extend(goals::counted(&reply.definitions));
    Ok(reply)
}

pub(super) fn blank(params: &FactionDetailParams) -> FactionDetailReply {
    FactionDetailReply {
        through: Through(i32::MAX),
        branch_id: params.branch_id.clone(),
        faction: params.faction.clone(),
        logo: None,
        definitions: Vec::new(),
        manifest: None,
        goals: Vec::new(),
        completed: Vec::new(),
        cancelled: Vec::new(),
        goal_reason: None,
        resources: None,
        manpower: None,
        manpower_unlocked: None,
        research_unlocked: None,
        research_countries: None,
        doctrine_folders: None,
        refusals: Vec::new(),
        warnings: Vec::new(),
    }
}

pub(super) struct Tags<'h> {
    links: HashMap<u32, u32>,
    slots: HashMap<u32, (&'h str, u32)>,
    reserved: Result<String, String>,
    unlinked: Option<String>,
}

impl<'h> Tags<'h> {
    pub(super) fn new(frame: &DecodedFrame, slots: HashMap<u32, (&'h str, u32)>) -> Self {
        let (links, unlinked) = match frame.complete::<tables::CountryLinks>() {
            Ok(rows) => (rows.iter().map(|row| (row.id, row.canonical_slot)).collect(), None),
            Err(why) => (HashMap::new(), Some(why.to_string())),
        };
        let reserved = frame.complete::<tables::ReservedCountryNaming>().map_err(|why| why.to_string()).and_then(|rows| match rows {
            [row] => String::from_utf8(row.tag.clone()).map_err(|_| "The reserved country's tag is not UTF-8".to_owned()),
            _ => Err("Country zero has no unique reserved name".to_owned()),
        });
        Self { links, slots, reserved, unlinked }
    }

    fn tag(&self, raw: u32) -> Result<String, String> {
        if raw == 0 {
            return self.reserved.clone();
        }
        if let Some(why) = &self.unlinked {
            return Err(format!("Country links: {why}"));
        }
        let slot = self.links.get(&raw).ok_or_else(|| format!("Country {raw} has no link at this hour"))?;
        self.slots.get(slot).map(|(tag, _)| (*tag).to_owned()).ok_or_else(|| format!("No country holds slot {slot} at this hour"))
    }
}

pub(super) fn goal_lists(frame: &DecodedFrame, faction: u64, texts: Option<&Texts<'_>>, missing: Option<&str>, reply: &mut FactionDetailReply, live: &Cells, filter: Option<GoalFilter>) -> io::Result<()> {
    let held = (|| -> Result<(Option<u32>, &[FactionGoalEntryRow]), String> {
        let header = owned(frame.complete::<tables::FactionGoalStatus>().map_err(|why| why.to_string())?, faction, |row| row.faction)
            .first()
            .ok_or("Faction goal status is absent")?;
        let entries = owned(frame.complete::<tables::FactionGoalEntries>().map_err(|why| why.to_string())?, faction, |row| row.faction);
        let mut next = [0u32; 5];
        for row in entries {
            let expected = next.get_mut(row.kind as usize).ok_or("Unknown faction goal list kind")?;
            if row.ordinal != *expected {
                return Err("Faction goal ordinals are not contiguous".into());
            }
            *expected += 1;
        }
        Ok((header.manifest, entries))
    })();
    let (manifest, entries) = match held {
        Ok(held) => held,
        Err(why) => {
            reply.warnings.push(format!("Faction goals: {why}"));
            reply.goal_reason = Some(why);
            return Ok(());
        }
    };
    let progress = dashboard_declare::field(TableId::FactionGoalEntries, "progress_raw")?;
    let mut definitions: Vec<FactionGoalDefinition> = Vec::new();
    let mut named = HashMap::<Option<u32>, u32>::new();
    let mut define = |token: Option<u32>| -> u32 {
        *named.entry(token).or_insert_with(|| {
            definitions.push(match (texts, missing) {
                (Some(texts), _) => texts.definition(token),
                (None, why) => goals::unnamed(frame, token, why.unwrap_or("Faction goal names need the recorded game folders")),
            });
            definitions.len() as u32 - 1
        })
    };
    if filter.is_none() {
        reply.manifest = manifest.map(|token| define(Some(token)));
    }
    let mut left_out = 0usize;
    for row in entries {
        let state = match row.kind {
            0 => GoalState::of(row.status),
            1 => GoalState::Completed,
            2 => GoalState::Cancelled,
            _ => continue,
        };
        if filter.is_some_and(|filter| !state.shown(filter)) {
            left_out += usize::from(state == GoalState::Unrecognised);
            continue;
        }
        match row.kind {
            0 => {
                reply.goals.push(FactionHeldGoal { ordinal: row.ordinal, definition: define(row.token), status: row.status, active_sections: row.active_sections.clone() });
                let key = u128::from(faction) << 40 | u128::from(row.kind) << 32 | u128::from(row.ordinal);
                live.value(&format!("goal:{}", row.ordinal), "progress", Cell { table: TableId::FactionGoalEntries, column: progress, key }, 1.0);
            }
            1 => reply.completed.push(define(row.token)),
            2 => reply.cancelled.push(define(row.token)),
            _ => {}
        }
    }
    reply.definitions = definitions;
    if left_out > 0 {
        reply.warnings.push(format!("{left_out} faction goals are in a state this version does not recognise; only All shows them"));
    }
    Ok(())
}

fn projection(frame: &DecodedFrame, faction: u64, reply: &mut FactionDetailReply, live: &Cells) -> io::Result<()> {
    match frame.complete::<tables::FactionProjection>() {
        Ok(rows) if owned(rows, faction, |row| row.faction).len() == 1 => {
            let column = dashboard_declare::field(TableId::FactionProjection, "projection_raw")?;
            live.value(FACTION_ROW, "projection", Cell { table: TableId::FactionProjection, column, key: u128::from(faction) }, 1e-5);
        }
        Ok(_) => reply.warnings.push("Faction projection: the faction has no row at this hour".into()),
        Err(why) => reply.warnings.push(format!("Faction projection: {why}")),
    }
    Ok(())
}

fn resources(frame: &DecodedFrame, faction: u64, reply: &mut FactionDetailReply, naming: &ResourceNaming, cancelled: &AtomicBool, live: &Cells) -> io::Result<()> {
    let rows = match frame.complete::<tables::FactionResources>() {
        Ok(rows) => owned(rows, faction, |row| row.faction),
        Err(why) => {
            reply.warnings.push(format!("Faction pooled resources: {why}"));
            return Ok(());
        }
    };
    if rows.iter().enumerate().any(|(index, row)| row.ordinal as usize != index) {
        reply.warnings.push("Faction resource slots are not contiguous".into());
        return Ok(());
    }
    let definitions = frame.complete::<tables::ResourceDefinitions>();
    let mut learned = Learned::default();
    if let (Ok(definitions), Ok(lexical)) = (&definitions, frame.complete::<tables::LexicalTokens>()) {
        learned.learn(definitions, lexical);
    }
    let (infos, warnings) = naming.infos(&learned.list(), cancelled)?;
    reply.warnings.extend(warnings);
    let column = dashboard_declare::field(TableId::FactionResources, "raw")?;
    let mut pooled = Vec::new();
    let mut art = BTreeMap::<String, u32>::new();
    for row in rows {
        if row.ordinal == 0 && row.raw == 0 {
            continue;
        }
        let mut detail = FactionResourceDetail { ordinal: row.ordinal, token: None, name: None, icon: None, reason: None };
        let named = (|| -> Result<(), String> {
            let definition = definitions
                .as_ref()
                .map_err(ToString::to_string)?
                .iter()
                .find(|definition| definition.id == row.ordinal)
                .ok_or("Faction resource slot has no recorded definition")?;
            if definition.token != row.token {
                return Err("Faction resource slot token disagrees with its recorded definition".into());
            }
            let info = infos.iter().find(|info| info.id == row.ordinal).ok_or("Faction resource token spelling unavailable")?;
            detail.token = Some(info.token.clone());
            detail.name = info.name.clone();
            detail.icon = info.icon.clone();
            detail.name.as_ref().map(|_| ()).ok_or_else(|| "Faction resource name unavailable in recorded content".to_owned())
        })();
        if let Err(why) = named {
            detail.reason = Some(why);
        }
        if detail.icon.is_none() {
            *art.entry(format!("Faction resource icon slot {} ({:?}) is unavailable", row.ordinal, detail.token)).or_default() += 1;
        }
        let key = u128::from(faction) << 32 | u128::from(row.ordinal);
        live.value(&format!("resource:{}", row.ordinal), "amount", Cell { table: TableId::FactionResources, column, key }, 1e-5);
        pooled.push(detail);
    }
    reply.refusals.extend(art.into_iter().map(|(reason, count)| FactionRefusalDetail { fact: "resource artwork".into(), count, reason }));
    reply.resources = Some(pooled);
    Ok(())
}

pub(super) fn manpower(frame: &DecodedFrame, faction: u64, tags: &Tags<'_>, reply: &mut FactionDetailReply, live: &Cells) -> io::Result<()> {
    let rows = match frame.complete::<tables::FactionManpower>() {
        Ok(rows) => owned(rows, faction, |row| row.faction),
        Err(why) => {
            reply.warnings.push(format!("Faction manpower: {why}"));
            return Ok(());
        }
    };
    if rows.iter().enumerate().any(|(index, row)| row.ordinal as usize != index) {
        reply.warnings.push("Faction manpower ordinals are not contiguous".into());
        return Ok(());
    }
    let column = dashboard_declare::field(TableId::FactionManpower, "value")?;
    let mut sources = Vec::with_capacity(rows.len());
    let mut members = Vec::with_capacity(rows.len());
    for row in rows {
        let tag = match tags.tag(row.country) {
            Ok(tag) => Some(tag),
            Err(why) => {
                reply.warnings.push(format!("Faction manpower entry {}: {why}", row.ordinal));
                None
            }
        };
        let source = Cell { table: TableId::FactionManpower, column, key: u128::from(faction) << 32 | u128::from(row.ordinal) };
        live.value(&format!("manpower:{}", row.ordinal), "value", source, 1.0);
        sources.push(source);
        members.push(FactionManpowerDetail { ordinal: row.ordinal, tag });
    }
    if !sources.is_empty() {
        live.sum(FACTION_ROW, "manpower", &sources, 1.0);
    }
    reply.manpower = Some(members);
    Ok(())
}

fn research(frame: &DecodedFrame, faction: u64, tags: &Tags<'_>, reply: &mut FactionDetailReply, live: &Cells) -> io::Result<()> {
    let rows = match frame.complete::<tables::FactionResearch>() {
        Ok(rows) => owned(rows, faction, |row| row.faction),
        Err(why) => {
            reply.warnings.push(format!("Faction research sharing: {why}"));
            return Ok(());
        }
    };
    let Some(header) = rows.first().filter(|row| row.ordinal == 0) else {
        reply.warnings.push("Faction research sharing: the faction has no sharing row at this hour".into());
        return Ok(());
    };
    let column = dashboard_declare::field(TableId::FactionResearch, "scientist_base_raw")?;
    live.value(FACTION_ROW, "research", Cell { table: TableId::FactionResearch, column, key: u128::from(faction) << 32 }, 1e-5);
    match header.countries.iter().map(|raw| tags.tag(*raw)).collect::<Result<Vec<_>, _>>() {
        Ok(countries) => reply.research_countries = Some(countries),
        Err(why) => reply.warnings.push(format!("Faction research members: {why}")),
    }
    Ok(())
}

fn doctrines(frame: &DecodedFrame, faction: u64, texts: Option<&Texts<'_>>, reply: &mut FactionDetailReply) {
    let rows = match frame.complete::<tables::FactionDoctrine>() {
        Ok(rows) => owned(rows, faction, |row| row.faction),
        Err(why) => {
            reply.warnings.push(format!("Faction doctrine folders: {why}"));
            return;
        }
    };
    let folders = rows
        .iter()
        .map(|row| {
            let mut detail = FactionDoctrineDetail { ordinal: row.ordinal, token: None, name: None, reason: None };
            let named = (|| -> Result<String, String> {
                let spelling = dashboard_text::faction_names::template_spelling(frame, Some(row.token))?.ok_or("Faction doctrine token is absent")?;
                detail.token = Some(spelling.clone());
                let texts = texts.ok_or("Faction doctrine names need the recorded game folders")?;
                let text = texts.localise(&spelling)?;
                let text = if text.contains('[') { texts.expand(&text)? } else { text };
                catalogue::plain_text(&text)
            })();
            match named {
                Ok(name) => detail.name = Some(name),
                Err(why) => detail.reason = Some(why),
            }
            detail
        })
        .collect();
    reply.doctrine_folders = Some(folders);
}
