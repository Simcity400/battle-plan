use dashboard_world::products::equipment_names::{self as names, EquipmentIcon, VariantName};

use dashboard_base::products::{Country, validate_country, flows::{CountryFlowReply, FlowEdge, FlowEnd, FlowEndpoint, FlowGap, FlowPart, FlowShape, build_flows}};
use dashboard_base::{Source, products::{LoadContext, check_reply_budget, at_hour::{At, AtHour, Edits, Held, Owner, Read, ReadsAt, before, field_columns}}};
use dashboard_follow::live::Source as Cell;
use dashboard_query::{identity::Holders, series::ranked::GroupSelection};
use dashboard_world::products::group_members::GroupsAt;
use dashboard_query::coverage::{Witness, witnessed, witnessed_rows};
use dashboard_text::{metadata::link_slot, variant_names::Texts};
use replay_data::rows::{IndustrialOrganisationDetailsRow, IndustrialOrganisationRow, RowCodec, TableId, VariantDefinitionRow, column, tables};
use replay_frame::frame::{DecodedFrame, SharedView, TableView};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io,
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct ProductionLicences;
pub struct IndustrialOrganisations;
pub struct EquipmentDesigns;

const UNSELECTED_POLICY: u32 = 0x4c17;
const FUNDS: usize = column::<IndustrialOrganisationDetailsRow>("state.funds_raw");
const POINTS: usize = column::<IndustrialOrganisationDetailsRow>("state.points");
const ORGANISATION_FIELDS: &[usize] = &[
    column::<IndustrialOrganisationRow>("country"),
    column::<IndustrialOrganisationRow>("owner"),
    column::<IndustrialOrganisationRow>("token"),
    column::<IndustrialOrganisationRow>("name"),
    column::<IndustrialOrganisationRow>("icon"),
];
const ORGANISATION_DETAILS: &[usize] = &[
    column::<IndustrialOrganisationDetailsRow>("state.size"),
    column::<IndustrialOrganisationDetailsRow>("state.policy"),
    column::<IndustrialOrganisationDetailsRow>("state.cooldown_hour"),
    column::<IndustrialOrganisationDetailsRow>("unlocked"),
];

