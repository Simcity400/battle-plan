use super::{country_goals, details, history};
use dashboard_follow::live::{CellValue, Cells};
use replay_data::rows::{
    CountryLinkRow, FactionGoalEntryRow, FactionGoalStatusRow, FactionManpowerRow, FactionMemberRow, LexicalTokenRow, ReservedCountryNamingRow,
    TableId, TableRows,
};
use replay_frame::frame::{DecodedFrame, Metrics, TableView, not_selected};
use std::collections::HashMap;

const FACTION: u64 = 12_884_901_976;
const OTHER: u64 = 4_294_967_384;

fn frame() -> DecodedFrame {
    DecodedFrame { branch: 1, hour: 100, tables: std::array::from_fn(|_| not_selected()), metrics: Metrics::default() }
}

fn set(frame: &mut DecodedFrame, table: TableId, rows: TableRows) {
    *frame.tables[table.index()] = TableView::Complete(rows);
}

fn goal(faction: u64, kind: u8, ordinal: u32, token: Option<u32>, progress_raw: i64) -> FactionGoalEntryRow {
    FactionGoalEntryRow { faction, kind, ordinal, token, status: 0, progress_raw, active_sections: vec![], date: 0, slots: 0 }
}

fn goal_frame(entries: Vec<FactionGoalEntryRow>) -> DecodedFrame {
    let mut frame = frame();
    set(&mut frame, TableId::FactionGoalStatus, TableRows::FactionGoalStatus(vec![FactionGoalStatusRow { faction: OTHER, manifest: None }, FactionGoalStatusRow { faction: FACTION, manifest: Some(1) }]));
    set(&mut frame, TableId::FactionGoalEntries, TableRows::FactionGoalEntries(entries));
    set(
        &mut frame,
        TableId::LexicalTokens,
        TableRows::LexicalTokens(["mod_manifest", "mod_goal", "mod_done"].into_iter().zip(1..).map(|(literal, token)| LexicalTokenRow { token, literal: literal.as_bytes().to_vec() }).collect()),
    );
    frame
}

fn params() -> details::FactionDetailParams {
    details::FactionDetailParams { branch_id: "1".into(), faction: FACTION.to_string(), viewpoint: None }
}

#[test]
fn a_factions_goal_lists_name_each_goal_once_and_follow_progress_by_its_own_rows() {
    let frame = goal_frame(vec![
        goal(OTHER, 0, 0, Some(2), 7),
        goal(FACTION, 0, 0, Some(2), 25_000),
        goal(FACTION, 0, 1, Some(1), 50_000),
        goal(FACTION, 1, 0, Some(3), 0),
        goal(FACTION, 1, 1, None, 0),
        goal(FACTION, 2, 0, Some(2), 0),
        goal(FACTION, 3, 0, None, 0),
    ]);
    let mut reply = details::blank(&params());
    let cells = Cells::default();
    details::goal_lists(&frame, FACTION, None, None, &mut reply, &cells, None).unwrap();
    assert_eq!(reply.goal_reason, None);
    let tokens: Vec<Option<&str>> = reply.definitions.iter().map(|definition| definition.token.as_deref()).collect();
    assert_eq!(tokens, [Some("mod_manifest"), Some("mod_goal"), Some("mod_done"), None], "the manifest first, then each goal once");
    assert_eq!(reply.manifest, Some(0));
    assert_eq!(reply.goals.iter().map(|goal| (goal.ordinal, goal.definition)).collect::<Vec<_>>(), [(0, 1), (1, 0)]);
    assert_eq!(reply.completed, [2, 3]);
    assert_eq!(reply.cancelled, [1], "a cancelled goal shares the held goal's definition");
    assert!(reply.definitions[1].reason.as_deref().is_some_and(|why| why.contains("recorded game folders")), "no content: named by token, why said");
    let live = cells.finish();
    let json = live.json(&live.values(&frame).into_iter().map(dashboard_follow::live::GroupState::Values).collect::<Vec<_>>());
    assert_eq!(json["rows"], serde_json::json!(["goal:0", "goal:1"]));
    let key = |ordinal: u32| (u128::from(FACTION) << 40 | u128::from(ordinal)).to_string();
    assert_eq!(json["groups"][0]["keys"], serde_json::json!([key(0), key(1)]));
    assert_eq!(live.values(&frame)[0], [CellValue::Number(25_000), CellValue::Number(50_000)]);
}

#[test]
fn goal_lists_that_skip_an_ordinal_or_lack_their_status_are_not_known() {
    let mut reply = details::blank(&params());
    let cells = Cells::default();
    details::goal_lists(&goal_frame(vec![goal(FACTION, 0, 0, Some(2), 0), goal(FACTION, 0, 2, Some(1), 0)]), FACTION, None, None, &mut reply, &cells, None).unwrap();
    assert_eq!(reply.goal_reason.as_deref(), Some("Faction goal ordinals are not contiguous"));
    assert!(reply.goals.is_empty() && reply.definitions.is_empty());
    assert!(cells.finish().is_empty());
    let mut missing = goal_frame(vec![]);
    set(&mut missing, TableId::FactionGoalStatus, TableRows::FactionGoalStatus(vec![FactionGoalStatusRow { faction: OTHER, manifest: None }]));
    let mut reply = details::blank(&params());
    details::goal_lists(&missing, FACTION, None, None, &mut reply, &Cells::default(), None).unwrap();
    assert_eq!(reply.goal_reason.as_deref(), Some("Faction goal status is absent"));
    let mut unread = goal_frame(vec![]);
    *unread.tables[TableId::FactionGoalEntries.index()] = TableView::Unavailable { reason: "writer not observed".into() };
    let mut reply = details::blank(&params());
    details::goal_lists(&unread, FACTION, None, None, &mut reply, &Cells::default(), None).unwrap();
    assert!(reply.goal_reason.unwrap().contains("writer not observed"));
}

