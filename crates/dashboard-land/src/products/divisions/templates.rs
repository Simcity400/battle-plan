
use dashboard_base::products::{Country, validate_country};
use super::{ArmyAttribution, Content, Countries, Refusals, find, side, token_key};
use dashboard_base::{Source, products::{LoadContext, at_hour::{At, AtHour, Edits, Held, Keys, Owner, Read, ReadsAt, field_mask}}};
use replay_data::rows::{DivisionEquipmentTemplateRow, Recorded, TableId, TemplateSubunit, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

const TABLES: [TableId; 7] = [
    TableId::Countries,
    TableId::CountryLinks,
    TableId::DivisionTemplates,
    TableId::DivisionTemplateIcons,
    TableId::LexicalTokens,
    TableId::DivisionEquipmentCounts,
    TableId::DivisionEquipmentByTemplate,
];

pub struct DivisionTemplates;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DivisionTemplatesParams {
    pub branch_id: String,
    pub country: Country,
    pub count_for: ArmyAttribution,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct TemplateUnit {
    #[doc = " The sub-unit's content key, shown on hover only."]
    pub key: Option<String>,
    pub name: Option<String>,
    pub name_missing: Option<String>,
    pub count: i32,
    #[doc = " A support company rather than a battalion."]
    pub support: bool,
    #[doc = " A country-local copy of the definition."]
    pub local: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum DesignerKind {
    Ordinary,
    Headquarters,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct TemplateEntry {
    #[doc = " The template's full native handle."]
    pub template: String,
    pub name: Option<String>,
    pub name_missing: Option<String>,
    pub icon: Option<String>,
    pub icon_missing: Option<String>,
    #[doc = " Position in the country's roster."]
    pub roster_ordinal: Option<u32>,
    #[doc = " Divisions of the template counted for the country; none when not known."]
    pub fielded: Option<u32>,
    pub units: Vec<TemplateUnit>,
    pub units_missing: Option<String>,
    #[doc = " Signed fixed-point base combat width with five decimals."]
    pub base_width_raw: Option<String>,
    #[doc = " The template's stored priority and the game's name for it."]
    pub priority: Option<i32>,
    pub priority_name: Option<String>,
    pub priority_missing: Option<String>,
    pub locked: Option<bool>,
    pub obsolete: Option<bool>,
    #[doc = " The hour the template was made obsolete, when recorded."]
    pub obsolete_hour: Option<i32>,
    pub designer: Option<DesignerKind>,
    pub force_recruiting: Option<bool>,
    pub new_equipment: Option<bool>,
    pub foreign_equipment: Option<bool>,
    pub manpower: Option<i32>,
    pub training_days: Option<i32>,
    pub settings_missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct DivisionTemplatesReply {
    pub tag: String,
    pub count_for: ArmyAttribution,
    pub templates: Vec<TemplateEntry>,
    #[doc = " Why fielded counts are unknown, when they all are."]
    pub fielded_missing: Option<String>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(DivisionTemplatesReply);

const UNLINKED_DIVISION: &str = "Fielded counts are unknown: a division of this attribution has a country with no recorded link";
const UNLINKED_TEMPLATE: &str = "A template's country has no recorded link; it is left out";
const MODEL_UNREADABLE: &str = "Template composition could not be read at capture";

pub(crate) fn slot_side_range(slot: u32, side: u8) -> (u128, u128) {
    let high = u128::from(slot) << 72 | u128::from(side) << 64;
    (high, high | u128::from(u64::MAX))
}

fn owned(frame: &DecodedFrame, slot: u32) -> (Vec<u64>, usize) {
    let (Ok(countries), Ok(roster)) = (Countries::read(frame), frame.complete::<tables::DivisionTemplates>()) else { return (Vec::new(), 0) };
    let mut owned = Vec::new();
    let mut unlinked = 0;
    for row in roster {
        match countries.slot(row.owner) {
            Some(owner) if owner == slot => owned.push(row.template),
            Some(_) => {}
            None if row.owner == 0 => {}
            None => unlinked += 1,
        }
    }
    (owned, unlinked)
}

fn fielded(frame: &DecodedFrame, slot: u32, side: u8) -> Result<BTreeMap<u64, Recorded<i64>>, String> {
    let counts = frame.complete::<tables::DivisionEquipmentCounts>().map_err(|why| format!("Divisions were not counted at this hour: {why}"))?;
    let rows = frame.complete::<tables::DivisionEquipmentByTemplate>().map_err(|why| format!("Divisions were not counted at this hour: {why}"))?;
    let key = |row: &replay_data::rows::DivisionEquipmentCountRow| (row.slot, row.side);
    if let Ok(at) = counts.binary_search_by_key(&(slot, side), key)
        && counts[at].unresolved != 0
    {
        return Err(UNLINKED_DIVISION.to_owned());
    }
    Ok(rows.iter().filter(|row: &&DivisionEquipmentTemplateRow| row.slot == slot && row.side == side).map(|row| (row.template, row.divisions)).collect())
}

impl AtHour for DivisionTemplates {
    const KEY: &'static str = "division-templates";
    const TITLE: &'static str = "Division templates";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    type Params = DivisionTemplatesParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, countFor];
    type Reply = DivisionTemplatesReply;
    type Context = Content;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![DivisionTemplatesParams::decl(), DivisionTemplatesReply::decl(), TemplateEntry::decl(), TemplateUnit::decl(), DesignerKind::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid branch")?;
        validate_country(&params.country)
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        TABLES.to_vec()
    }

    fn reads(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let Some(owner) = owner else { return Vec::new() };
        let side = side(params.count_for);
        match step {
            0 => {
                let (lo, hi) = slot_side_range(owner.slot, side);
                vec![
                    Read::all(TableId::Countries),
                    Read::all(TableId::CountryLinks),
                    Read::all(TableId::DivisionTemplates),
                    Read::these(TableId::DivisionEquipmentCounts, [u128::from(owner.slot) << 8 | u128::from(side)]),
                    Read { table: TableId::DivisionEquipmentByTemplate, keys: Keys::Range(lo, hi) },
                ]
            }
            1 => vec![Read::these(TableId::DivisionTemplateIcons, owned(held, owner.slot).0.into_iter().map(u128::from))],
            2 => vec![Read::these(TableId::LexicalTokens, held.complete::<tables::DivisionTemplateIcons>().unwrap_or(&[]).iter()
                .filter(|row| row.model_unreadable == 0)
                .filter_map(|row| row.grids.require("template grids").ok())
                .flat_map(|grid| grid.regiments.iter().chain(&grid.regimental_support).chain(&grid.support))
                .filter(|unit| unit.token != 0)
                .map(|unit| u128::from(unit.token)))],
            _ => Vec::new(),
        }
    }

    fn plan_changes(stage: usize, edits: &Edits, _frame: &DecodedFrame, _at: &ReadsAt<'_, Self::Context>) -> bool {
        let roster = edits.fields(TableId::DivisionTemplates, field_mask(TableId::DivisionTemplates, &["owner"]))
            || edits.fields(TableId::CountryLinks, u64::MAX);
        match stage {
            1 => roster,
            2 => roster || edits.fields(TableId::DivisionTemplateIcons, field_mask(TableId::DivisionTemplateIcons, &["grids", "model_unreadable"])),
            _ => false,
        }
    }

    fn advance(frame: &DecodedFrame, _params: &Self::Params, at: &At<'_, Self::Context>, edits: &Edits, _live: &dashboard_follow::live::Live, _data: &mut Held<'_>) -> io::Result<bool> {
        let table = TableId::DivisionTemplates;
        if !edits.only_columns(table, field_mask(table, &["name", "name_key"])) {
            return Ok(false);
        }
        let (Some(owner), Ok(countries), Ok(roster)) = (at.owner, Countries::read(frame), frame.complete::<tables::DivisionTemplates>()) else {
            return Ok(false);
        };
        Ok(edits.tables.get(&table).is_some_and(|names| names.keys().all(|&key| {
            find(Some(roster), key as u64, |row| row.template).is_some_and(|row| countries.slot(row.owner) != Some(owner.slot))
        })))
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        let fields: &[&str] = match table {
            TableId::DivisionTemplates => &["owner", "name", "name_key"],
            TableId::DivisionEquipmentCounts => &["unresolved"],
            TableId::DivisionEquipmentByTemplate => &["divisions"],
            TableId::DivisionTemplateIcons => &["priority_unit", "priority_unit_name", "custom_icon", "country", "display_name", "settings", "base_width", "grids", "model_unreadable"],
            _ => return super::naming_columns(table),
        };
        Some(dashboard_base::products::at_hour::field_columns(table, fields))
    }

    fn valid_reply_through(frame: &DecodedFrame, _params: &Self::Params, at: &At<'_, Self::Context>, reply: &Self::Reply, _partial: bool) -> i32 {
        super::names_through(frame, reply.templates.iter().map(|t| t.template.as_str()), at.context)
            .min(dashboard_base::products::at_hour::warnings_through(frame.hour, reply.fielded_missing.iter().chain(&reply.warnings).map(String::as_str)))
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        Ok(Content::load(source, load))
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Division templates country is unavailable at this hour"))?;
        let mut refusals = Refusals::default();
        let reply = build(frame, params, at.holders.name(&params.country, at.hour), owner.slot, at.context, &mut refusals, at.cancelled)?;
        dashboard_base::products::check_reply_budget(&reply)?;
        Ok(reply)
    }
}

fn build(frame: &DecodedFrame, params: &DivisionTemplatesParams, tag: String, slot: u32, content: &Content, refusals: &mut Refusals, cancelled: &std::sync::atomic::AtomicBool) -> io::Result<DivisionTemplatesReply> {
    frame.complete::<tables::Countries>()?;
    frame.complete::<tables::CountryLinks>()?;
    frame.complete::<tables::DivisionTemplates>()?;
    let mut reply = DivisionTemplatesReply { tag, count_for: params.count_for, templates: Vec::new(), fielded_missing: None, warnings: Vec::new() };
    let (mut owned, unlinked) = owned(frame, slot);
    refusals.note_times(UNLINKED_TEMPLATE, unlinked);
    let counts = fielded(frame, slot, side(params.count_for));
    if let Err(why) = &counts {
        if why == UNLINKED_DIVISION {
            refusals.note(why.clone());
        }
        reply.fielded_missing = Some(why.clone());
    }
    let icons = frame.complete::<tables::DivisionTemplateIcons>().ok();
    let lexical = frame.complete::<tables::LexicalTokens>().ok();
    let settings_of = |handle: u64| find(icons, handle, |row| row.template);
    owned.sort_by_key(|&handle| {
        let ordinal = settings_of(handle).and_then(|icon| icon.settings.require("template settings").ok().cloned().flatten()).map(|settings| settings.roster_ordinal);
        (ordinal.is_none(), ordinal, handle)
    });
    for handle in owned {
        dashboard_text::metadata::check_cancel(cancelled)?;
        let (name, name_missing) = refusals.keep(content.template_name(frame, handle));
        let (icon, icon_missing) = refusals.keep(content.template_icon(frame, handle));
        let fielded = match &counts {
            Err(_) => None,
            Ok(counts) => match counts.get(&handle) {
                None => Some(0),
                Some(Recorded::Value(count)) => u32::try_from(*count).ok(),
                Some(Recorded::NotRecorded) => {
                    refusals.note(UNLINKED_DIVISION);
                    None
                }
            },
        };
        let mut entry = TemplateEntry {
            template: handle.to_string(),
            name,
            name_missing,
            icon,
            icon_missing,
            roster_ordinal: None,
            fielded,
            units: Vec::new(),
            units_missing: None,
            base_width_raw: None,
            priority: None,
            priority_name: None,
            priority_missing: None,
            locked: None,
            obsolete: None,
            obsolete_hour: None,
            designer: None,
            force_recruiting: None,
            new_equipment: None,
            foreign_equipment: None,
            manpower: None,
            training_days: None,
            settings_missing: None,
        };
        let Some(composition) = settings_of(handle) else {
            const UNREAD: &str = "Template composition was not read at this hour";
            entry.units_missing = Some(UNREAD.into());
            entry.settings_missing = Some(UNREAD.into());
            refusals.note(UNREAD);
            reply.templates.push(entry);
            continue;
        };
        entry.base_width_raw = composition.base_width.require("base width").ok().copied().flatten().map(|raw| raw.to_string());
        match composition.grids.require("template grids") {
            Ok(_) if composition.model_unreadable != 0 => {
                refusals.note(MODEL_UNREADABLE);
                entry.units_missing = Some(MODEL_UNREADABLE.into());
            }
            Ok(grids) => {
                let mut add = |cells: &[TemplateSubunit], support: bool| {
                    for unit in cells.iter().filter(|unit| unit.token != 0) {
                        let key = lexical.ok_or_else(|| "Sub-unit names unavailable: lexical tokens was not read".to_owned()).and_then(|rows| token_key(rows, unit.token));
                        let (name, name_missing) = refusals.keep(key.clone().and_then(|key| content.localised(&key)));
                        entry.units.push(TemplateUnit { key: key.ok(), name, name_missing, count: unit.count, support, local: unit.local != 0 });
                    }
                };
                add(&grids.regiments, false);
                add(&grids.regimental_support, false);
                add(&grids.support, true);
            }
            Err(why) => {
                refusals.note("Template grids were not recorded");
                entry.units_missing = Some(why);
            }
        }
        match composition.settings.require("template settings") {
            Ok(Some(settings)) => {
                entry.roster_ordinal = Some(settings.roster_ordinal);
                entry.priority = Some(settings.priority);
                (entry.priority_name, entry.priority_missing) = refusals.keep(priority_name(content, settings.priority));
                entry.locked = Some(settings.locked != 0);
                entry.obsolete = Some(settings.obsolete != 0);
                entry.obsolete_hour = (settings.obsolete != 0 && settings.obsolete_change_date != engine_native_memory::cgame_date::NULL_RAW_HOUR).then_some(settings.obsolete_change_date);
                entry.designer = Some(if settings.is_army_hq != 0 { DesignerKind::Headquarters } else { DesignerKind::Ordinary });
                entry.force_recruiting = Some(settings.force_allow_recruiting != 0);
                entry.new_equipment = Some(settings.allow_new_equipment != 0);
                entry.foreign_equipment = Some(settings.allow_foreign_equipment != 0);
                entry.manpower = Some(settings.total_manpower);
                entry.training_days = Some(settings.training_time);
            }
            Ok(None) => {
                const REFUSED: &str = "Template settings were refused at capture";
                refusals.note(REFUSED);
                entry.settings_missing = Some(REFUSED.into());
            }
            Err(why) => {
                refusals.note("Template settings were not recorded");
                entry.settings_missing = Some(why);
            }
        }
        reply.templates.push(entry);
    }
    reply.warnings = std::mem::take(refusals).lines();
    Ok(reply)
}

fn priority_name(content: &Content, priority: i32) -> Result<String, String> {
    let cat = content.cat.as_ref().map_err(|_| "Priority names need the recorded content stack".to_owned())?;
    let stages = cat.defines()?.int32("NCountry", "REINFORCEMENT_DIVISION_PRIORITY_COUNT").map_err(|why| format!("Template priority stages unavailable: {why}"))?;
    if !(0..=2).contains(&priority) || priority >= stages {
        return Err(format!("Template priority {priority} has no proved name"));
    }
    content.localised(&format!("TEMPLATE_PRIO_{priority}"))
}
