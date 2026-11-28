use super::spending::{ConstructionSpendingParams, Facts, facts, place_buildings};
use super::{COST_SCALE, SpendingGap};
use dashboard_base::products::breakdown::{self, Amount, Breakdown, BreakdownPart, Dated, Exact, History, HistoryBuilder, Node, counted};
use dashboard_base::products::{Country, LoadContext, Product, check_reply_budget, validate_country};
use dashboard_base::Source;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, sync::atomic::AtomicBool};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConstructionBreakdownParams {
    pub branch_id: String,
    pub country: Country,
    pub hour: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ConstructionBreakdownReply {
    #[doc = " The branch's first hour: spending is counted from it."]
    pub from: i32,
    pub hour: i32,
    #[doc = " Everything in the reply stands unchanged from `hour` through this hour."]
    pub through: i32,
    pub breakdown: Breakdown,
    #[doc = " Projects read by `hour` whose output before observation began is not counted."]
    pub before_first_read: u32,
    #[doc = " Projects read by `hour` whose output no country can be given, not counted."]
    pub unattributed: u32,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(ConstructionBreakdownReply);

pub struct ConstructionBreakdown;

impl Product for ConstructionBreakdown {
    const KEY: &'static str = "construction-breakdown";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<dashboard_base::products::read_ahead::Prepare<Self::Params>> =
        Some(|source, _, load| super::prepare_naming(source, load));
    type Params = ConstructionBreakdownParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, hour: End];
    type Reply = ConstructionBreakdownReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![ConstructionBreakdownParams::decl(), ConstructionBreakdownReply::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid branch")?;
        validate_country(&params.country)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let query = dashboard_base::queries::query(source, params.branch_id.parse().map_err(|_| dashboard_core::invalid("Invalid branch"))?)?;
        let whole = ConstructionSpendingParams { branch_id: params.branch_id.clone(), country: params.country.clone() };
        let key = (whole, query.timeline.first, query.timeline.last);
        let held = breakdown::held(source, context, key, || facts(source, &query, &params.country, context, cancelled).map(|facts| index(&facts)))?;
        let reply = held.at(params.hour);
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}

pub(super) struct ConstructionHistory {
    from: i32,
    history: History,
    names: HashMap<u32, Dated<String>>,
    before_first_read: Vec<i32>,
    unattributed: Vec<i32>,
    warnings: Dated<Vec<String>>,
}

fn relabel(parts: &mut [BreakdownPart], names: &HashMap<u32, Dated<String>>, hour: i32) {
    for part in parts {
        for child in &mut part.children {
            if let Some(id) = child.key.strip_prefix("type:").and_then(|id| id.parse::<u32>().ok())
                && let Some(name) = names.get(&id).and_then(|steps| steps.at(hour))
            {
                child.name = name.clone();
            }
        }
    }
}

impl ConstructionHistory {
    pub(super) fn at(&self, hour: i32) -> ConstructionBreakdownReply {
        let (mut breakdown, through) = self.history.at(hour);
        relabel(&mut breakdown.parts, &self.names, hour);
        ConstructionBreakdownReply {
            from: self.from,
            hour,
            through,
            breakdown,
            before_first_read: counted(&self.before_first_read, hour),
            unattributed: counted(&self.unattributed, hour),
            warnings: self.warnings.at(hour).cloned().unwrap_or_default(),
        }
    }
}

fn gap_reason(gap: SpendingGap) -> &'static str {
    match gap {
        SpendingGap::NotRecorded => "Some output was not recorded",
        SpendingGap::LinkedOutput => "Some output was shared with another project and not recorded exactly",
        SpendingGap::InexactOutput => "Some output was not recorded exactly",
        SpendingGap::ObservationRestart => "Observation restarted, so some output was not counted",
        SpendingGap::FinalOutputMissing => "A project's final output was not recorded",
        SpendingGap::OwnerUnknown => "Some output was not traced to a country",
    }
}

type Place = (bool, bool, u32, u32, u32);

pub(super) fn index(facts: &Facts) -> ConstructionHistory {
    let mut events: Vec<(i32, Place, Amount)> = Vec::new();
    for (&place, steps) in &facts.spent {
        let mut held = 0_i128;
        for step in steps {
            match step.total_raw.as_deref().and_then(|raw| raw.parse::<i128>().ok()) {
                Some(total) if let Some(spent) = Exact::new(total - held, i128::from(COST_SCALE)) => {
                    events.push((step.known_at, place, Amount::Known(spent)));
                    held = total;
                }
                _ => {
                    events.push((step.known_at, place, Amount::NotRecorded("Spending was not recorded at this hour".into())));
                    break;
                }
            }
        }
    }
    let buildings = place_buildings(&facts.spent, &facts.lines);
    let mut early_gaps: Vec<(i32, SpendingGap)> = Vec::new();
    for line in &facts.lines {
        let (railway, building, state, to_state) = line.leaf;
        for &(hour, _, gap, kind) in &line.gaps {
            if !kind.new && !kind.repair {
                early_gaps.push((hour, gap));
                continue;
            }
            for (repair, active) in [(false, kind.new), (true, kind.repair)] {
                if active {
                    events.push((hour, (repair, railway, building, state, to_state), Amount::Partial(Exact::ZERO, gap_reason(gap).into())));
                }
            }
        }
    }
    events.sort_by_key(|event| (event.0, event.1));
    let mut ordinals: HashMap<Option<u32>, u32> = HashMap::new();
    for (_, place, _) in &events {
        let building = buildings.get(place).copied().flatten();
        let next = ordinals.len() as u32;
        ordinals.entry(building).or_insert(next);
    }
    let names: HashMap<u32, Dated<String>> = facts.names.iter().map(|(&id, steps)| (id, Dated::new(steps.clone()))).collect();
    let warnings = Dated::new(facts.warnings.clone());
    let mut built = HistoryBuilder::new(facts.from, facts.to);
    for (hour, place, amount) in events {
        let building = buildings.get(&place).copied().flatten();
        let purpose = if place.0 { Node::new("repair", "Repairs", 1) } else { Node::new("new", "New buildings", 0) };
        let name = building
            .and_then(|id| names.get(&id).and_then(|steps| steps.at(hour)).cloned())
            .unwrap_or_else(|| "Building name unavailable".into());
        let kind = Node::new(format!("type:{}", building.map_or_else(|| "none".to_owned(), |id| id.to_string())), name, ordinals[&building]);
        let leaf = built.leaf(vec![purpose, kind]);
        built.add(hour, leaf, amount, None);
    }
    for (hour, gap) in early_gaps {
        built.qualify(hour, gap_reason(gap));
        built.change(hour);
    }
    for hour in names.values().flat_map(Dated::hours).chain(warnings.hours()) {
        built.change(hour);
    }
    let mut unattributed = facts.unattributed.clone();
    unattributed.sort_unstable();
    for &hour in &unattributed {
        built.qualify(hour, gap_reason(SpendingGap::OwnerUnknown));
        built.change(hour);
    }
    let mut before_first_read: Vec<i32> = facts.lines.iter().filter_map(|line| line.before_first_read).collect();
    before_first_read.sort_unstable();
    for &hour in &before_first_read {
        built.change(hour);
    }
    for (first, last, reason) in &facts.unrecorded {
        built.unrecorded(*first, *last, reason.clone());
    }
    ConstructionHistory { from: facts.from, history: built.build(), names, before_first_read, unattributed, warnings }
}