#[test]
fn manpower_contributions_are_each_occurrence_named_by_the_hours_holder_and_summed() {
    let mut frame = frame();
    let row = |ordinal, country, value| FactionManpowerRow { faction: FACTION, ordinal, country, value };
    set(&mut frame, TableId::FactionManpower, TableRows::FactionManpower(vec![row(0, 7, 1000), row(1, 0, 250), row(2, 7, 30), row(3, 99, 5)]));
    set(&mut frame, TableId::CountryLinks, TableRows::CountryLinks(vec![CountryLinkRow { id: 7, canonical_slot: 3 }]));
    set(&mut frame, TableId::ReservedCountryNaming, TableRows::ReservedCountryNaming(vec![ReservedCountryNamingRow { tag: b"RSV".to_vec(), ..Default::default() }]));
    let slots = HashMap::from([(3u32, ("AAA", 7u32))]);
    let mut reply = details::blank(&params());
    let cells = Cells::default();
    details::manpower(&frame, FACTION, &details::Tags::new(&frame, slots), &mut reply, &cells).unwrap();
    let tags: Vec<Option<&str>> = reply.manpower.as_ref().unwrap().iter().map(|entry| entry.tag.as_deref()).collect();
    assert_eq!(tags, [Some("AAA"), Some("RSV"), Some("AAA"), None]);
    assert!(reply.warnings.iter().any(|warning| warning.contains("Country 99 has no link")));
    let live = cells.finish();
    let values = live.values(&frame);
    assert_eq!(values[0], [1000, 250, 30, 5].map(CellValue::Number));
    let json = live.json(&values.into_iter().map(dashboard_follow::live::GroupState::Values).collect::<Vec<_>>());
    assert_eq!(json["rows"], serde_json::json!(["manpower:0", "manpower:1", "manpower:2", "manpower:3", details::FACTION_ROW]));
    assert!(json["kinds"].as_array().unwrap().iter().any(|kind| kind["op"] == "sum"), "the total is the sum of every occurrence");
    set(&mut frame, TableId::FactionManpower, TableRows::FactionManpower(vec![row(0, 7, 1000), row(2, 7, 30)]));
    let mut reply = details::blank(&params());
    let cells = Cells::default();
    details::manpower(&frame, FACTION, &details::Tags::new(&frame, HashMap::new()), &mut reply, &cells).unwrap();
    assert_eq!(reply.manpower, None);
    assert!(cells.finish().is_empty());
}

#[test]
fn a_lane_joins_a_held_selection_and_starts_a_segment_at_a_break_or_a_gap() {
    let mut lane = history::Lane::default();
    for (first, last, value, starts) in [(10, 19, 5, true), (20, 29, 5, false), (30, 39, 6, false), (40, 49, 6, true), (60, 69, 6, false)] {
        lane.push(first, last, value, starts);
    }
    assert_eq!(lane.runs, [(10, 29, 5, true), (30, 39, 6, false), (40, 49, 6, true), (60, 69, 6, true)]);
}

#[test]
fn a_country_s_faction_is_the_one_listing_any_country_linked_to_its_slot() {
    let links = [CountryLinkRow { id: 7, canonical_slot: 3 }, CountryLinkRow { id: 8, canonical_slot: 3 }, CountryLinkRow { id: 9, canonical_slot: 4 }];
    let members = [FactionMemberRow { country: 9, faction: OTHER, ordinal: 0 }, FactionMemberRow { country: 8, faction: FACTION, ordinal: 1 }];
    assert_eq!(country_goals::faction_of(&members, &links, 3), Some(FACTION));
    assert_eq!(country_goals::faction_of(&members, &links, 4), Some(OTHER));
    assert_eq!(country_goals::faction_of(&members, &links, 5), None, "a slot no member links to belongs to no faction");
}

#[test]
fn faction_goals_resolve_name_and_follow_only_the_goals_the_filter_shows_and_an_unknown_state_is_never_in_progress() {
    use super::country_goals::{GoalFilter, GoalState, shown_goals};
    let with = |status: i32, row: FactionGoalEntryRow| FactionGoalEntryRow { status, ..row };
    let frame = goal_frame(vec![
        goal(FACTION, 0, 0, Some(2), 25_000),
        with(9, goal(FACTION, 0, 1, Some(1), 50_000)),
        with(3, goal(FACTION, 0, 2, Some(2), 75_000)),
        goal(FACTION, 1, 0, Some(3), 0),
    ]);
    let listed = |filter: GoalFilter| {
        let mut reply = details::blank(&params());
        let cells = Cells::default();
        details::goal_lists(&frame, FACTION, None, None, &mut reply, &cells, Some(filter)).unwrap();
        let live = cells.finish();
        let rows = if live.is_empty() { serde_json::json!([]) } else { live.json(&live.values(&frame).into_iter().map(dashboard_follow::live::GroupState::Values).collect::<Vec<_>>())["rows"].clone() };
        (reply, rows)
    };
    let (progress, rows) = listed(GoalFilter::InProgress);
    assert_eq!(progress.manifest, None, "the manifest is not resolved for a list of goals");
    let tokens: Vec<Option<&str>> = progress.definitions.iter().map(|definition| definition.token.as_deref()).collect();
    assert_eq!(tokens, [Some("mod_goal")], "a hidden goal's definition is never resolved");
    assert_eq!(rows, serde_json::json!(["goal:0", "goal:2"]), "a hidden goal registers no progress cell");
    assert!(progress.warnings.iter().any(|warning| warning.starts_with("1 faction goals are in a state")));
    let (goals, definitions, _) = shown_goals(progress);
    let shown: Vec<_> = goals.iter().map(|goal| (goal.key.as_str(), definitions[goal.definition as usize].token.as_deref(), goal.state)).collect();
    assert_eq!(shown, [("goal:0", Some("mod_goal"), GoalState::InProgress), ("goal:2", Some("mod_goal"), GoalState::Completing)]);
    let (all, _) = listed(GoalFilter::All);
    let (all, _, _) = shown_goals(all);
    assert_eq!(all.iter().find(|goal| goal.key == "goal:1").map(|goal| (goal.state, goal.unrecognised_status)), Some((GoalState::Unrecognised, Some(9))));
    let (done, rows) = listed(GoalFilter::Completed);
    assert_eq!(rows, serde_json::json!([]));
    let (done, done_definitions, _) = shown_goals(done);
    assert_eq!((done.len(), done[0].ordinal, done_definitions[0].token.as_deref()), (1, None, Some("mod_done")));
}

