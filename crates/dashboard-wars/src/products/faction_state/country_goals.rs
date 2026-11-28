use super::details::{self, DetailContent, FactionDetailParams, FactionDetailReply, FactionDetails};
use super::goals::{self, FactionGoalDefinition};
use dashboard_base::products::Country;
use dashboard_base::{Source, products::{LoadContext, at_hour::{At, AtHour, DateSelection, Edits, Held, Owner, PrepareHour, Read, ReadDates, ReadsAt, ScriptedInputs, field_columns, warnings_through}}};
use dashboard_text::metadata::link_slot;
use replay_data::rows::{CountryLinkRow, FactionMemberRow, ProvinceRow, TableId, mask, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::io;
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct CountryFactionGoals;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryFactionGoalsParams {
    pub branch_id: String,
    pub country: Country,
    pub goals: GoalFilter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum GoalFilter {
    All,
    InProgress,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum GoalState {
    InProgress,
    Completing,
    Cancelling,
    Completed,
    Cancelled,
    #[doc = " A native state this version does not know; shown as such, never as in progress."]
    Unrecognised,
}

impl GoalState {
    pub(super) fn of(status: i32) -> Self {
        match status {
            0 => Self::InProgress,
            1 => Self::Completed,
            2 => Self::Cancelled,
            3 => Self::Completing,
            4 => Self::Cancelling,
            _ => Self::Unrecognised,
        }
    }

    pub(super) fn shown(self, filter: GoalFilter) -> bool {
        match filter {
            GoalFilter::All => true,
            GoalFilter::InProgress => matches!(self, Self::InProgress | Self::Completing | Self::Cancelling),
            GoalFilter::Completed => self == Self::Completed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CountryGoal {
    pub key: String,
    #[doc = " An index into the reply's `definitions`."]
    pub definition: u32,
    pub state: GoalState,
    #[doc = " The held goal's ordinal, the row of its live progress cell `goal:<ordinal>`; none for a goal in the completed or cancelled lists."]
    pub ordinal: Option<u32>,
    #[doc = " The native state, where it is `unrecognised`."]
    pub unrecognised_status: Option<i32>,
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
pub struct CountryFactionGoalsReply {
    #[serde(skip)]
    #[cfg_attr(any(test, feature = "export-types"), ts(skip))]
    through: Through,
    #[doc = " The faction the country belongs to at the hour; none when it belongs to none, or when"]
    #[doc = " `membership_missing` says membership is not known."]
    pub faction: Option<String>,
    #[doc = " The goals the filter shows, held goals first in the game's order, then completed, then cancelled."]
    pub goals: Vec<CountryGoal>,
    #[doc = " The definitions the goals name, once each."]
    pub definitions: Vec<FactionGoalDefinition>,
    #[doc = " Why the faction's goal lists are not known at the hour; the goals are then empty."]
    pub goal_reason: Option<String>,
    pub membership_missing: Option<String>,
    pub warnings: Vec<String>,
}

pub(super) fn shown_goals(detail: FactionDetailReply) -> (Vec<CountryGoal>, Vec<FactionGoalDefinition>, Vec<String>) {
    let held = detail.goals.iter().map(|goal| {
        let state = GoalState::of(goal.status);
        CountryGoal { key: format!("goal:{}", goal.ordinal), definition: goal.definition, state, ordinal: Some(goal.ordinal), unrecognised_status: (state == GoalState::Unrecognised).then_some(goal.status) }
    });
    let listed = |list: &[u32], name: &'static str, state: GoalState| {
        list.iter().enumerate().map(move |(at, &definition)| CountryGoal { key: format!("{name}:{at}"), definition, state, ordinal: None, unrecognised_status: None }).collect::<Vec<_>>()
    };
    let goals = held.chain(listed(&detail.completed, "completed", GoalState::Completed)).chain(listed(&detail.cancelled, "cancelled", GoalState::Cancelled)).collect();
    (goals, detail.definitions, detail.warnings)
}

dashboard_base::reply_warnings!(CountryFactionGoalsReply);

const MEMBERSHIP: [TableId; 2] = [TableId::FactionMembers, TableId::CountryLinks];

const fn all_dates(table: TableId) -> DateSelection {
    DateSelection { table, fields: table.full_mask(), shift: None }
}

const DATE_SELECTIONS: &[DateSelection] = &[
    DateSelection { table: TableId::CountryLinks, fields: mask::<CountryLinkRow>(&["canonical_slot"]), shift: None },
    DateSelection { table: TableId::Provinces, fields: mask::<ProvinceRow>(&[]), shift: None },
    all_dates(TableId::Countries),
    all_dates(TableId::States),
    all_dates(TableId::FactionMembers),
    all_dates(TableId::Politics),
    all_dates(TableId::PoliticalParties),
    all_dates(TableId::Autonomy),
    all_dates(TableId::CountryAppearance),
    all_dates(TableId::CountryCounterInputs),
    all_dates(TableId::GlobalSavedEventTargets),
    all_dates(TableId::ReservedCountryNaming),
    all_dates(TableId::CountryWars),
];

fn blank(missing: Option<String>) -> CountryFactionGoalsReply {
    CountryFactionGoalsReply { through: Through(i32::MAX), faction: None, goals: Vec::new(), definitions: Vec::new(), goal_reason: None, membership_missing: missing, warnings: Vec::new() }
}

pub(super) fn faction_of(members: &[FactionMemberRow], links: &[CountryLinkRow], slot: u32) -> Option<u64> {
    members.iter().find(|member| member.faction != 0 && link_slot(links, member.country) == Some(slot)).map(|member| member.faction)
}

fn membership(frame: &DecodedFrame, owner: Owner) -> Result<Option<u64>, String> {
    let members = frame.complete::<tables::FactionMembers>().map_err(|why| format!("Faction memberships are not known at this hour: {why}"))?;
    let links = frame.complete::<tables::CountryLinks>().map_err(|why| format!("Country links are not known at this hour: {why}"))?;
    Ok(faction_of(members, links, owner.slot))
}

fn detail_params(params: &CountryFactionGoalsParams, faction: u64) -> FactionDetailParams {
    FactionDetailParams { branch_id: params.branch_id.clone(), faction: faction.to_string(), viewpoint: Some(params.country.clone()) }
}

fn scripted_tables() -> Vec<TableId> {
    let mut tables: Vec<TableId> = goals::text_tables().chain([TableId::LexicalTokens, TableId::Countries, TableId::CountryLinks]).collect();
    tables.sort_unstable();
    tables.dedup();
    tables
}

impl AtHour for CountryFactionGoals {
    const KEY: &'static str = "country-faction-goals";
    const TITLE: &'static str = "Country faction goals";
    const CONTENT_DEPENDENT: bool = true;
    const RESET_READERS: Option<fn(&Self::Context)> = Some(|context| context.inputs().release_world());
    const RELEASE_ON_GAP: Option<fn(&Source)> = Some(ScriptedInputs::release_world_for);
    const PREPARE: Option<PrepareHour<Self::Params>> = Some(|source, params, load, hour| {
        if load.content.is_none() { return Ok(None); }
        let branch = params.branch_id.parse().map_err(|_| dashboard_core::invalid("Invalid faction branch"))?;
        let query = dashboard_base::queries::query(source, branch)?;
        query.timeline.at(hour).map_or(Ok(None), |span| ReadDates::prepare_block_dates(source, span.run, DATE_SELECTIONS))
    });
    type Params = CountryFactionGoalsParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, goals];
    type Reply = CountryFactionGoalsReply;
    type Context = DetailContent;

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid faction branch")?;
        dashboard_base::products::validate_country(&params.country)
    }

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![CountryFactionGoalsParams::decl(), CountryFactionGoalsReply::decl(), GoalFilter::decl(), GoalState::decl(), CountryGoal::decl()]
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        let mut tables: Vec<TableId> = details::country_goal_reads(0)
            .into_iter()
            .map(|read| read.table)
            .chain(MEMBERSHIP)
            .chain([TableId::LexicalTokens])
            .collect();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    fn reads(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        match (step, owner) {
            (0, Some(_)) => MEMBERSHIP.map(Read::all).into(),
            (1, Some(owner)) => match membership(held, owner).ok().flatten() {
                Some(faction) => details::country_goal_reads(faction),
                None => [TableId::FactionGoalStatus, TableId::FactionGoalEntries]
                    .into_iter()
                    .map(|table| Read::these(table, []))
                    .collect(),
            },
            (2, Some(owner)) => {
                let tokens = membership(held, owner)
                    .ok()
                    .flatten()
                    .map(|faction| goals::shown_tokens(held, faction, params.goals))
                    .unwrap_or_default();
                vec![Read::these(TableId::LexicalTokens, tokens)]
            }
            _ => Vec::new(),
        }
    }

    fn reads_in(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        if step == 3 {
            let Some(owner) = owner else { return Vec::new() };
            let Some(faction) = membership(held, owner).ok().flatten() else { return Vec::new() };
            if at.context.catalogue().is_ok_and(|cat| goals::needs_lexicon_lookup(held, faction, cat, params.goals)) {
                return vec![Read::all(TableId::LexicalTokens)];
            }
            return Vec::new();
        }
        Self::reads(params, owner, step, held)
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        match table {
            TableId::FactionMembers => Some(field_columns(table, &["faction"])),
            TableId::CountryLinks => Some(field_columns(table, &["canonical_slot"])),
            TableId::FactionGoalStatus => Some(Vec::new()),
            TableId::FactionGoalEntries => Some(field_columns(table, &["token", "status", "progress_raw"])),
            _ => None,
        }
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> {
        match table {
            TableId::FactionGoalEntries => Some(field_columns(table, &["token", "status"])),
            _ => Self::columns(table),
        }
    }

    fn plan_changes(stage: usize, edits: &Edits, _held: &DecodedFrame, _at: &ReadsAt<'_, Self::Context>) -> bool {
        let touched = |tables: &[TableId]| edits.replaced.iter().chain(edits.tables.keys()).any(|table| tables.contains(table));
        match stage {
            0 => false,
            1 => touched(&MEMBERSHIP),
            2 => touched(&[TableId::FactionMembers, TableId::CountryLinks, TableId::FactionGoalEntries]),
            _ => touched(&[TableId::FactionMembers, TableId::CountryLinks, TableId::FactionGoalEntries, TableId::LexicalTokens]),
        }
    }

    fn advance(frame: &DecodedFrame, _params: &Self::Params, at: &At<'_, Self::Context>, edits: &Edits, _live: &dashboard_follow::live::Live, data: &mut Held<'_>) -> io::Result<bool> {
        if !edits.replaced.is_empty() || !edits.tables.keys().all(|table| MEMBERSHIP.contains(table)) || !data["membershipMissing"].is_null() {
            return Ok(false);
        }
        let Some(owner) = at.owner else { return Ok(false) };
        Ok(match membership(frame, owner) {
            Ok(Some(faction)) => data["faction"].as_str() == Some(faction.to_string().as_str()),
            Ok(None) => data["faction"].is_null(),
            Err(_) => false,
        })
    }

    fn valid_reply_through(frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>, reply: &Self::Reply, partial: bool) -> i32 {
        if partial {
            return frame.hour;
        }
        reply.through.0.min(warnings_through(frame.hour, reply.warnings.iter().map(String::as_str).chain(reply.goal_reason.as_deref()).chain(reply.membership_missing.as_deref())))
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        <FactionDetails as AtHour>::context(source, load).map(|context| context.with_scripted_playback(source, DATE_SELECTIONS))
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Faction goals country is unavailable at this hour"))?;
        let reply = match membership(frame, owner) {
            Err(why) => blank(Some(why)),
            Ok(None) => blank(None),
            Ok(Some(faction)) => {
                let asked = detail_params(params, faction);
                let (detail, through) = if at.context.catalogue().is_ok_and(|cat| goals::scripted_for(frame, faction, cat, params.goals)) {
                    let (detail, through) = at.context.inputs().evaluate_with_reads(frame, &scripted_tables(), at.cancelled, |world| {
                        (
                            details::goals_only_in(frame, world, &asked, faction, params.goals, at),
                            vec![dashboard_text::triggers::inputs::TableRead::fields(TableId::Countries, &["canonical_slot", "tag"])],
                        )
                    })?;
                    (detail?, through)
                } else {
                    (details::goals_only_in(frame, frame, &asked, faction, params.goals, at)?, i32::MAX)
                };
                let goal_reason = detail.goal_reason.clone();
                let (goals, definitions, warnings) = shown_goals(detail);
                CountryFactionGoalsReply { through: Through(through), faction: Some(faction.to_string()), goals, definitions, goal_reason, membership_missing: None, warnings }
            }
        };
        dashboard_base::products::check_reply_budget(&reply)?;
        Ok(reply)
    }
}