fn validate_branch(branch: &str) -> Result<(), &'static str> {
    branch.parse::<u64>().map(|_| ()).map_err(|_| "Invalid branch")
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductionLicencesParams {
    pub branch_id: String,
    #[doc = " One country: the licences it holds and grants by partner. Otherwise the links between `countries` (every country when empty)."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub subject: Option<Country>,
    #[serde(default)]
    pub countries: Vec<Country>,
    #[serde(default)]
    pub between: bool,
    #[doc = " A group: its members at the hour (or every member so far), resolved in Rust; replaces `countries`."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub limit: Option<u32>,
    #[doc = " An explicit top partners per side: exactly this many named and the rest folded, where the order is proven at the hour."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub top: Option<u32>,
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub side_totals_only: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndustrialOrganisationsParams {
    pub branch_id: String,
    pub country: Country,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OrganisationNow {
    pub id: String,
    pub token: String,
    pub name: Option<String>,
    pub name_unknown: Option<String>,
    #[doc = " The icon's key in `icons`."]
    pub icon: Option<String>,
    pub icon_unknown: Option<String>,
    pub size: Option<i32>,
    #[doc = " Traits unlocked."]
    pub traits: Option<u32>,
    #[doc = " The selected policy's script key; null for none or where the state was not read."]
    pub policy_token: Option<String>,
    pub policy_name: Option<String>,
    #[doc = " Why a selected policy cannot be named, when it cannot."]
    pub policy_unknown: Option<String>,
    #[doc = " The hour the policy may change again; null when none is set. The days left are counted"]
    #[doc = " where they are shown, as the game counts them: its day less the shown hour's."]
    pub cooldown_hour: Option<i32>,
    #[doc = " Why this organisation's state (size, funds, traits, policy) is not known at the hour, when it is not."]
    pub details_missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IndustrialOrganisationsReply {
    pub branch_id: String,
    pub tag: String,
    pub organisations: Vec<OrganisationNow>,
    #[doc = " Why the organisations are not known at the hour, when they are not."]
    pub missing: Option<String>,
    pub icons: Vec<EquipmentIcon>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EquipmentDesignsParams {
    pub branch_id: String,
    pub country: Country,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct DesignNow {
    #[cfg_attr(any(test, feature = "export-types"), ts(type = "Omit<VariantName, 'hour'>"))]
    pub name: VariantName,
    #[doc = " Flagged obsolete at the hour."]
    pub obsolete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EquipmentDesignsReply {
    pub branch_id: String,
    pub tag: String,
    #[doc = " The country's saved designs at the hour, current before obsolete, then by name."]
    pub designs: Vec<DesignNow>,
    #[doc = " Why the designs are not known at the hour, when they are not."]
    pub missing: Option<String>,
    pub icons: Vec<EquipmentIcon>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(IndustrialOrganisationsReply, EquipmentDesignsReply);

const LICENCES_NOT_RECORDED: &str = "A licence was not recorded at this hour";
const LICENCE_UNLINKED: &str = "The licence names a country the recording cannot link at this hour";

pub struct WitnessedLicences {
    pub edges: Vec<FlowEdge>,
    pub gaps: Vec<FlowGap>,
    pub unlinked: HashMap<u32, String>,
}

pub fn witnessed_licences(frame: &DecodedFrame) -> Result<WitnessedLicences, String> {
    let read = witnessed_rows::<tables::ProductionLicenseRecords>(frame);
    if read.rows.is_empty()
        && let Some(reason) = read.missing
    {
        return Err(reason);
    }
    let mut unlinked = HashMap::new();
    let mut end = |raw: u32| match witnessed::<tables::CountryLinks>(frame, u128::from(raw)) {
        Witness::Row(link) => FlowEnd::Slot(link.canonical_slot),
        Witness::Absent => {
            unlinked.entry(raw).or_insert_with(|| LICENCE_UNLINKED.to_owned());
            FlowEnd::Raw(raw)
        }
        Witness::NotRecorded(reason) => {
            unlinked.entry(raw).or_insert_with(|| format!("Which country the licence names was not read at this hour: {reason}"));
            FlowEnd::Raw(raw)
        }
    };
    let edges = read.rows.iter().map(|row| FlowEdge { from: end(row.giver), to: end(row.owner), part: 0, amount: Some(1) }).collect();
    Ok(WitnessedLicences { edges, gaps: read.missing.map(FlowGap::partial).into_iter().collect(), unlinked })
}

fn licence_parts() -> Vec<FlowPart> {
    vec![FlowPart { key: "licences".to_owned(), name: Some("Licences".to_owned()), name_missing: None, ordinal: 0, icon: None }]
}

fn licences_unavailable(params: &ProductionLicencesParams, holders: &Holders, context: &GroupsAt, hour: i32) -> bool {
    params.subject.as_ref().is_some_and(|country| holders.slot_at(country, hour).is_none())
        || matches!(context.chosen_slots(params.group.as_ref(), &params.countries, holders, hour), Ok(Err(_)))
}

fn licence_reaches(from: FlowEnd, to: FlowEnd, subject: Option<u32>, chosen: Option<&HashSet<u32>>, between: bool) -> bool {
    if let Some(subject) = subject {
        return (from.slot().is_none_or(|slot| slot == subject) || to.slot().is_none_or(|slot| slot == subject))
            && !(from == FlowEnd::Slot(subject) && to == from);
    }
    if from.slot().is_some() && from == to {
        return false;
    }
    let Some(chosen) = chosen else { return true };
    let ends = [from.slot(), to.slot()];
    let unknown = ends.iter().any(Option::is_none);
    let mut known = ends.iter().flatten();
    let decided = if between { known.any(|slot| !chosen.contains(slot)) } else { known.any(|slot| chosen.contains(slot)) };
    !matches!((decided, between, unknown), (true, true, _) | (false, false, false))
}

impl AtHour for ProductionLicences {
    const KEY: &'static str = "production-licences";
    const TITLE: &'static str = "Production licences";
    const CONTENT_DEPENDENT: bool = false;
    type Params = ProductionLicencesParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, subject, countries, "countries[]", between, group, limit, top, sideTotalsOnly];
    type Reply = CountryFlowReply;
    type Context = GroupsAt;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![ProductionLicencesParams::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        validate_branch(&params.branch_id)?;
        params.countries.iter().try_for_each(validate_country)?;
        params.subject.as_ref().map_or(Ok(()), validate_country)?;
        dashboard_base::products::flows::validate_chosen(params.subject.as_ref(), &params.countries, params.group.is_some())
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        vec![TableId::ProductionLicenseRecords, TableId::CountryLinks]
    }

    fn reads_in(params: &Self::Params, _: Option<Owner>, stage: usize, frame: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        if licences_unavailable(params, at.holders, at.context, at.hour) {
            return Vec::new();
        }
        match stage {
            0 => vec![Read::all(TableId::ProductionLicenseRecords)],
            1 => {
                let rows = witnessed_rows::<tables::ProductionLicenseRecords>(frame);
                if rows.rows.is_empty() {
                    return Vec::new();
                }
                vec![Read::these(TableId::CountryLinks, rows.rows.iter().flat_map(|row| [u128::from(row.owner), u128::from(row.giver)]))]
            }
            _ => Vec::new(),
        }
    }

    fn retained_reads(params: &Self::Params, owner: Option<Owner>, stage: usize, frame: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> (Vec<Read>, i32) {
        let until = if stage == 0 {
            before(at.holders.next_change(at.hour)).min(at.context.valid_through(params.group.as_ref(), at.hour))
        } else {
            i32::MAX
        };
        (Self::reads_in(params, owner, stage, frame, at), until)
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        (table == TableId::ProductionLicenseRecords).then(|| field_columns(table, &["giver"]))
    }

    fn plan_changes(stage: usize, edits: &Edits, _: &DecodedFrame, _: &ReadsAt<'_, Self::Context>) -> bool {
        stage == 1 && edits.fields(TableId::ProductionLicenseRecords, u64::MAX)
    }

    fn compares_previous(table: TableId) -> bool {
        matches!(table, TableId::ProductionLicenseRecords | TableId::CountryLinks)
    }

    fn advance(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>, edits: &Edits, _: &dashboard_follow::live::Live, _: &mut Held<'_>) -> io::Result<bool> {
        if !edits.replaced.is_empty() {
            return Ok(false);
        }
        let licences = TableId::ProductionLicenseRecords;
        let links = TableId::CountryLinks;
        let Some(keys) = edits.tables.get(&licences) else {
            return Ok(false);
        };
        let Some(prior_licences) = edits.previous.get(&licences) else {
            return Ok(false);
        };
        let Some(prior_links) = edits.previous.get(&links) else {
            return Ok(false);
        };
        let now_links = &frame.tables[links.index()];
        if edits.tables.get(&links).is_some_and(|rows| rows.keys().any(|&id| prior_links.keys().contains(id) && now_links.keys().contains(id))) {
            return Ok(false);
        }
        let subject = params.subject.as_ref().map(|country| at.holders.slot_at(country, at.hour));
        if subject == Some(None) {
            return Ok(true);
        }
        let Ok(chosen) = at.context.chosen_slots(params.group.as_ref(), &params.countries, at.holders, at.hour)? else {
            return Ok(true);
        };
        let relevant = |licences: &SharedView, links: &SharedView, key: u128| {
            let TableView::Complete(rows) = licences.held() else { return None };
            let rows = rows.rows::<tables::ProductionLicenseRecords>()?;
            let row = &rows[rows.binary_search_by_key(&key, RowCodec::key).ok()?];
            let TableView::Complete(rows) = links.held() else { return None };
            let links = rows.rows::<tables::CountryLinks>()?;
            let end = |raw| link_slot(links, raw).map_or(FlowEnd::Raw(raw), FlowEnd::Slot);
            let (from, to) = (end(row.giver), end(row.owner));
            licence_reaches(from, to, subject.flatten(), chosen.as_ref(), params.between).then_some((from, to))
        };
        Ok(keys.keys().all(|&key| relevant(prior_licences, prior_links, key) == relevant(&frame.tables[licences.index()], now_links, key)))
    }

    fn context(_source: &Source, _load: &LoadContext) -> io::Result<Self::Context> {
        Ok(GroupsAt::default())
    }

    fn context_for(source: &Source, load: &LoadContext, params: &Self::Params) -> io::Result<Self::Context> {
        GroupsAt::load(source, load, &params.branch_id, params.group.as_ref())
    }

    fn valid_through(_frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> i32 {
        before(at.holders.next_change(at.hour)).min(at.context.valid_through(params.group.as_ref(), at.hour))
    }

    fn valid_reply_through(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>, _: &Self::Reply, _: bool) -> i32 {
        Self::valid_through(frame, params, at)
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let empty = |missing: String| CountryFlowReply::not_recorded(missing, 0, Vec::new());
        let resolve = |country: &Country| at.holders.slot_at(country, at.hour);
        let subject = params.subject.as_ref().map(resolve);
        if let Some(None) = subject {
            return Ok(empty(params.subject.as_ref().and_then(|country| at.holders.not_known(country, at.hour)).unwrap_or_else(|| "The chosen country holds no tag at this hour".to_owned())));
        }
        let chosen = match at.context.chosen_slots(params.group.as_ref(), &params.countries, at.holders, at.hour)? {
            Ok(chosen) => chosen,
            Err(refused) => return Ok(empty(refused)),
        };
        let licences = match witnessed_licences(frame) {
            Ok(licences) => licences,
            Err(why) => return Ok(empty(format!("Licences are not recorded at this hour: {why}"))),
        };
        let shape = FlowShape {
            subject: subject.flatten(),
            chosen: chosen.as_ref(),
            between: params.between,
            limit: params.limit.map(|limit| limit.max(1) as usize),
            top: params.top.map(|top| top.max(1) as usize),
            side_totals_only: params.side_totals_only.unwrap_or_default(),
            table: false,
            not_recorded: LICENCES_NOT_RECORDED,
            unlinked: LICENCE_UNLINKED,
        };
        let describe = |end: FlowEnd| match end {
            FlowEnd::Raw(raw) => FlowEndpoint::unidentified(end, licences.unlinked.get(&raw).cloned().unwrap_or_else(|| LICENCE_UNLINKED.to_owned())),
            FlowEnd::Slot(_) | FlowEnd::Collective => FlowEndpoint::at(at.holders, end, at.hour, LICENCE_UNLINKED),
        };
        let reply = build_flows(&licences.edges, &licences.gaps, licence_parts(), 0, &shape, &describe);
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}

fn organisations_of<'a>(frame: &'a DecodedFrame, owner: Owner) -> io::Result<Vec<&'a IndustrialOrganisationRow>> {
    let links = frame.complete::<tables::CountryLinks>()?;
    Ok(frame.complete::<tables::IndustrialOrganisations>()?.iter().filter(|row| link_slot(links, row.country) == Some(owner.slot)).collect())
}

impl AtHour for IndustrialOrganisations {
    const KEY: &'static str = "industrial-organisations";
    const TITLE: &'static str = "Industrial organisations";
    const CONTENT_DEPENDENT: bool = true;
    type Params = IndustrialOrganisationsParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country];
    type Reply = IndustrialOrganisationsReply;
    type Context = names::Context;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![IndustrialOrganisationsParams::decl(), IndustrialOrganisationsReply::decl(), OrganisationNow::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        validate_branch(&params.branch_id)?;
        validate_country(&params.country)
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        let mut tables = names::ORGANISATION_TABLES.to_vec();
        tables.push(TableId::IndustrialOrganisationDetails);
        tables
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        match table {
            TableId::IndustrialOrganisations => Some(ORGANISATION_FIELDS.to_vec()),
            TableId::IndustrialOrganisationDetails => Some([ORGANISATION_DETAILS, &[FUNDS, POINTS]].concat()),
            _ => names::naming_columns(table),
        }
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> {
        if table == TableId::IndustrialOrganisationDetails { Some(ORGANISATION_DETAILS.to_vec()) } else { Self::columns(table) }
    }

    fn reads(_params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let Some(owner) = owner else { return Vec::new() };
        let owned = || organisations_of(held, owner).unwrap_or_default();
        match step {
            0 => names::ORGANISATION_WHOLE.iter().chain([&TableId::IndustrialOrganisations]).map(|&table| Read::all(table)).collect(),
            1 => vec![Read::these(TableId::IndustrialOrganisationDetails, owned().iter().map(|row| u128::from(row.organisation)))],
            2 => {
                let mut tokens: BTreeSet<u32> = owned().iter().map(|row| row.token).collect();
                let policies = owned().into_iter().filter_map(|row| witnessed::<tables::IndustrialOrganisationDetails>(held, u128::from(row.organisation)).row()).map(|detail| detail.state.policy);
                tokens.extend(policies.filter(|&policy| policy != UNSELECTED_POLICY));
                names::spelling_reads(held, tokens, false)
            }
            _ => Vec::new(),
        }
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        names::context(source, load)
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Industrial organisations country is unavailable at this hour"))?;
        let mut reply = IndustrialOrganisationsReply { branch_id: params.branch_id.clone(), tag: at.holders.name(&params.country, at.hour), organisations: Vec::new(), missing: None, icons: Vec::new(), warnings: Vec::new() };
        let rows = match organisations_of(frame, owner) {
            Ok(rows) => rows,
            Err(error) => {
                reply.missing = Some(format!("Industrial organisations are not recorded at this hour: {}", names::undated_table_error(frame, &error)));
                return Ok(reply);
            }
        };
        let lexical = frame.complete::<tables::LexicalTokens>().ok();
        let texts: Option<&dyn Texts> = at.context.as_ref().ok().map(|content| content.catalogue.as_ref() as &dyn Texts);
        let ids: Vec<u64> = rows.iter().map(|row| row.organisation).collect();
        let mut named = names::catalogue_names(frame, &[], &ids, at.context);
        let details_table = TableId::IndustrialOrganisationDetails;
        for row in rows {
            let id = row.organisation.to_string();
            let key = u128::from(row.organisation);
            at.live.value(&id, "funds", Cell { table: details_table, column: FUNDS, key }, 1.0);
            at.live.value(&id, "points", Cell { table: details_table, column: POINTS, key }, 1.0);
            let name = named.organisations.remove(&row.organisation);
            let (detail, details_missing) = match witnessed::<tables::IndustrialOrganisationDetails>(frame, u128::from(row.organisation)) {
                Witness::Row(detail) => (Some(detail), None),
                Witness::Absent => (None, Some("The organisation's size, funds and policy are not recorded at this hour".to_owned())),
                Witness::NotRecorded(reason) => (None, Some(format!("Organisation sizes, funds and policies are not recorded at this hour: {reason}"))),
            };
            let selected = detail.map(|detail| detail.state.policy).filter(|&policy| policy != UNSELECTED_POLICY);
            let policy_token = selected.and_then(|policy| lexical.and_then(|lexical| names::spelled(lexical, policy)));
            let policy_unknown = match (selected, &policy_token, texts) {
                (None, ..) => None,
                (Some(_), None, _) => Some("lexical tokens does not spell the selected policy.".to_owned()),
                (Some(_), Some(_), None) => at.context.as_ref().err().cloned(),
                (Some(_), Some(token), Some(texts)) => (!texts.has(token)).then(|| "The content does not name the selected policy.".to_owned()),
            };
            let policy_name = policy_token.as_deref().and_then(|token| texts.and_then(|texts| texts.text(token)));
            reply.organisations.push(OrganisationNow {
                id,
                token: name.as_ref().map(|name| name.token.clone()).unwrap_or_default(),
                name: name.as_ref().and_then(|name| name.name.clone()),
                name_unknown: name.as_ref().and_then(|name| name.name_unknown.clone()),
                icon: name.as_ref().and_then(|name| name.icon.clone()),
                icon_unknown: name.as_ref().and_then(|name| name.icon_unknown.clone()),
                size: detail.map(|detail| detail.state.size),
                traits: detail.map(|detail| detail.unlocked.len() as u32),
                policy_token,
                policy_name,
                policy_unknown,
                cooldown_hour: detail.and_then(|detail| detail.state.cooldown_hour),
                details_missing,
            });
        }
        reply.organisations.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.len().cmp(&b.id.len())).then_with(|| a.id.cmp(&b.id)));
        reply.warnings.extend(std::mem::take(&mut named.warnings));
        reply.icons = named.icons();
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}

fn designs_of(frame: &DecodedFrame, owner: Owner) -> io::Result<Vec<&replay_data::rows::VariantDefinitionRow>> {
    let links = frame.complete::<tables::CountryLinks>()?;
    Ok(frame.complete::<tables::VariantDefinitions>()?.iter().filter(|row| link_slot(links, row.creator_link) == Some(owner.slot)).collect())
}

impl AtHour for EquipmentDesigns {
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    const KEY: &'static str = "equipment-designs";
    const TITLE: &'static str = "Equipment designs";
    const CONTENT_DEPENDENT: bool = true;
    type Params = EquipmentDesignsParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country];
    type Reply = EquipmentDesignsReply;
    type Context = names::Context;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![EquipmentDesignsParams::decl(), EquipmentDesignsReply::decl(), DesignNow::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        validate_branch(&params.branch_id)?;
        validate_country(&params.country)
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        names::TABLES.to_vec()
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        let mut columns = names::naming_columns(table)?;
        if table == TableId::VariantDefinitions {
            columns.push(const { column::<VariantDefinitionRow>("obsolete") });
        }
        Some(columns)
    }

    fn reads(_params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let Some(owner) = owner else { return Vec::new() };
        match step {
            0 => names::WHOLE.iter().chain([&TableId::VariantDefinitions]).map(|&table| Read::all(table)).collect(),
            1 => vec![names::capital_state_read(held)],
            2 => names::spelling_reads(held, designs_of(held, owner).unwrap_or_default().iter().map(|row| row.equipment_token).collect(), true),
            _ => Vec::new(),
        }
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        names::context(source, load)
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Equipment designs country is unavailable at this hour"))?;
        let mut reply = EquipmentDesignsReply { branch_id: params.branch_id.clone(), tag: at.holders.name(&params.country, at.hour), designs: Vec::new(), missing: None, icons: Vec::new(), warnings: Vec::new() };
        let rows = match designs_of(frame, owner) {
            Ok(rows) => rows,
            Err(error) => {
                reply.missing = Some(format!("Equipment designs are not recorded at this hour: {}", names::undated_table_error(frame, &error)));
                return Ok(reply);
            }
        };
        let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
        let mut named = names::catalogue_names(frame, &ids, &[], at.context);
        reply.designs = rows
            .iter()
            .filter_map(|row| Some(DesignNow { name: named.variants.remove(&row.id)?, obsolete: row.obsolete != 0 }))
            .collect();
        reply.designs.sort_by(|a, b| a.obsolete.cmp(&b.obsolete).then_with(|| a.name.name.cmp(&b.name.name)).then_with(|| a.name.id.len().cmp(&b.name.id.len())).then_with(|| a.name.id.cmp(&b.name.id)));
        reply.warnings.extend(std::mem::take(&mut named.warnings));
        reply.icons = named.icons();
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}