#[test]
fn faction_pools_orders_resources_tracks_quiet_hours_and_resolves_slot_membership() {
    use super::pools::FactionPools;
    use dashboard_base::{
        Source, SourceKey,
        products::{LoadContext, at_hour::exact_panel},
        test_fixtures::{CAMPAIGN, records, source},
    };
    use replay::campaign::Replay;
    use replay_data::rows::{
        CountryRow, FactionDoctrineRow, FactionResearchRow, FactionResourceRow, FactionRow, FactionUnlockRow, ResourceDefinitionRow,
    };
    use serde_json::json;
    use std::sync::{Arc, atomic::AtomicBool};

    let frames: Vec<DecodedFrame> = (100..=105)
        .map(|hour| {
            let mut frame = source(1, hour, None);
            let tag = if hour >= 104 { b"BBB".to_vec() } else { b"AAA".to_vec() };
            set(
                &mut frame,
                TableId::Countries,
                TableRows::Countries(vec![
                    CountryRow { id: 7, canonical_slot: 3, tag, ..Default::default() },
                    CountryRow { id: 8, canonical_slot: 4, tag: b"CCC".to_vec(), ..Default::default() },
                ]),
            );
            set(
                &mut frame,
                TableId::CountryLinks,
                TableRows::CountryLinks(vec![
                    CountryLinkRow { id: 7, canonical_slot: 3 },
                    CountryLinkRow { id: 8, canonical_slot: 4 },
                ]),
            );
            set(
                &mut frame,
                TableId::Factions,
                TableRows::Factions(vec![FactionRow { id: FACTION, ..Default::default() }]),
            );
            set(
                &mut frame,
                TableId::FactionMembers,
                TableRows::FactionMembers(vec![
                    FactionMemberRow { country: 7, faction: FACTION, ordinal: 0 },
                    FactionMemberRow { country: 8, faction: FACTION, ordinal: 1 },
                ]),
            );
            set(
                &mut frame,
                TableId::ResourceDefinitions,
                TableRows::ResourceDefinitions(vec![
                    ResourceDefinitionRow { id: 1, token: 10, active: 1, factory_cost: 0 },
                    ResourceDefinitionRow { id: 2, token: 11, active: 1, factory_cost: 0 },
                    ResourceDefinitionRow { id: 3, token: 12, active: 1, factory_cost: 0 },
                ]),
            );
            set(
                &mut frame,
                TableId::LexicalTokens,
                TableRows::LexicalTokens(vec![
                    LexicalTokenRow { token: 10, literal: b"oil".to_vec() },
                    LexicalTokenRow { token: 11, literal: b"steel".to_vec() },
                    LexicalTokenRow { token: 12, literal: b"rubber".to_vec() },
                    LexicalTokenRow { token: 20, literal: b"land_doctrine".to_vec() },
                ]),
            );
            if hour == 105 {
                *frame.tables[TableId::FactionResources.index()] = TableView::Unavailable { reason: "resources unread".into() };
            } else {
                let steel = if hour >= 103 { 500_000 } else { 200_000 };
                set(
                    &mut frame,
                    TableId::FactionResources,
                    TableRows::FactionResources(vec![
                        FactionResourceRow { faction: FACTION, ordinal: 0, token: 0, raw: 0 },
                        FactionResourceRow { faction: FACTION, ordinal: 1, token: 10, raw: 200_000 },
                        FactionResourceRow { faction: FACTION, ordinal: 2, token: 11, raw: steel },
                        FactionResourceRow { faction: FACTION, ordinal: 3, token: 12, raw: 90_000 },
                    ]),
                );
            }
            set(
                &mut frame,
                TableId::FactionUnlocks,
                TableRows::FactionUnlocks(vec![FactionUnlockRow { faction: FACTION, manpower: u8::from(hour >= 102), research: 1 }]),
            );
            set(
                &mut frame,
                TableId::FactionManpower,
                TableRows::FactionManpower(vec![
                    FactionManpowerRow { faction: FACTION, ordinal: 0, country: 8, value: 150 },
                    FactionManpowerRow { faction: FACTION, ordinal: 1, country: 7, value: 400 },
                ]),
            );
            set(
                &mut frame,
                TableId::FactionResearch,
                TableRows::FactionResearch(vec![FactionResearchRow { faction: FACTION, ordinal: 0, scientist_base_raw: 120_000, bonus_raw: 0, countries: vec![7, 8] }]),
            );
            set(
                &mut frame,
                TableId::FactionDoctrine,
                TableRows::FactionDoctrine(vec![FactionDoctrineRow { faction: FACTION, ordinal: 0, token: 20 }]),
            );
            set(
                &mut frame,
                TableId::ReservedCountryNaming,
                TableRows::ReservedCountryNaming(vec![ReservedCountryNamingRow { tag: b"RSV".to_vec(), ..Default::default() }]),
            );
            frame
        })
        .collect();

    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Source::from_replay(SourceKey { source_id: "wars".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap();
    let load = LoadContext { content: None };
    let params = json!({ "branchId": "1", "country": 3, "pool": "resources" });
    let panel = exact_panel::<FactionPools>(&source, &load, &params).unwrap();
    let exact = panel.exact().unwrap();
    let h100 = (exact.advance)(100, true, &false.into(), None);
    assert!(h100.fresh);
    let r100 = h100.value.as_ref().as_ref().unwrap();
    assert_eq!(r100["status"], "ok");
    assert_eq!(r100["decimals"], 5);
    let ordinals: Vec<u64> = r100["resources"].as_array().unwrap().iter().map(|r| r["ordinal"].as_u64().unwrap()).collect();
    assert_eq!(ordinals, [1, 2, 3], "equal amounts tie-break by ordinal, skipping zero sentinel ordinal 0");
    let h101 = (exact.advance)(101, false, &false.into(), None);
    let h102 = (exact.advance)(102, false, &false.into(), None);
    assert!(!h101.fresh && !h102.fresh, "quiet hours with unchanged resource inputs do not recompute");
    let h103 = (exact.advance)(103, false, &false.into(), None);
    assert!(h103.fresh);
    let r103 = h103.value.as_ref().as_ref().unwrap();
    let ordinals103: Vec<u64> = r103["resources"].as_array().unwrap().iter().map(|r| r["ordinal"].as_u64().unwrap()).collect();
    assert_eq!(ordinals103, [2, 1, 3], "steel rises to the top once its amount exceeds oil");
    let h104 = (exact.advance)(104, false, &false.into(), None);
    assert_eq!(h104.value.as_ref().as_ref(), (exact.cold)(104, &false.into()).as_ref(), "slot lookup holds across AAA -> BBB tag change");
    let h105 = (exact.advance)(105, false, &false.into(), None);
    let r105 = h105.value.as_ref().as_ref().unwrap();
    assert_eq!(r105["status"], "poolNotRecorded");
    assert!(r105["reason"].as_str().unwrap().contains("resources unread"));

    let mp_panel = exact_panel::<FactionPools>(&source, &load, &json!({ "branchId": "1", "country": 3, "pool": "manpower" })).unwrap();
    let mp_exact = mp_panel.exact().unwrap();
    assert_eq!((mp_exact.cold)(101, &false.into()).unwrap()["status"], "poolLocked");
    let mp102 = (mp_exact.cold)(102, &false.into()).unwrap();
    assert_eq!(mp102["status"], "ok");
    assert_eq!(mp102["manpowerTotal"], "550");
    assert_eq!(mp102["manpower"][0]["slot"], 3);
    assert_eq!(mp102["manpower"][0]["amount"], "400");
    assert_eq!(mp102["manpower"][1]["slot"], 4);

    let res_panel = exact_panel::<FactionPools>(&source, &load, &json!({ "branchId": "1", "faction": FACTION.to_string(), "pool": "research" })).unwrap();
    let res = (res_panel.exact().unwrap().cold)(100, &false.into()).unwrap();
    assert_eq!(res["status"], "ok");
    assert_eq!(res["research"]["scientistBase"], "120000");
    assert_eq!(res["research"]["members"].as_array().unwrap().len(), 2);

    let absent_panel = exact_panel::<FactionPools>(&source, &load, &json!({ "branchId": "1", "faction": OTHER.to_string(), "pool": "doctrine" })).unwrap();
    assert_eq!((absent_panel.exact().unwrap().cold)(100, &false.into()).unwrap()["status"], "factionAbsent");
}

#[test]
fn an_unlinked_roster_entry_leaves_membership_unknown_and_an_unknown_unlock_is_never_an_empty_pool() {
    use super::pools::{FactionPoolKind, FactionPoolStatus, blank, build_manpower, pooled_faction};
    let members = [FactionMemberRow { country: 9, faction: FACTION, ordinal: 0 }];
    let links = [CountryLinkRow { id: 7, canonical_slot: 3 }];
    assert!(matches!(pooled_faction(&members, &links, 3), Err((FactionPoolStatus::MembershipNotRecorded, _))), "the unlinked entry could be slot 3's membership");
    let linked = [CountryLinkRow { id: 7, canonical_slot: 3 }, CountryLinkRow { id: 9, canonical_slot: 4 }];
    assert_eq!(pooled_faction(&members, &linked, 3), Ok(None), "every entry resolves to another country: slot 3 is in no faction");
    assert_eq!(pooled_faction(&members, &linked, 4), Ok(Some(FACTION)));

    let mut unread = frame();
    set(&mut unread, TableId::FactionManpower, TableRows::FactionManpower(Vec::new()));
    *unread.tables[TableId::FactionUnlocks.index()] = TableView::Unavailable { reason: "unlocks unread".into() };
    let mut reply = blank(FactionPoolKind::Manpower, FactionPoolStatus::Ok, None, Some(FACTION));
    build_manpower(&unread, FACTION, &mut reply);
    assert_eq!(reply.status, FactionPoolStatus::PoolNotRecorded, "a locked pool and an unlocked empty one look alike without the unlock");
    assert!(reply.manpower.is_empty() && reply.manpower_total.is_none());
}

#[test]
fn country_faction_goals_holds_quiet_and_foreign_hours_and_refreshes_on_membership_goal_and_damage() {
    use super::country_goals::CountryFactionGoals;
    use dashboard_base::{
        Source, SourceKey,
        products::{LoadContext, at_hour::{exact_panel, frame_panel}},
        test_fixtures::{CAMPAIGN, materialized_frame, played, records, source, streamed},
    };
    use replay::campaign::Replay;
    use replay_data::rows::CountryRow;
    use serde_json::json;
    use std::sync::{Arc, atomic::AtomicBool};

    let make = |branch: u64, hour: i32| {
        let mut f = source(branch, hour, Some(100));
        set(&mut f, TableId::Countries, TableRows::Countries(vec![
            CountryRow { id: 7, canonical_slot: 3, tag: b"AAA".to_vec(), political_power_raw: Some(i64::from(hour) * 5), ..Default::default() },
            CountryRow { id: 8, canonical_slot: 4, tag: b"BBB".to_vec(), ..Default::default() },
            CountryRow { id: 9, canonical_slot: 5, tag: b"CCC".to_vec(), ..Default::default() },
        ]));
        set(&mut f, TableId::CountryLinks, TableRows::CountryLinks([(7, 3), (8, 4), (9, 5)].map(|(id, canonical_slot)| CountryLinkRow { id, canonical_slot }).to_vec()));
        let mut members = Vec::new();
        if hour <= 102 || (hour >= 105 && hour != 109) {
            members.push(FactionMemberRow { country: 7, faction: FACTION, ordinal: 0 });
        }
        if hour % 2 == 0 {
            members.push(FactionMemberRow { country: 8, faction: OTHER, ordinal: 0 });
        } else {
            members.push(FactionMemberRow { country: 9, faction: OTHER, ordinal: 0 });
        }
        if (111..=112).contains(&hour) {
            *f.tables[TableId::FactionMembers.index()] = TableView::Unavailable { reason: "members unread".into() };
        } else {
            set(&mut f, TableId::FactionMembers, TableRows::FactionMembers(members));
        }
        set(&mut f, TableId::FactionGoalStatus, TableRows::FactionGoalStatus(vec![
            FactionGoalStatusRow { faction: OTHER, manifest: Some(1) },
            FactionGoalStatusRow { faction: FACTION, manifest: Some(if hour >= 104 { 2 } else { 1 }) },
        ]));
        let mut entries = vec![
            goal(OTHER, 0, 0, Some(2), i64::from(hour) * 100),
            FactionGoalEntryRow {
                faction: FACTION,
                kind: 0,
                ordinal: 0,
                token: Some(2),
                status: if hour >= 106 { 3 } else { 0 },
                progress_raw: i64::from(hour) * 1000,
                active_sections: vec![hour as u32],
                date: hour,
                slots: 1,
            },
        ];
        if hour >= 107 {
            entries.push(goal(FACTION, 1, 0, Some(3), 0));
        }
        if hour == 108 {
            *f.tables[TableId::FactionGoalEntries.index()] = TableView::Unavailable { reason: "goal entries damaged".into() };
        } else {
            set(&mut f, TableId::FactionGoalEntries, TableRows::FactionGoalEntries(entries));
        }
        set(&mut f, TableId::LexicalTokens, TableRows::LexicalTokens(
            ["mod_manifest", "mod_goal", "mod_done"].into_iter().zip(1..).map(|(literal, token)| LexicalTokenRow { token, literal: literal.as_bytes().to_vec() }).collect(),
        ));
        f
    };

    let main_frames: Vec<_> = (100..=114).map(|hour| make(1, hour)).collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, main_frames.clone())]).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Source::from_replay(SourceKey { source_id: "cfg-played".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap();
    let load = LoadContext { content: None };
    let params = json!({"branchId": "1", "country": "AAA", "goals": "all"});
    let panel = exact_panel::<CountryFactionGoals>(&source, &load, &params).unwrap();
    let exact = panel.exact().unwrap();
    let mut whole = frame_panel::<CountryFactionGoals>(&source, &load, &params).unwrap();
    let steps = played(source.replay_ref(), exact, 100, 114);
    for step in &steps {
        let expected = streamed(&materialized_frame(&mut *whole, source.replay_ref(), 1, step.hour), step.fresh);
        assert_eq!(streamed(&step.value, step.fresh), expected, "country-faction-goals at {}", step.hour);
        if matches!(step.hour, 101 | 102 | 104 | 114) {
            assert_eq!(step.product_calls, 0, "quiet/foreign/progress hour {} should not rebuild", step.hour);
        }
    }
    let back = (exact.advance)(105, true, &false.into(), None);
    assert_eq!(*back.value, materialized_frame(&mut *whole, source.replay_ref(), 1, 105), "backward seek");
    let cold = (exact.cold)(100, &false.into());
    assert_eq!(cold, materialized_frame(&mut *whole, source.replay_ref(), 1, 100), "cold path");
    let progress = cold.as_ref().unwrap()["live"]["groups"][0]["values"][0].as_i64();
    assert_eq!(progress, Some(100_000), "cold country-faction-goals must decode nonzero progress_raw live cell");
    let alt_frames: Vec<_> = (105..=108).map(|hour| make(2, hour)).collect();
    let branched = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, main_frames), records(2, alt_frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let branch2 = branched.timelines().unwrap().list.iter().find(|t| t.id != 1).unwrap().id;
    let alt_source = Source::from_replay(SourceKey { source_id: "cfg-branch".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(branched), &AtomicBool::new(false)).unwrap();
    let alt_panel = exact_panel::<CountryFactionGoals>(&alt_source, &load, &json!({"branchId": branch2.to_string(), "country": "AAA", "goals": "all"})).unwrap();
    let alt_read = (alt_panel.exact().unwrap().advance)(106, true, &false.into(), None);
    let mut alt_whole = frame_panel::<CountryFactionGoals>(&alt_source, &load, &json!({"branchId": branch2.to_string(), "country": "AAA", "goals": "all"})).unwrap();
    assert_eq!(*alt_read.value, materialized_frame(&mut *alt_whole, alt_source.replay_ref(), branch2, 106), "branch switch");
}

#[test]
fn country_faction_goals_refreshes_when_scripted_goal_text_input_changes() {
    scripted_goal_refresh("cfg_scripted_goal_flag", "has_global_flag = mod_flag", false);
    scripted_goal_refresh("cfg_scripted_goal_war", "has_war = yes", true);
}

fn scripted_goal_refresh(fixture: &str, condition: &str, unrelated_tokens: bool) {
    use super::country_goals::CountryFactionGoals;
    use dashboard_base::{
        products::{LoadContext, at_hour::{AtHour, exact_panel, frame_panel}},
        test_fixtures::{game, materialized_frame, played, source, source_with_content, streamed, visual_mods::Machine},
    };
    use replay_data::rows::{CountryRow, CountryWarRow, Recorded, ScriptFlagRow};
    use serde_json::json;
    use std::{collections::BTreeMap, sync::{Arc, Mutex}};

    game::enter();
    let machine = Machine::new(fixture);
    machine.write("common/defines/probe.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NCharacter={OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU={}},NSupply={MAX_RAILWAY_LEVEL=7}}");
    machine.write("common/factions/goals/mod_goals.txt", b"mod_goal = { name = mod_goal_name description = mod_goal_desc }\n");
    machine.write(
        "common/scripted_localisation/mod_loc.txt",
        format!("defined_text = {{\n  name = ModGoalPhrase\n  text = {{ trigger = {{ {condition} }} localization_key = mod_phrase_on }}\n  text = {{ trigger = {{ always = yes }} localization_key = mod_phrase_off }}\n}}\n").as_bytes(),
    );
    machine.write(
        "localisation/english/mod_l_english.yml",
        "\u{feff}l_english:\n mod_goal_name:0 \"Mod Goal\"\n mod_goal_desc:0 \"State: [ROOT.ModGoalPhrase]\"\n mod_phrase_on:0 \"Active\"\n mod_phrase_off:0 \"Idle\"\n".as_bytes(),
    );

    let last = if unrelated_tokens { 106 } else { 105 };
    let frames: Vec<_> = (100..=last)
        .map(|hour| {
            let mut f = source(1, hour, Some(100));
            set(&mut f, TableId::Countries, TableRows::Countries(vec![
                CountryRow { id: 7, canonical_slot: 3, tag: b"AAA".to_vec(), ..Default::default() },
                CountryRow { id: 8, canonical_slot: 4, tag: b"BBB".to_vec(), ..Default::default() },
            ]));
            set(&mut f, TableId::CountryLinks, TableRows::CountryLinks(vec![
                CountryLinkRow { id: 7, canonical_slot: 3 }, CountryLinkRow { id: 8, canonical_slot: 4 },
            ]));
            set(&mut f, TableId::FactionMembers, TableRows::FactionMembers(vec![FactionMemberRow { country: 7, faction: FACTION, ordinal: 0 }]));
            set(&mut f, TableId::FactionGoalStatus, TableRows::FactionGoalStatus(vec![FactionGoalStatusRow { faction: FACTION, manifest: None }]));
            set(&mut f, TableId::FactionGoalEntries, TableRows::FactionGoalEntries(vec![goal(FACTION, 0, 0, Some(1), i64::from(hour) * 100)]));
            let mut lexical = vec![
                LexicalTokenRow { token: 1, literal: b"mod_goal".to_vec() },
                LexicalTokenRow { token: 2, literal: b"mod_flag".to_vec() },
            ];
            if unrelated_tokens {
                lexical.extend((101..=hour).map(|hour| LexicalTokenRow { token: hour as u32, literal: format!("unrelated_{hour}").into_bytes() }));
                let enemies = if (103..106).contains(&hour) { vec![8] } else { Vec::new() };
                set(&mut f, TableId::CountryWars, TableRows::CountryWars(vec![CountryWarRow {
                    country: 7, enemies_read: 1, enemies, ordered: Recorded::Value(None), can_occupy_non_war: 0,
                }]));
                set(&mut f, TableId::WarRelations, TableRows::WarRelations(Vec::new()));
            } else {
                let flags = if hour >= 103 { vec![ScriptFlagRow { scope_kind: 0, scope_id: 0, token: 2, set_hour: 103, value: 1, remaining_days: 0 }] } else { Vec::new() };
                set(&mut f, TableId::ScriptFlags, TableRows::ScriptFlags(flags));
            }
            set(&mut f, TableId::LexicalTokens, TableRows::LexicalTokens(lexical));
            f
        })
        .collect();

    let params = json!({"branchId": "1", "country": "AAA", "goals": "all"});
    let asked: super::country_goals::CountryFactionGoalsParams = serde_json::from_value(params.clone()).unwrap();
    let counted = |replay: &replay::campaign::Replay, panel: &dashboard_follow::panel::AtHour, gap: Option<(Arc<dashboard_base::Source>, LoadContext)>| {
        let stretches = Arc::new(Mutex::new(BTreeMap::<i32, (u64, u64)>::new()));
        let counts = Arc::clone(&stretches);
        let advance = Arc::clone(&panel.advance);
        let mut gap_params = params.clone();
        gap_params["hour"] = json!(last + 1);
        let tracked = dashboard_follow::panel::AtHour {
            advance: Arc::new(move |hour, seed, cancelled, step| {
                if hour == last && let Some((source, load)) = &gap {
                    let refusal = dashboard_base::products::at_hour::entry::<CountryFactionGoals>()
                        .answer(source, gap_params.clone(), load, cancelled).unwrap_err();
                    assert!(refusal.to_string().contains("hour is not recorded"));
                }
                let (reply, reads) = replay_recording::cells::count_reads(|| advance(hour, seed, cancelled, step));
                let mut counts = counts.lock().unwrap();
                let (stretches, opened) = counts.entry(hour).or_default();
                let wars = reads.get(&TableId::CountryWars).copied().unwrap_or_default();
                *stretches += wars.stretches;
                *opened += wars.opened;
                reply
            }),
            cold: Arc::clone(&panel.cold),
            read: Arc::clone(&panel.read),
        };
        let steps = played(replay, &tracked, 100, last);
        let counts = std::mem::take(&mut *stretches.lock().unwrap());
        (steps, counts)
    };
    if unrelated_tokens {
        let (control, load) = source_with_content(frames.clone(), &machine.root);
        let panel = exact_panel::<CountryFactionGoals>(&control, &load, &params).unwrap();
        let (_, reads) = counted(control.replay_ref(), panel.exact().unwrap(), None);
        assert!(reads.values().map(|&(stretches, _)| stretches).sum::<u64>() > 0);
    }
    let prepared = |frames| {
        let (source, load) = source_with_content(frames, &machine.root);
        let source = Arc::new(source);
        if unrelated_tokens && let Some(pending) = CountryFactionGoals::PREPARE.unwrap()(&source, &asked, &load, 100).unwrap() {
            pending.wait().unwrap()
        }
        (source, load)
    };
    if unrelated_tokens {
        let (warm, load) = prepared(frames.clone());
        let panel = exact_panel::<CountryFactionGoals>(&warm, &load, &params).unwrap();
        let (_, reads) = counted(warm.replay_ref(), panel.exact().unwrap(), None);
        assert_eq!(reads[&last].1, 0, "the second change can reuse its parked readers");
    }
    let (source, load) = prepared(frames);
    let panel = exact_panel::<CountryFactionGoals>(&source, &load, &params).unwrap();
    let exact = panel.exact().unwrap();
    let mut whole = frame_panel::<CountryFactionGoals>(&source, &load, &params).unwrap();
    let gap = unrelated_tokens.then(|| (Arc::clone(&source), LoadContext { content: load.content.clone() }));
    let (steps, reads) = counted(source.replay_ref(), exact, gap);
    if unrelated_tokens {
        assert_eq!(reads[&103].0, 0);
        assert_eq!(reads.values().map(|&(stretches, _)| stretches).sum::<u64>(), 0);
        assert!(reads[&last].1 > 0, "a gap request discards the parked world");
    }
    for step in &steps {
        let expected = streamed(&materialized_frame(&mut *whole, source.replay_ref(), 1, step.hour), step.fresh);
        assert_eq!(streamed(&step.value, step.fresh), expected, "scripted country-faction-goals at {}", step.hour);
        let desc = step.value.as_ref().as_ref().unwrap()["definitions"][0]["description"].as_str().unwrap_or_else(|| panic!("missing description: {:?}", step.value));
        assert_eq!(desc, if (103..106).contains(&step.hour) { "State: Active" } else { "State: Idle" });
        if matches!(step.hour, 101 | 102 | 104 | 105) {
            assert_eq!(step.product_calls, 0, "{fixture} quiet scripted hour {} should hold", step.hour);
        }
    }
}

#[test]
fn faction_details_holds_absent_refusal_and_live_edits_and_keeps_cold_live_values() {
    use super::details::FactionDetails;
    use dashboard_base::{
        Source, SourceKey,
        products::{LoadContext, at_hour::{exact_panel, frame_panel}},
        test_fixtures::{CAMPAIGN, materialized_frame, played, records, source, streamed},
    };
    use replay::campaign::Replay;
    use replay_data::rows::{
        CountryRow, FactionDoctrineRow, FactionProjectionRow, FactionResearchRow, FactionResourceRow, FactionRow,
        FactionUnlockRow, ResourceDefinitionRow,
    };
    use serde_json::json;
    use std::sync::{Arc, atomic::AtomicBool};

    let frames: Vec<_> = (100..=108)
        .map(|hour| {
            let mut f = source(1, hour, None);
            let ger_tag = if hour >= 107 { b"GDR".to_vec() } else { b"GER".to_vec() };
            set(&mut f, TableId::Countries, TableRows::Countries(vec![
                CountryRow { id: 1, canonical_slot: 1, tag: ger_tag, ..Default::default() },
            ]));
            set(&mut f, TableId::CountryLinks, TableRows::CountryLinks(vec![CountryLinkRow { id: 1, canonical_slot: 1 }]));
            set(&mut f, TableId::ReservedCountryNaming, TableRows::ReservedCountryNaming(vec![ReservedCountryNamingRow { tag: b"RSV".to_vec(), ..Default::default() }]));
            set(&mut f, TableId::ResourceDefinitions, TableRows::ResourceDefinitions(vec![
                ResourceDefinitionRow { id: 0, token: 10, ..Default::default() },
                ResourceDefinitionRow { id: 1, token: 11, ..Default::default() },
            ]));
            set(&mut f, TableId::LexicalTokens, TableRows::LexicalTokens(vec![
                LexicalTokenRow { token: 1, literal: b"mod_manifest".to_vec() },
                LexicalTokenRow { token: 2, literal: b"mod_goal".to_vec() },
                LexicalTokenRow { token: 3, literal: b"mod_doctrine".to_vec() },
                LexicalTokenRow { token: 10, literal: b"oil".to_vec() },
                LexicalTokenRow { token: 11, literal: b"steel".to_vec() },
            ]));
            let present = hour >= 102;
            set(&mut f, TableId::Factions, TableRows::Factions(if present {
                vec![FactionRow { id: FACTION, ..Default::default() }]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionGoalStatus, TableRows::FactionGoalStatus(if present {
                vec![FactionGoalStatusRow { faction: FACTION, manifest: Some(1) }]
            } else {
                Vec::new()
            }));
            let step = i64::from(hour - 100);
            set(&mut f, TableId::FactionGoalEntries, TableRows::FactionGoalEntries(if present {
                vec![goal(FACTION, 0, 0, Some(2), 10_000 + step * 500)]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionUnlocks, TableRows::FactionUnlocks(if present {
                vec![FactionUnlockRow { faction: FACTION, research: 1, manpower: 1 }]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionProjection, TableRows::FactionProjection(if present {
                vec![FactionProjectionRow { faction: FACTION, projection_raw: 200_000 + step * 1_000, ..Default::default() }]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionManpower, TableRows::FactionManpower(if present {
                vec![FactionManpowerRow { faction: FACTION, ordinal: 0, country: 1, value: 1_000 + (step as i32) * 10 }]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionResearch, TableRows::FactionResearch(if present {
                vec![FactionResearchRow { faction: FACTION, ordinal: 0, scientist_base_raw: 50_000 + step * 100, countries: vec![1], ..Default::default() }]
            } else {
                Vec::new()
            }));
            set(&mut f, TableId::FactionDoctrine, TableRows::FactionDoctrine(if present {
                vec![FactionDoctrineRow { faction: FACTION, ordinal: 0, token: 3 }]
            } else {
                Vec::new()
            }));
            let res0 = if hour >= 105 { 300_000 + step * 10 } else { 0 };
            set(&mut f, TableId::FactionResources, TableRows::FactionResources(if present {
                vec![
                    FactionResourceRow { faction: FACTION, ordinal: 0, token: 10, raw: res0 },
                    FactionResourceRow { faction: FACTION, ordinal: 1, token: 11, raw: 500_000 + step * 100 },
                ]
            } else {
                Vec::new()
            }));
            f
        })
        .collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let source = Source::from_replay(
        SourceKey { source_id: "fd-exact".into(), revision: "1".into() },
        "memory.battleplan".into(),
        Arc::new(replay),
        &AtomicBool::new(false),
    )
    .unwrap();
    let load = LoadContext { content: None };
    let params = json!({"branchId": "1", "faction": FACTION.to_string(), "viewpoint": null});
    let panel = exact_panel::<FactionDetails>(&source, &load, &params).unwrap();
    let exact = panel.exact().unwrap();
    let mut whole = frame_panel::<FactionDetails>(&source, &load, &params).unwrap();
    let steps = played(source.replay_ref(), exact, 100, 108);
    for step in &steps {
        let expected = streamed(&materialized_frame(&mut *whole, source.replay_ref(), 1, step.hour), step.fresh);
        assert_eq!(streamed(&step.value, step.fresh), expected, "faction-details at hour {}", step.hour);
    }
    assert!(steps[0].value.is_err());
    assert_eq!(steps[1].product_calls, 0, "absent faction refusal holds while Factions stays empty");
    assert!(steps[2].value.is_ok());
    for hour in [103, 104, 106, 108] {
        assert_eq!(steps[(hour - 100) as usize].product_calls, 0, "moving live cells at hour {hour} should hold");
    }
    assert_eq!(steps[4].value.as_ref().as_ref().unwrap()["resources"].as_array().unwrap().len(), 1);
    assert_eq!(steps[5].value.as_ref().as_ref().unwrap()["resources"].as_array().unwrap().len(), 2, "ordinal 0 flipping from 0 to nonzero rebuilds");
    assert_eq!(steps[7].value.as_ref().as_ref().unwrap()["manpower"][0]["tag"], "GDR");
    let cold = (exact.cold)(106, &AtomicBool::new(false));
    let expected_cold = materialized_frame(&mut *whole, source.replay_ref(), 1, 106);
    assert_eq!(cold, expected_cold, "cold read preserves every nonzero live cell");
    let cold_groups = cold.unwrap()["live"]["groups"].as_array().unwrap().clone();
    assert!(cold_groups.iter().all(|g| g["values"].as_array().unwrap().iter().all(|v| v.as_i64().unwrap() > 0)));
}

#[test]
fn cached_faction_art_keeps_its_overlay_notes() {
    use dashboard_base::{catalogue, test_fixtures::visual_mods::{BLUE, Machine, RED, first_pixel, png, select}};

    let machine = Machine::new("faction_art_notes");
    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_fixture_faction textureFile=gfx/fixture_faction.png}}");
    machine.write("gfx/fixture_faction.png", &png(RED));
    machine.write_mod("fixture_art", "gfx/fixture_faction.png", &png(BLUE));
    let (source, mut options) = machine.open();
    select(&source, &mut options, &["fixture_art"]);
    let catalogue = catalogue::catalogue(&source, &options).unwrap();
    let art = super::Art::default();
    let read = || {
        let (image, noted) = catalogue::capture(|| art.sprite(&catalogue, "GFX_fixture_faction", 64, "Fixture faction").unwrap());
        let mut warnings = Vec::new();
        noted.diagnostics().append_to(&mut warnings);
        (first_pixel(&image), warnings)
    };
    let first = read();
    assert_eq!(first.0, BLUE);
    assert!(first.1.iter().any(|line| line.starts_with("Artwork from visual mods") && line.contains("fixture_art")), "{first:?}");
    assert_eq!(read(), first);
}
