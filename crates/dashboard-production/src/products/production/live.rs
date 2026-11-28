use dashboard_world::products::equipment_names::{self as names, EquipmentIcon, Named, OrganisationName, VariantName};

use dashboard_base::products::Country;
use super::{
    DECIMALS, LineKind,
};
use dashboard_base::{Source, products::{LoadContext, check_reply_budget, at_hour::{At, AtHour, Keys, Owner, Read, ReadsAt, field_columns, warnings_through}}};
use crate::products::production::ProductionIndex;
use dashboard_follow::live::Source as Cell;
use dashboard_text::metadata::link_slot;
use replay_data::rows::{ProductionLineRow, TableId, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct ProductionLinesAt;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum LineOrder {
    #[default]
    Game,
    Factories,
    Output,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductionLinesAtParams {
    pub branch_id: String,
    pub country: Country,
    #[doc = " The game's queue order when absent."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub order: Option<LineOrder>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OutputPartNow {
    pub variant: String,
    #[doc = r#" The organisation's reference; "0" for none."#]
    pub mio: String,
    #[doc = " False when a tick could not be counted: the line's total is then not known."]
    pub exact: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OutputNow {
    pub parts: Vec<OutputPartNow>,
    pub since_hour: i32,
    pub whole_life: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct LineNow {
    pub handle: String,
    pub kind: Option<LineKind>,
    pub variant: String,
    pub mio: String,
    pub max_factories: i32,
    pub priority: i32,
    #[doc = " Its output rows; null where they are not recorded at the hour."]
    pub output: Option<OutputNow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProductionLinesAtReply {
    pub branch_id: String,
    pub tag: String,
    pub decimals: u8,
    #[doc = " The country's lines in the asked order; empty with `missing` where they are not known."]
    pub lines: Vec<LineNow>,
    #[doc = " Why the lines are not known at the hour, when they are not: never shown as none."]
    pub missing: Option<String>,
    #[doc = " Why the lines' output is not known at the hour, when it is not."]
    pub output_missing: Option<String>,
    pub variants: Vec<VariantName>,
    pub organisations: Vec<OrganisationName>,
    pub icons: Vec<EquipmentIcon>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(ProductionLinesAtReply);

pub struct LinesContext {
    naming: names::Context,
    index: ProductionIndex,
}

fn owned<'a>(held: &'a DecodedFrame, owner: Owner) -> io::Result<Vec<&'a ProductionLineRow>> {
    let links = held.complete::<tables::CountryLinks>()?;
    let mut lines: Vec<_> = held.complete::<tables::ProductionLines>()?.iter().filter(|line| link_slot(links, line.country) == Some(owner.slot)).collect();
    lines.sort_by_key(|line| (line.priority, line.handle));
    Ok(lines)
}

fn ordered<'a>(mut lines: Vec<&'a ProductionLineRow>, order: LineOrder) -> Vec<&'a ProductionLineRow> {
    match order {
        LineOrder::Game => {}
        LineOrder::Factories => lines.sort_by(|a, b| b.active_factories.cmp(&a.active_factories)),
        LineOrder::Output => lines.sort_by(|a, b| b.speed_raw.cmp(&a.speed_raw)),
    }
    lines
}

fn line_reads(at: &ReadsAt<'_, LinesContext>, held: &DecodedFrame, owner: Owner) -> (Vec<Read>, i32) {
    let (Ok(links), Ok(index)) = (held.complete::<tables::CountryLinks>(), at.context.index.run(at.run)) else {
        return (vec![Read::all(TableId::ProductionLines), Read::all(TableId::ProductionLineOutput)], at.hour);
    };
    let mut through = i32::MAX;
    let mut handles = BTreeSet::new();
    for life in &index.lines {
        if life.last < at.hour || link_slot(links, life.country) != Some(owner.slot) { continue; }
        if life.first > at.hour {
            through = through.min(life.first - 1);
        } else {
            through = through.min(life.last);
            handles.insert(life.handle);
        }
    }
    let lines = Read::these(TableId::ProductionLines, handles.iter().copied().map(u128::from));
    let output = Keys::Ranges(handles.into_iter().map(|handle| (u128::from(handle) << 32, u128::from(handle) << 32 | u128::from(u32::MAX))).collect());
    (vec![lines, Read { table: TableId::ProductionLineOutput, keys: output }], through)
}

impl AtHour for ProductionLinesAt {
    const KEY: &'static str = "production-lines-at";
    const TITLE: &'static str = "Production lines";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    type Params = ProductionLinesAtParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, order];
    type Reply = ProductionLinesAtReply;
    type Context = LinesContext;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            ProductionLinesAtParams::decl(),
            ProductionLinesAtReply::decl(),
            LineOrder::decl(),
            LineKind::decl(),
            OutputPartNow::decl(),
            OutputNow::decl(),
            LineNow::decl(),
            VariantName::decl(),
            OrganisationName::decl(),
            EquipmentIcon::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        if params.branch_id.parse::<u64>().is_err() {
            return Err("Invalid branch");
        }
        dashboard_base::products::validate_country(&params.country)
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }



    fn projection(params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> {
        let fields: &[&str] = match table {
            TableId::ProductionLines => {
                let mut columns = field_columns(table, &["country", "kind", "variant", "mio", "max_factories", "priority"]);
                let sort = match params.order.unwrap_or_default() {
                    LineOrder::Game => None,
                    LineOrder::Factories => Some("active_factories"),
                    LineOrder::Output => Some("speed_raw"),
                };
                if let Some(sort) = sort { columns.extend(field_columns(table, &[sort])); }
                return Some(columns);
            }
            TableId::ProductionLineOutput => &["variant", "mio", "since_hour", "observed_since_creation", "ended", "exact", "purpose"],
            TableId::Countries => &["canonical_slot", "tag"],
            TableId::CountryAppearance => &["cosmetic_tag", "name_tag", "graphical_culture", "graphical_culture_2d", "original_tag", "original_tag_reference"],
            TableId::ReservedCountryNaming => &["original_tag", "original_tag_reference", "dynamic_tag_boundary"],
            TableId::PoliticalParties => &["is_ruling", "token"],
            TableId::Politics => &["capital", "original_capital"],
            TableId::States => &["continent"],
            TableId::Factions => &[],
            _ => return None,
        };
        Some(field_columns(table, fields))
    }

    fn plan_changes(stage: usize, edits: &dashboard_base::products::at_hour::Edits, _held: &DecodedFrame, _at: &dashboard_base::products::at_hour::ReadsAt<'_, Self::Context>) -> bool {
        match stage {
            1 => edits.fields(TableId::CountryLinks, u64::MAX),
            2 => edits.fields(TableId::CountryLinks, u64::MAX)
                || edits.fields(TableId::Politics, dashboard_base::products::at_hour::field_mask(TableId::Politics, &["capital", "original_capital"]))
                || edits.fields(TableId::ProductionLines, dashboard_base::products::at_hour::field_mask(TableId::ProductionLines, &["variant", "mio", "country"]))
                || edits.fields(TableId::ProductionLineOutput, dashboard_base::products::at_hour::field_mask(TableId::ProductionLineOutput, &["variant", "mio", "ended"])),
            3 => [TableId::VariantDefinitions, TableId::IndustrialOrganisations, TableId::States].into_iter().any(|t| edits.fields(t, u64::MAX)),
            _ => false,
        }
    }

    fn valid_reply_through(frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>, reply: &Self::Reply, partial: bool) -> i32 {
        if partial { return frame.hour; }
        warnings_through(frame.hour, reply.warnings.iter().chain(&reply.missing).chain(&reply.output_missing)
            .map(String::as_str)
            .chain(reply.variants.iter().flat_map(|name| [name.name_unknown.as_deref(), name.icon_unknown.as_deref()]).flatten())
            .chain(reply.organisations.iter().flat_map(|name| [name.name_unknown.as_deref(), name.icon_unknown.as_deref()]).flatten()))
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        let mut tables = vec![TableId::ProductionLines, TableId::ProductionLineOutput];
        tables.extend_from_slice(names::TABLES);
        tables
    }

    fn retained_reads(params: &Self::Params, owner: Option<Owner>, stage: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> (Vec<Read>, i32) {
        if stage == 1 && let Some(owner) = owner { return line_reads(at, held, owner); }
        (Self::reads_in(params, owner, stage, held, at), i32::MAX)
    }

    fn reads_in(_params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        let Some(owner) = owner else { return Vec::new() };
        match step {
            0 => names::WHOLE.iter().map(|&table| Read::all(table)).collect(),
            1 => line_reads(at, held, owner).0,
            2 => {
                let lines = owned(held, owner).unwrap_or_default();
                let handles: BTreeSet<u64> = lines.iter().map(|line| line.handle).collect();
                let mut variants: BTreeSet<u64> = lines.iter().map(|line| line.variant).collect();
                let mut mios: BTreeSet<u64> = lines.iter().map(|line| line.mio).filter(|&mio| mio != 0).collect();
                for row in held.complete::<tables::ProductionLineOutput>().ok().into_iter().flatten().filter(|row| handles.contains(&row.line) && !row.ended) {
                    variants.insert(row.variant);
                    if row.mio != 0 {
                        mios.insert(row.mio);
                    }
                }
                vec![
                    Read::these(TableId::VariantDefinitions, variants.into_iter().map(u128::from)),
                    Read::these(TableId::IndustrialOrganisations, mios.into_iter().map(u128::from)),
                    names::capital_state_read(held),
                ]
            }
            3 => {
                let mut tokens: BTreeSet<u32> = names::variant_tokens(held).collect();
                tokens.extend(held.complete::<tables::IndustrialOrganisations>().ok().into_iter().flatten().map(|row| row.token));
                names::spelling_reads(held, tokens, true)
            }
            _ => Vec::new(),
        }
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        Ok(LinesContext { naming: names::context(source, load)?, index: ProductionIndex::of(source) })
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Production lines country is unavailable at this hour"))?;
        let mut reply = ProductionLinesAtReply {
            branch_id: params.branch_id.clone(),
            tag: at.holders.name(&params.country, at.hour),
            decimals: DECIMALS,
            lines: Vec::new(),
            missing: None,
            output_missing: None,
            variants: Vec::new(),
            organisations: Vec::new(),
            icons: Vec::new(),
            warnings: Vec::new(),
        };
        let lines = match owned(frame, owner) {
            Ok(lines) => ordered(lines, params.order.unwrap_or_default()),
            Err(error) => {
                reply.missing = Some(format!("Production lines are not recorded at this hour: {error}"));
                return Ok(reply);
            }
        };
        let output = frame.complete::<tables::ProductionLineOutput>();
        if let Err(error) = &output {
            reply.output_missing = Some(format!("Units made are not recorded at this hour: {error}"));
        }
        let field = |table, name| dashboard_declare::field(table, name);
        let line_table = TableId::ProductionLines;
        let columns = (|| -> io::Result<_> {
            Ok((
                [field(line_table, "active_factories")?, field(line_table, "requested_factories")?, field(line_table, "efficiency_raw")?, field(line_table, "speed_raw")?, field(line_table, "cost_raw")?],
                field(TableId::ProductionLineOutput, "units")?,
            ))
        })();
        if let Err(error) = &columns {
            reply.warnings.push(format!("Line values cannot be read: {error}"));
        }
        let mut variants = BTreeSet::new();
        let mut mios = BTreeSet::new();
        for line in &lines {
            variants.insert(line.variant);
            if line.mio != 0 {
                mios.insert(line.mio);
            }
            let rows: Vec<_> = output.as_ref().map(|rows| {
                let first = rows.partition_point(|row| row.line < line.handle);
                rows[first..].iter().take_while(|row| row.line == line.handle).filter(|row| !row.ended).collect()
            }).unwrap_or_default();
            for row in &rows {
                variants.insert(row.variant);
                if row.mio != 0 {
                    mios.insert(row.mio);
                }
            }
            let handle = line.handle.to_string();
            if let Ok((line_columns, units)) = &columns {
                for (name, &column) in ["active", "requested", "efficiency", "speed", "cost"].into_iter().zip(line_columns) {
                    at.live.value(&handle, name, Cell { table: line_table, column, key: u128::from(line.handle) }, 1.0);
                }
                if output.is_ok() && !rows.is_empty() {
                    let parts: Vec<Cell> = rows.iter().map(|row| Cell { table: TableId::ProductionLineOutput, column: *units, key: u128::from(row.line) << 32 | u128::from(row.ordinal) }).collect();
                    at.live.sum(&handle, "output", &parts, 1.0);
                }
            }
            reply.lines.push(LineNow {
                handle,
                kind: LineKind::of(i64::from(line.kind)),
                variant: line.variant.to_string(),
                mio: line.mio.to_string(),
                max_factories: line.max_factories,
                priority: line.priority,
                output: (output.is_ok() && !rows.is_empty()).then(|| OutputNow {
                    since_hour: rows.iter().map(|row| row.since_hour).min().unwrap_or(frame.hour),
                    whole_life: rows.iter().all(|row| row.observed_since_creation),
                    parts: rows.iter().map(|row| OutputPartNow { variant: row.variant.to_string(), mio: row.mio.to_string(), exact: row.exact }).collect(),
                }),
            });
        }
        let mut named = Named::default();
        let variants: Vec<u64> = variants.into_iter().collect();
        let mios: Vec<u64> = mios.into_iter().collect();
        names::name(frame, &variants, &mios, &at.context.naming, &mut named);
        reply.warnings.extend(std::mem::take(&mut named.warnings));
        reply.icons = named.icons();
        reply.variants = named.variants.into_values().map(|variant| VariantName { hour: None, ..variant }).collect();
        reply.organisations = named.organisations.into_values().map(|organisation| OrganisationName { hour: None, ..organisation }).collect();
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}
