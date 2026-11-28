use super::{
    EventFeedKind, Names,
    cards::{ScriptCurrency, edge},
    cause, derived, families,
    row::{Columns, FeedEvent, Party, Slot},
    text::Notices,
};
use dashboard_query::log::{Cell, Row};
use replay_data::{
    event_feed_rows::{EventFeedRoute, EventFeedScope, EventFeedSource, SCRIPT_FAMILY_NAMES},
    rows::{FeedEventRow, TableId, TagHolderRow},
    value::Value,
};
use std::collections::HashMap;

fn column(name: &str) -> usize {
    dashboard_declare::field(TableId::FeedEvents, name).unwrap()
}

fn whole_log_search_sort(product: &str) {
    use dashboard_base::{products::LoadContext, test_fixtures::{CAMPAIGN, ownership_frame, records}};
    use replay_data::{event_feed_rows::{EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript, EventWindows}, rows::{Recorded, TableRows}};
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};
    let frames = (100..=106).map(|hour| {
        let mut frame = ownership_frame(1, hour, 9001);
        frame.metrics.table_ns[TableId::EventFeedHistory.index()] = 1;
        let tokens = match hour { 100 => b"alpha".as_slice(), 101 => b"bravo".as_slice(), _ => b"zulu".as_slice() };
        let events = if hour <= 102 { vec![EventFeedEvent {
            generation: 1, sequence: (hour - 99) as u64, hour,
            source: if product == "event-choices-tail" { EventFeedSource::ScriptEvent } else { EventFeedSource::DecisionSelected },
            actor: if product == "event-choices-tail" { EventFeedCountry::default() } else { EventFeedCountry { reference: if hour == 100 { 66 } else { 9001 }, id: if hour == 100 { 66 } else { 9001 }, instance: 1 } },
            target: if product == "event-choices-tail" { EventFeedCountry { reference: if hour == 100 { 66 } else { 9001 }, id: if hour == 100 { 66 } else { 9001 }, instance: 1 } } else { EventFeedCountry::default() }, cause: Recorded::NotRecorded,
            script: Some(EventFeedScript { token_len: tokens.len() as u32, scope_unrecorded: true, ..Default::default() }),
        }] } else { Vec::new() };
        let count = events.len() as u64;
        *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
            generation: 1, read_hour: hour, coverage: EventFeedCoverage::Complete,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
            outstanding: 0, failure: None, script_refusals: 0, events, script_tokens: tokens.to_vec(), script_scopes: Vec::new(),
            windows: Recorded::Value(EventWindows { verdicts: Ok(Vec::new()), presentations: Vec::new(), presentations_unrecorded: None }),
        }]));
        frame
    }).collect();
    let replay = replay::campaign::Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Arc::new(dashboard_base::Source::from_replay(dashboard_base::SourceKey { source_id: product.into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap());
    let context = LoadContext { content: None };
    let cancel = AtomicBool::new(false);
    let panel = |extra: serde_json::Value| {
        let mut ask = serde_json::json!({ "panel": product, "branchId": "1", "limit": 2 });
        if product == "event-feed-tail" { ask["categories"] = serde_json::json!(["politics"]); }
        ask.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        match product {
            "decisions-tail" => super::decisions_dated_panel(&source, &context, &ask).unwrap(),
            "event-choices-tail" => super::event_choices_dated_panel(&source, &context, &ask).unwrap(),
            _ => super::event_feed_dated_panel(&source, &context, &ask).unwrap(),
        }
    };
    let by_date = panel(serde_json::json!({}));
    let baseline = (by_date.exact().unwrap().cold)(106, &cancel).unwrap();
    assert_eq!(baseline["total"], "3");
    let hours = |reply: &serde_json::Value| reply["rows"].as_array().unwrap().iter().map(|r| r["hour"].as_i64().unwrap()).collect::<Vec<_>>();
    assert_eq!(hours(&baseline), [102, 101]);
    let searched = panel(serde_json::json!({ "search": "alpha" }));
    assert_eq!(hours(&(searched.exact().unwrap().cold)(106, &cancel).unwrap()), [100]);
    let future = panel(serde_json::json!({ "search": "zulu" }));
    assert_eq!((future.exact().unwrap().cold)(101, &cancel).unwrap()["total"], "0");
    let sorted = panel(serde_json::json!({ "sort": { "column": "country", "direction": "descending" } }));
    let exact = sorted.exact().unwrap();
    assert_eq!(hours(&(exact.cold)(106, &cancel).unwrap()), [100, 102]);
    for hour in 100..=106 {
        assert_eq!(*(exact.advance)(hour, hour == 100, &cancel, None).value, (exact.cold)(hour, &cancel), "{product} at {hour}");
    }
}

#[test]
fn decisions_searches_beyond_page_two_and_sorts_the_whole_log_while_playing() { whole_log_search_sort("decisions-tail"); }

#[test]
fn event_choices_searches_beyond_page_two_and_sorts_the_whole_log_while_playing() { whole_log_search_sort("event-choices-tail"); }

#[test]
fn event_feed_searches_beyond_page_two_and_sorts_the_whole_log_while_playing() { whole_log_search_sort("event-feed-tail"); }

fn row(generation: u64, sequence: u64, set: &[(&str, Cell)]) -> Row {
    let mut cells = vec![Cell::Unrecorded; TableId::FeedEvents.field_count()];
    for (name, cell) in set {
        cells[column(name)] = cell.clone();
    }
    Row { hour: 0, key: u128::from(generation) << 64 | u128::from(sequence), facet: 0, cells }
}

fn number(value: i64) -> Cell {
    Cell::Number(value)
}

fn party(reference: u32, slot: Slot) -> Party {
    Party { reference, id: Some(reference), slot }
}

#[test]
fn a_war_start_keeps_the_known_participant_its_role_and_the_missing_reason() {
    use dashboard_base::{products::LoadContext, test_fixtures::{CAMPAIGN, records, source}};
    use replay_data::rows::{TableRows, WarRelationRow};
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};
    let frames = (100..=102).map(|hour| {
        let mut frame = source(1, hour, Some(1));
        let rows = if hour == 100 { Vec::new() } else {
            vec![
                WarRelationRow { first_country: 9001, second_country: 9002, start_hour: 101, end_hour: 0, first_was_instigator: 1, ..Default::default() },
                WarRelationRow { first_country: 9001, second_country: 9003, start_hour: 101, end_hour: 0, first_was_instigator: 0, ..Default::default() },
            ]
        };
        *frame.tables[TableId::WarRelations.index()] = TableView::Complete(TableRows::WarRelations(rows));
        frame
    }).collect();
    let replay = replay::campaign::Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let held = dashboard_base::Source::from_replay(dashboard_base::SourceKey { source_id: "partial-war-feed".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap();
    let query = dashboard_base::queries::query(&held, 1).unwrap();
    let transitions = super::transitions::load_branch(&query, &AtomicBool::new(false)).unwrap();
    let reply = dashboard_world::products::groups::shared(&held, &query, &LoadContext { content: None }, &AtomicBool::new(false)).unwrap();
    let (made, notes, omitted, _) = families::group_rows(Some(&reply), None, &transitions, &[], (100, 102), &[super::EventFeedCategory::WarsPeace]);
    let rows: Vec<_> = made.into_iter().map(|entry| entry.row).collect();
    assert_eq!(rows.iter().filter(|row| row.kind == EventFeedKind::WarStarted).map(|row| &row.id).collect::<std::collections::HashSet<_>>().len(), 2, "opposite partial starts at the same hour never share a row key");
    let start = rows.iter().find(|row| row.kind == EventFeedKind::WarStarted && row.participants.as_ref().is_some_and(|roles| roles[0].is_some())).expect("the start exists even when its defender is unresolved");
    assert_eq!(start.tags, ["GER"]);
    assert_eq!(start.participants, Some([Some("GER".into()), None]));
    let wire = serde_json::to_value(start).unwrap();
    assert!(wire["participantsMissing"][0].is_null());
    assert!(wire["participantsMissing"][1].as_str().unwrap().contains("defending"));
    assert!(rows.iter().any(|row| row.participants == Some([None, Some("GER".into())])), "the known defender keeps its role as well");
    assert!(notes.iter().any(|(hour, note)| *hour == 101 && note.contains("totals may be incomplete")), "country-filtered totals must not claim complete attribution: {notes:?}");
    assert!(omitted.is_empty(), "a retained start is never reported as omitted");
}

fn event(code: u8, hour: i32, actor: Slot, target: Slot) -> FeedEvent {
    FeedEvent {
        generation: 1,
        sequence: 7,
        hour,
        read_hour: hour,
        code,
        actor: party(11, actor),
        target: party(12, target),
        cause: None,
        token: None,
        native_event: None,
        instance: None,
        option: None,
        country_target: None,
        numeric_target: None,
        lifecycle: None,
        days: None,
        scopes: None,
        opener: None,
        dated: None,
        costs: [None, None],
        coverage: None,
        outstanding: None,
        refusals: None,
        failure: Vec::new(),
        incomplete: None,
        window: None,
        presentation: None,
    }
}

#[test]
fn a_feed_rows_cells_read_back_typed_and_what_was_not_recorded_stays_so() {
    let scope = EventFeedScope { country: 31, random: [5, 6], bindings: [0, 1, 0], state: 4, country_attempted: true, country_id: Some(310), ..EventFeedScope::default() };
    let mut scopes = Vec::new();
    vec![scope].encode(&mut scopes).unwrap();
    let columns = Columns::new().unwrap();
    let held = row(
        1,
        42,
        &[
            ("hour", number(100)),
            ("read_hour", number(101)),
            ("kind", number(i64::from(EventFeedSource::Join as u8))),
            ("actor_reference", number(11)),
            ("actor_id", number(110)),
            ("actor_slot", number(7)),
            ("target_reference", number(0)),
            ("target_slot", number(i64::from(TagHolderRow::NONE))),
            ("route", number(i64::from(EventFeedRoute::CallAlly as u8))),
            ("within", number(0)),
            ("conference", Cell::Missing),
            ("state", Cell::Missing),
            ("ally_reference", number(13)),
            ("ally_id", Cell::Missing),
            ("ally_instance", Cell::Missing),
            ("ally_slot", Cell::Unrecorded),
            ("reason", number(4)),
            ("flags", number(1)),
            ("token", Cell::Text("news.1".into())),
            ("native_event_kind", number(50)),
            ("native_event_id", number(9)),
            ("instance", Cell::Missing),
            ("option", number(-1)),
            ("country_target", Cell::Missing),
            ("numeric_target", Cell::Missing),
            ("lifecycle", Cell::Missing),
            ("days", Cell::Missing),
            ("scopes", Cell::List(scopes)),
            ("opener", number(40)),
            ("opener_hour", number(99)),
            ("dated", Cell::Missing),
            ("political_power", number(-5_000_000)),
            ("command_power", Cell::Missing),
            ("coverage", Cell::Missing),
            ("outstanding", Cell::Missing),
            ("refusals", Cell::Missing),
            ("failure", Cell::Text(String::new())),
            ("backlog", number(0)),
        ],
    );
    let read = columns.read(&held).unwrap();
    assert_eq!((read.generation, read.sequence, read.hour, read.read_hour), (1, 42, 100, 101));
    assert_eq!(read.actor, Party { reference: 11, id: Some(110), slot: Slot::Of(7) });
    assert_eq!(read.target.slot, Slot::None, "reference zero names no country");
    let cause = read.cause.unwrap();
    assert_eq!((cause.route, cause.reason, cause.flags, cause.conference), (EventFeedRoute::CallAlly as u8, Some(4), 1, None));
    assert_eq!(cause.ally.map(|ally| (ally.reference, ally.slot)), Some((13, Slot::NotRecorded)), "an ally no link witnessed is not recorded, not none");
    assert_eq!(read.token.as_deref(), Some(&b"news.1"[..]));
    assert_eq!((read.native_event, read.option, read.instance), (Some((50, 9)), Some(-1), None));
    assert_eq!(read.scopes, Some(vec![scope]));
    assert_eq!(read.opener, Some(40));
    assert_eq!(read.costs, [Some(Some(-5_000_000)), Some(None)]);
    assert_eq!(read.id(), "native:1:42");
    let unread_command = row(1, 43, &[("hour", number(5)), ("read_hour", number(5)), ("kind", number(i64::from(EventFeedSource::DecisionSelected as u8))), ("actor_reference", number(11)), ("target_reference", number(0)), ("political_power", number(-7)), ("command_power", Cell::Unrecorded)]);
    assert_eq!(columns.read(&unread_command).unwrap().costs, [Some(Some(-7)), None], "an unread command-power cell keeps the political power read beside it");

    let bare = row(FeedEventRow::DERIVED, 5 << 32 | 3, &[("hour", number(5)), ("read_hour", number(5)), ("kind", number(i64::from(FeedEventRow::CEASED_TO_EXIST))), ("actor_reference", number(11)), ("target_reference", number(0))]);
    let read = columns.read(&bare).unwrap();
    assert_eq!((&read.cause, &read.scopes, &read.costs, &read.token), (&None, &None, &[None, None], &None));
    assert_eq!(read.actor.slot, Slot::NotRecorded);
    assert_eq!(read.id(), "derived:5:3");
    assert!(columns.read(&row(1, 1, &[("hour", number(5))])).is_err());
}

#[test]
fn a_cause_reads_in_words_with_its_flags_and_a_scripts_operands_follow_it() {
    let mut join = event(EventFeedSource::Join as u8, 10, Slot::Of(7), Slot::Of(8));
    join.cause = Some(super::row::Cause { route: EventFeedRoute::CallAlly as u8, within: EventFeedRoute::OnAction as u8, conference: Some(77), state: None, ally: None, reason: Some(4), flags: 1 });
    let words = cause::words(&join, Some("ITA".into())).unwrap();
    assert_eq!((words.how.as_deref(), words.within.as_deref(), words.reason.as_deref()), (Some("a call to arms"), Some("an on_action"), Some("guarantee")));
    assert_eq!((words.conference.as_deref(), words.ally.as_deref()), (Some("77"), Some("ITA")));
    assert_eq!(words.notes, ["against every enemy of the ally's side"]);
    assert_eq!(cause::words(&event(EventFeedSource::Annex as u8, 10, Slot::Of(7), Slot::Of(8)), None), None, "no cause recorded, no words");

    let mut choice = event(EventFeedSource::EventOption as u8, 10, Slot::Of(7), Slot::Of(7));
    choice.native_event = Some((50, 9));
    choice.option = Some(0);
    choice.scopes = Some(vec![EventFeedScope { country: 3, references: [0, 0, 5, 0, 0, 0, 0, 0, 0], ..EventFeedScope::default() }]);
    let words = cause::words(&choice, None).unwrap();
    assert_eq!(words.how, None);
    assert_eq!(words.target, None);
    assert_eq!(
        words.notes,
        [
            "Native event identity: 9 (type 50).",
            "Selected native option ordinal: 0.",
            "Scope 0: country reference 3; state 0; ROOT 0, FROM 0, PREV 0; random words 0, 0.",
            "Scope 0 ace reference: 5.",
        ]
    );
    let mut annex = event(EventFeedSource::Annex as u8, 10, Slot::Of(7), Slot::Of(8));
    annex.cause = Some(super::row::Cause { route: EventFeedRoute::AnnexEffect as u8, within: EventFeedRoute::CivilWarAnnex as u8, conference: None, state: None, ally: None, reason: None, flags: 0 });
    assert!(cause::civil_war(&annex), "an annexation inside a civil war's resolution");

    use super::EventFeedScriptTarget::{Country, State};
    let mut decision = event(EventFeedSource::DecisionSelected as u8, 10, Slot::Of(7), Slot::None);
    for (country, numeric, expected) in [
        (Some(8), Some(77), Country),
        (Some(u32::MAX), Some(77), State { id: Some(77) }),
        (None, Some(77), State { id: Some(77) }),
        (Some(0), Some(-1), State { id: None }),
    ] {
        decision.country_target = country;
        decision.numeric_target = numeric;
        assert_eq!(cause::words(&decision, None).unwrap().target, Some(expected));
    }
}

#[test]
fn an_incomplete_read_is_one_notice_counted_with_examples_and_its_refused_families_each_one() {
    let mut notices = Notices::default();
    for hour in [100, 101, 102, 103] {
        let mut read = event(FeedEventRow::READ_INCOMPLETE, hour, Slot::None, Slot::None);
        read.coverage = Some(if hour == 100 { 1 } else { 0 });
        read.outstanding = Some(0);
        read.refusals = Some(if hour == 101 { 0b11 } else { 0 });
        read.failure = if hour == 103 { b"identity differs".to_vec() } else { Vec::new() };
        families::read_notices(&read, &[(90, 100)], &mut notices);
    }
    let mut complete = event(FeedEventRow::READ_INCOMPLETE, 104, Slot::None, Slot::None);
    complete.coverage = Some(2);
    complete.refusals = Some(1);
    families::read_notices(&complete, &[], &mut notices);
    let list = notices.into_list();
    let coverage = list.iter().find(|notice| notice.kind == "nativeEventCoverage").unwrap();
    assert_eq!(coverage.count, Some(4), "a complete read is no coverage notice");
    assert!(coverage.reason.starts_with("Partial read at 100: The recorder was detached"), "{}", coverage.reason);
    assert_eq!(coverage.reason.matches("; example: ").count(), 2, "three examples at most, the first the reason");
    let families: Vec<_> = list.iter().filter(|notice| notice.kind == "nativeScriptFamilyCoverage").map(|notice| (notice.label.as_str(), notice.count)).collect();
    assert_eq!(families, [(SCRIPT_FAMILY_NAMES[0].1, Some(2)), (SCRIPT_FAMILY_NAMES[1].1, Some(1))]);
}

#[test]
fn a_decisions_cost_is_listed_only_when_proven_and_only_the_currencies_it_charged() {
    let row = |event: &FeedEvent| super::EventFeedRow {
        id: event.id(),
        hour: event.hour,
        read_hour: event.read_hour,
        name_hour: event.hour,
        exact: true,
        category: super::EventFeedCategory::Politics,
        kind: EventFeedKind::DecisionSelected,
        tags: Vec::new(),
        participants: None,
        participants_missing: None,
        owner_missing: None,
        item: None,
        amount: None,
        cause: None,
        strike: None,
        decision_state: None,
        window: None,
    };
    let mut selected = event(EventFeedSource::DecisionSelected as u8, 10, Slot::Of(7), Slot::None);
    selected.costs = [Some(Some(-5_000_000)), Some(Some(0))];
    let costs = edge(&selected, row(&selected), false).costs.unwrap();
    assert_eq!(costs.iter().map(|cost| (cost.currency, cost.amount.as_str())).collect::<Vec<_>>(), [(ScriptCurrency::PoliticalPower, "-5000000")]);
    selected.costs = [Some(None), Some(None)];
    assert!(edge(&selected, row(&selected), false).costs.is_none(), "a kind that charges nothing proves nothing");
    selected.costs = [None, None];
    assert!(edge(&selected, row(&selected), false).costs.is_none(), "not proven is null, never zero");
    selected.costs = [Some(Some(-5_000_000)), None];
    let partly = edge(&selected, row(&selected), false);
    assert_eq!((partly.costs.map(|costs| costs.len()), partly.costs_unread), (Some(1), vec![ScriptCurrency::CommandPower]), "an unread command-power cell keeps the political power that was read, and says which currency it lacks");
    selected.costs = [Some(Some(0)), None];
    let zero = edge(&selected, row(&selected), false);
    assert_eq!((zero.costs.map(|costs| costs.iter().map(|cost| cost.amount.clone()).collect::<Vec<_>>()), zero.costs_unread), (Some(vec!["0".to_owned()]), vec![ScriptCurrency::CommandPower]), "a proven zero stays beside the currency not read");
    selected.costs = [None, Some(Some(-30))];
    assert_eq!(edge(&selected, row(&selected), false).costs_unread, [ScriptCurrency::PoliticalPower], "and the other way round");
    selected.costs = [Some(Some(-20)), Some(None)];
    let partial = edge(&selected, row(&selected), true);
    assert_eq!((partial.costs, partial.costs_unread), (None, Vec::new()), "a charge an older recorder wrote from a partial read is not proven");
}

#[test]
fn a_derived_row_is_dated_by_what_its_countries_held() {
    let names = Names { slots: HashMap::from([(7, vec![("GER".to_owned(), 0, 99), ("DDR".to_owned(), 100, 200)]), (8, vec![("POL".to_owned(), 0, 200)])]) };
    let mut notices = Notices::default();
    let mut ceased = event(FeedEventRow::CEASED_TO_EXIST, 150, Slot::Of(7), Slot::None);
    ceased.generation = FeedEventRow::DERIVED;
    ceased.dated = Some(90);
    let row = derived(&ceased, EventFeedKind::CeasedToExist, ceased.id(), &names, &mut notices);
    assert_eq!((row.hour, row.name_hour, row.exact, row.tags.as_slice()), (150, 90, false, &["GER".to_owned()][..]), "named at its last read with states");
    let mut guarantee = event(FeedEventRow::GUARANTEE_GRANTED, 150, Slot::Of(8), Slot::Of(7));
    guarantee.generation = FeedEventRow::DERIVED;
    guarantee.dated = Some(120);
    let row = derived(&guarantee, EventFeedKind::GuaranteeGranted, guarantee.id(), &names, &mut notices);
    assert_eq!((row.hour, row.exact, row.tags.as_slice()), (120, true, &["POL".to_owned(), "DDR".to_owned()][..]));
    guarantee.dated = Some(80);
    let row = derived(&guarantee, EventFeedKind::GuaranteeGranted, guarantee.id(), &names, &mut notices);
    assert_eq!((row.hour, row.exact), (150, false), "a tag that changed since the start dates it at its read");
    guarantee.target.slot = Slot::NotRecorded;
    let row = derived(&guarantee, EventFeedKind::GuaranteeGranted, guarantee.id(), &names, &mut notices);
    assert_eq!((row.hour, row.exact, row.participants), (150, false, Some([Some("POL".to_owned()), None])), "a witnessed guarantee keeps its row, the party no tag names not identified");
    ceased.actor.slot = Slot::NotRecorded;
    assert_eq!(derived(&ceased, EventFeedKind::CeasedToExist, ceased.id(), &names, &mut notices).participants, Some([None, None]), "and so does a cessation");
    assert!(notices.into_list().iter().any(|notice| notice.kind == "guaranteeGranted"));
}

#[test]
fn a_logged_strike_reads_back_whole_and_a_refused_part_is_never_totalled() {
    use replay_data::{
        event_feed_rows::EventFeedCountry,
        rows::{NuclearBuildingDamage, NuclearEquipmentLoss, NuclearStrikePart, NuclearStrikeRefusal, NuclearStrikeRow, NuclearUnitDamage, NuclearWarSupport},
    };
    let strike = NuclearStrikeRow {
        id: 9,
        hour: 40,
        launcher: EventFeedCountry { reference: 3, id: 3, instance: 1 },
        nuke_type: 1,
        province: 13_000,
        state: 1_204,
        owner: EventFeedCountry { reference: 7, id: 7, instance: 1 },
        controller: 7,
        war_support: vec![NuclearWarSupport { country: 7, before_raw: 60_000, after_raw: 35_000 }],
        buildings: vec![NuclearBuildingDamage { amount_raw: 20_000_000, ..Default::default() }, NuclearBuildingDamage { amount_raw: 5_000_000, ..Default::default() }],
        units: vec![NuclearUnitDamage { manpower_before: 9_000, manpower_after: 6_500, ..Default::default() }],
        equipment: vec![NuclearEquipmentLoss { amount_raw: 1_200_000, ..Default::default() }],
        refusals: vec![NuclearStrikeRefusal { part: NuclearStrikePart::Equipment, reason: b"equipment destruction writers not forwarded".to_vec(), count: 1 }],
        wings: Vec::new(),
    };
    fn list<T: Value>(value: &T) -> Cell {
        let mut bytes = Vec::new();
        value.encode(&mut bytes).unwrap();
        Cell::List(bytes)
    }
    let names = ["hour", "launcher", "nuke_type", "province", "state", "owner", "controller", "war_support", "buildings", "units", "equipment", "refusals", "wings"];
    let cells = vec![
        Cell::Number(40),
        list(&strike.launcher),
        Cell::Number(1),
        Cell::Number(13_000),
        Cell::Number(1_204),
        list(&strike.owner),
        Cell::Number(7),
        list(&strike.war_support),
        list(&strike.buildings),
        list(&strike.units),
        list(&strike.equipment),
        list(&strike.refusals),
        list(&strike.wings),
    ];
    let columns = super::nuclear::Columns::of(&names);
    let logged = Row { hour: 41, key: 9, facet: 0, cells: cells.clone() };
    assert_eq!(columns.strike(&logged).unwrap(), strike);
    let mut unread = cells;
    unread[6] = Cell::Unrecorded;
    assert!(columns.strike(&Row { hour: 41, key: 9, facet: 0, cells: unread }).is_err(), "a cell not recorded refuses the strike");
    let row = super::nuclear::row(&strike, 41, [Some("USA".into()), Some("JAP".into())], None);
    assert_eq!((row.id.as_str(), row.hour, row.read_hour, row.kind), ("nuclear:9", 40, 41, EventFeedKind::NuclearStrike));
    let detail = row.strike.unwrap();
    assert_eq!((detail.state, detail.building_levels, detail.war_support, detail.units), (Some(1_204), Some(2.5), Some(-25.0), Some(1)));
    assert_eq!((detail.manpower_lost, detail.equipment_lost), (None, None));
    assert_eq!(detail.not_recorded, ["equipment: equipment destruction writers not forwarded (1×)"]);
    assert_eq!(super::nuclear::detail(&NuclearStrikeRow::default()).state, None, "a province without a state");
}

#[test]
fn scripted_text_not_expanded_is_marked_where_it_stood_never_as_an_authored_ellipsis() {
    use dashboard_text::scripted_text::{SCRIPTED_GAP, ScriptedGap, expand_dates_in};
    let localisation = content_reader_localisation::localisation::Localisation::default();
    let hour = game_model::calendar::HOUR_EPOCH + 1938 * 8760;
    let expanded = expand_dates_in(
        "Build in [FROM.GetName]\u{2026} [ROOT.GetAdjective] plan of [GetYear] [open",
        hour,
        &localisation,
        "scope was not recorded",
    );
    assert_eq!(
        expanded.text,
        format!("Build in {SCRIPTED_GAP}\u{2026} {SCRIPTED_GAP} plan of 1938 [open")
    );
    assert_eq!(
        expanded.gaps,
        [
            ScriptedGap { key: "FROM.GetName".into(), reason: "scope was not recorded".into() },
            ScriptedGap { key: "ROOT.GetAdjective".into(), reason: "scope was not recorded".into() },
        ]
    );
    assert_ne!(SCRIPTED_GAP, '\u{2026}');
}

#[test]
fn a_family_failing_for_other_than_damage_or_a_refusal_is_read_again_at_the_next_request_and_panel_read() {
    use super::{event_feed_dated_panel, index::{FAULT, Feed, feed_for}};
    use dashboard_base::{products::LoadContext, test_fixtures::{CAMPAIGN, records, source}};
    use replay::campaign::Replay;
    use std::{io::ErrorKind, sync::{Arc, atomic::AtomicBool}};
    let frames = (100..=104).map(|hour| source(1, hour, Some(1))).collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Arc::new(dashboard_base::Source::from_replay(dashboard_base::SourceKey { source_id: "feed-unsettled".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap());
    let query = dashboard_base::queries::query(&source, 1).unwrap();
    let (context, cancel) = (LoadContext { content: None }, AtomicBool::new(false));
    let feed = || feed_for(&source, &query, 1, &context, None, &cancel).unwrap();
    let refused = |feed: &Feed| feed.notices.iter().map(|notice| notice.1.clone()).collect::<Vec<_>>();
    let failing = |fault: Option<ErrorKind>| {
        FAULT.with(|held| held.set(fault.map(|kind| ("subjects", kind))));
        crate::forget(&source);
    };

    let settled = feed();
    assert_eq!(refused(&settled), ["projectFinished", "nuclearTypeNames"], "with no content stack, special projects refuse by name");
    assert!(Arc::ptr_eq(&settled, &feed()), "an explicit refusal is settled, so the feed is kept");

    failing(Some(ErrorKind::InvalidData));
    let damaged = feed();
    assert_eq!(refused(&damaged), ["subjects", "projectFinished", "nuclearTypeNames"]);
    assert!(Arc::ptr_eq(&damaged, &feed()), "recorded damage is settled, so the feed is kept");

    failing(Some(ErrorKind::Interrupted));
    let interrupted = feed();
    assert_eq!(refused(&interrupted), ["subjects", "projectFinished", "nuclearTypeNames"], "any other failure refuses that family by name for this reply");
    assert!(!Arc::ptr_eq(&interrupted, &feed()), "and the feed is not kept, so the next request reads it again");

    let panel = event_feed_dated_panel(&source, &context, &serde_json::json!({ "panel": "event-feed-tail", "branchId": "1", "limit": 64, "categories": ["diplomacy"] })).unwrap();
    let exact = panel.exact().unwrap();
    let kinds = |hour: i32, seed: bool| {
        let read = (exact.advance)(hour, seed, &cancel, None);
        let reply = read.value.as_ref().as_ref().unwrap().clone();
        (reply["unavailable"].as_array().unwrap().iter().map(|notice| notice["kind"].as_str().unwrap().to_owned()).collect::<Vec<_>>(), reply["nextChange"].as_i64())
    };
    assert_eq!(kinds(103, true), (vec!["contentStack".to_owned(), "subjects".to_owned(), "factionMembership".to_owned()], Some(104)), "a panel opened on a failed read refuses the family, valid for its own hour only");
    FAULT.with(|held| held.set(None));
    assert_eq!(kinds(103, false).0, ["contentStack", "factionMembership"], "asked again at the same hour, the panel keeps no failed reply: it prepares the feed again and reads the family");
    assert_eq!(kinds(102, true).0, ["contentStack", "factionMembership"]);
}

#[test]
fn incomplete_reads_raise_the_notices_a_walk_of_the_reads_asked_raises() {
    let reads = [(100, 1, 0), (101, 0, 0b11), (102, 2, 0b100), (103, 0, 0), (104, 1, 0b1), (105, 2, 0b1), (106, 0, 0b10000), (107, 0, 0b1)];
    let rows: Vec<(i32, u128, FeedEvent)> = reads
        .iter()
        .enumerate()
        .map(|(sequence, &(hour, coverage, refusals))| {
            let mut read = event(FeedEventRow::READ_INCOMPLETE, hour, Slot::None, Slot::None);
            (read.sequence, read.coverage, read.outstanding, read.refusals) = (sequence as u64, Some(coverage), Some(2), Some(refusals));
            (hour, sequence as u128, read)
        })
        .collect();
    let losses = [(90, 101)];
    let brief = |notices: Notices| notices.into_list().into_iter().map(|notice| (notice.kind, notice.label, notice.reason, notice.count)).collect::<Vec<_>>();
    let mut walked = Notices::default();
    for (_, _, read) in rows.iter().filter(|(hour, ..)| (101..=106).contains(hour)) {
        families::read_notices(read, &losses, &mut walked);
    }
    let mut counted = Notices::default();
    families::incomplete_notices(&rows, &losses, 101, 106, &mut counted);
    let (walked, counted) = (brief(walked), brief(counted));
    assert_eq!(walked.len(), 5, "coverage, then the families 1, 2, 4 and 16 in the order the reads meet them: {walked:?}");
    assert_eq!(counted, walked, "counts, examples and order equal a walk of the reads in the hours asked, none after");
}

#[test]
fn two_projects_completing_in_one_hour_are_two_rows() {
    use dashboard_research::products::special_projects::{CompletionOwner, ProjectCompletion};
    let completion = |project, ordinal, attribution| ProjectCompletion { attribution, date: 100, observed_hour: 101, owner: 9, project, ordinal, name: None, icon: None };
    let resolved = || CompletionOwner::Resolved { slot: 7, tag: "X01".into() };
    let ids: std::collections::BTreeSet<String> = [completion(1, 4, resolved()), completion(2, 4, resolved()), completion(1, 7, resolved())]
        .into_iter()
        .map(|item| families::completion_row(item).row.id)
        .collect();
    assert_eq!(ids.len(), 3, "each project's completion marker is its own row: {ids:?}");
    let completed = families::completion_row(completion(1, 4, resolved()));
    let (row, slot) = (completed.row, completed.slots.and_then(|s| s[0]));
    assert_eq!(
        (row.hour, row.read_hour, row.name_hour, row.tags.as_slice(), row.participants, row.owner_missing, slot),
        (100, 101, 100, ["X01".to_owned()].as_slice(), Some([Some("X01".to_owned()), None]), None, Some(7)),
        "named, flagged and coloured at its completion, not at the read"
    );

    let first_reason = "Owner not recorded: Countries was not complete at these hours".to_owned();
    let second_reason = "Owner not recorded: raw owner 9 had no country link at this hour.".to_owned();
    let first = families::completion_row(
        completion(3, 0, CompletionOwner::NotRecorded { slot: Some(7), reason: first_reason.clone() }),
    );
    let second = families::completion_row(
        completion(4, 0, CompletionOwner::NotRecorded { slot: None, reason: second_reason.clone() }),
    );
    let (first_unknown, first_slot, second_unknown, second_slot) = (first.row, first.slots.and_then(|s| s[0]), second.row, second.slots.and_then(|s| s[0]));
    assert_eq!(
        (first_unknown.hour, first_unknown.read_hour, first_unknown.name_hour, first_unknown.tags.as_slice(), first_unknown.participants, first_unknown.owner_missing.as_deref(), first_slot),
        (100, 101, 100, [].as_slice(), Some([None, None]), Some(first_reason.as_str()), Some(7)),
        "an unidentified project owner keeps its row with its own missing reason and known slot"
    );
    assert_eq!(
        (second_unknown.participants, second_unknown.owner_missing.as_deref(), second_slot),
        (Some([None, None]), Some(second_reason.as_str()), None),
        "a second unidentified completion carries its own distinct reason on its row"
    );
}

#[test]
fn decision_failed_state_requires_container_and_decodes_each_container_without_guessing() {
    use super::decision_state::{
        COUNTRY_DECISION_STATE_REFUSED, DECISION_CONTAINER_NOT_RECORDED, DECISION_INSTANCE_MISMATCH,
        DECISION_NO_MISSION_LIFECYCLE, DECISION_OCCURRENCE_NOT_FOUND, DecisionLifecycleState,
        EVENT_FEED_HISTORY_INCOMPLETE, decode, enforce_same_incarnation, not_applicable, not_recorded,
        resolve_in_pack,
    };
    use replay_data::event_feed_rows::{
        EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript, EventFeedScriptChange,
        decisions::{DecisionChange, Operation, REMOVAL, REENABLE, TAKEN, TARGETED, TARGETED_TIMED, TIMED},
    };
    use replay_data::rows::Recorded;

    assert_eq!(decode(TIMED, 0, 100), DecisionLifecycleState::Active);
    assert_eq!(decode(TIMED, 1, 100), DecisionLifecycleState::Completed);
    assert_eq!(decode(TIMED, 2, 100), DecisionLifecycleState::Failed { observed_hour: 100 });
    assert_eq!(decode(TIMED, 3, 100), DecisionLifecycleState::Aborted);
    assert_eq!(decode(TIMED, 4, 100), DecisionLifecycleState::ReEnableCooldown);
    assert_eq!(decode(TIMED, 99, 100), DecisionLifecycleState::Unsupported { raw: 99 });

    for container in [TARGETED, TARGETED_TIMED] {
        assert_eq!(decode(container, 0, 100), DecisionLifecycleState::Available);
        assert_eq!(decode(container, 1, 100), DecisionLifecycleState::Completed);
        assert_eq!(decode(container, 2, 100), DecisionLifecycleState::ReEnableCooldown);
        assert_eq!(decode(container, 3, 100), DecisionLifecycleState::Failed { observed_hour: 100 });
        assert_eq!(decode(container, 4, 100), DecisionLifecycleState::Aborted);
        assert_eq!(decode(container, -7, 100), DecisionLifecycleState::Unsupported { raw: -7 });
    }
    for container in [TAKEN, REENABLE, REMOVAL] {
        assert_eq!(decode(container, 2, 100), not_applicable(DECISION_NO_MISSION_LIFECYCLE));
    }
    assert_eq!(decode(255, 2, 100), not_recorded(DECISION_CONTAINER_NOT_RECORDED));

    let actor = EventFeedCountry { reference: 11, id: 11, instance: 1 };
    let feed_ev = |sequence: u64, source: EventFeedSource, instance: Option<u32>, lifecycle: Option<i32>| {
        let mut ev = event(source as u8, 100, Slot::Of(7), Slot::None);
        ev.sequence = sequence;
        ev.read_hour = 101;
        ev.instance = instance;
        ev.lifecycle = lifecycle;
        ev
    };
    let history_ev = |sequence: u64, source: EventFeedSource, change: Option<DecisionChange>, instance: Option<u32>, lifecycle: Option<i32>| EventFeedEvent {
        sequence,
        generation: 1,
        hour: 100,
        source,
        actor,
        target: EventFeedCountry::default(),
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript {
            instance,
            lifecycle,
            change: change.map(EventFeedScriptChange::Decision),
            ..EventFeedScript::default()
        }),
    };
    let make_pack = |coverage: EventFeedCoverage, refusals: u8, events: Vec<EventFeedEvent>| EventFeedHistoryRow {
        generation: 1,
        read_hour: 101,
        coverage,
        counts: Some(EventFeedCounts::default()),
        outstanding: 0,
        failure: None,
        script_refusals: refusals,
        events,
        script_tokens: Vec::new(),
        script_scopes: Vec::new(),
        windows: Default::default(),
    };

    let timed_fail = feed_ev(10, EventFeedSource::DecisionTimedOut, Some(50), Some(2));
    let ok_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(10, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Update, TIMED)), Some(50), Some(2))],
    );
    assert_eq!(resolve_in_pack(&timed_fail, &ok_pack), DecisionLifecycleState::Failed { observed_hour: 101 });

    let targeted_not_failed = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(10, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Update, TARGETED_TIMED)), Some(50), Some(2))],
    );
    assert_eq!(resolve_in_pack(&timed_fail, &targeted_not_failed), DecisionLifecycleState::ReEnableCooldown);

    let instant = feed_ev(12, EventFeedSource::DecisionSelected, None, None);
    let instant_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(12, EventFeedSource::DecisionSelected, None, None, None)],
    );
    assert_eq!(resolve_in_pack(&instant, &instant_pack), not_applicable(DECISION_NO_MISSION_LIFECYCLE));
    let taken_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(12, EventFeedSource::DecisionSelected, Some(DecisionChange::new(Operation::Store, TAKEN)), None, None)],
    );
    assert_eq!(resolve_in_pack(&instant, &taken_pack), not_applicable(DECISION_NO_MISSION_LIFECYCLE));
    let effect = feed_ev(13, EventFeedSource::DecisionTimeoutEffect, None, None);
    let effect_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(13, EventFeedSource::DecisionTimeoutEffect, None, None, None)],
    );
    assert_eq!(resolve_in_pack(&effect, &effect_pack), not_applicable(DECISION_NO_MISSION_LIFECYCLE));
    let duration = feed_ev(14, EventFeedSource::DecisionDurationChanged, None, None);
    let duration_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(14, EventFeedSource::DecisionDurationChanged, None, None, None)],
    );
    assert_eq!(resolve_in_pack(&duration, &duration_pack), not_recorded(DECISION_CONTAINER_NOT_RECORDED), "a changed duration is a timed decision whose container was not recorded");

    let partial_instant = make_pack(EventFeedCoverage::Partial, 0, instant_pack.events.clone());
    assert_eq!(resolve_in_pack(&instant, &partial_instant), not_recorded(EVENT_FEED_HISTORY_INCOMPLETE));
    let refused_instant = make_pack(EventFeedCoverage::Complete, 1, instant_pack.events.clone());
    assert_eq!(resolve_in_pack(&instant, &refused_instant), not_recorded(EVENT_FEED_HISTORY_INCOMPLETE));

    let partial = make_pack(EventFeedCoverage::Partial, 0, ok_pack.events.clone());
    assert_eq!(resolve_in_pack(&timed_fail, &partial), not_recorded(EVENT_FEED_HISTORY_INCOMPLETE));
    let unavailable = make_pack(EventFeedCoverage::Unavailable, 0, ok_pack.events.clone());
    assert_eq!(resolve_in_pack(&timed_fail, &unavailable), not_recorded(EVENT_FEED_HISTORY_INCOMPLETE));
    let refused_family = make_pack(EventFeedCoverage::Complete, 1, ok_pack.events.clone());
    assert_eq!(resolve_in_pack(&timed_fail, &refused_family), not_recorded(EVENT_FEED_HISTORY_INCOMPLETE));

    for bad_op in [Operation::Refused, Operation::Invalidate] {
        let mut events = ok_pack.events.clone();
        events.push(history_ev(11, EventFeedSource::ScriptState, Some(DecisionChange::new(bad_op, 0)), None, None));
        assert_eq!(resolve_in_pack(&timed_fail, &make_pack(EventFeedCoverage::Complete, 0, events)), not_recorded(COUNTRY_DECISION_STATE_REFUSED));

        let mut instant_events = instant_pack.events.clone();
        instant_events.push(history_ev(14, EventFeedSource::ScriptState, Some(DecisionChange::new(bad_op, 0)), None, None));
        assert_eq!(resolve_in_pack(&instant, &make_pack(EventFeedCoverage::Complete, 0, instant_events)), not_recorded(COUNTRY_DECISION_STATE_REFUSED));
    }

    let empty_pack = make_pack(EventFeedCoverage::Complete, 0, Vec::new());
    assert_eq!(resolve_in_pack(&timed_fail, &empty_pack), not_recorded(DECISION_OCCURRENCE_NOT_FOUND));

    let missing_lifecycle_timeout = feed_ev(15, EventFeedSource::DecisionTimedOut, Some(50), None);
    let missing_lifecycle_pack = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(15, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Update, TIMED)), Some(50), None)],
    );
    assert_eq!(resolve_in_pack(&missing_lifecycle_timeout, &missing_lifecycle_pack), not_recorded(DECISION_CONTAINER_NOT_RECORDED));

    let no_container = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(10, EventFeedSource::DecisionTimedOut, None, Some(50), Some(2))],
    );
    assert_eq!(resolve_in_pack(&timed_fail, &no_container), not_recorded(DECISION_CONTAINER_NOT_RECORDED));

    let no_instance = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(10, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Update, TIMED)), None, Some(2))],
    );
    assert_eq!(resolve_in_pack(&timed_fail, &no_instance), not_recorded(DECISION_CONTAINER_NOT_RECORDED));

    let erase_op = make_pack(
        EventFeedCoverage::Complete,
        0,
        vec![history_ev(10, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Erase, TIMED)), Some(50), Some(2))],
    );
    assert_eq!(resolve_in_pack(&timed_fail, &erase_op), not_recorded(DECISION_CONTAINER_NOT_RECORDED));

    let opener = feed_ev(9, EventFeedSource::DecisionActivated, Some(49), Some(0));
    let mut rows = vec![
        Some(super::EventFeedRow {
            id: opener.id(),
            hour: 100,
            read_hour: 100,
            name_hour: 100,
            exact: true,
            category: super::EventFeedCategory::Politics,
            kind: EventFeedKind::DecisionActivated,
            tags: Vec::new(),
            participants: None,
            participants_missing: None,
            owner_missing: None,
            item: None,
            amount: None,
            cause: None,
            strike: None,
            decision_state: Some(DecisionLifecycleState::Active),
            window: None,
        }),
        Some(super::EventFeedRow {
            id: timed_fail.id(),
            hour: 100,
            read_hour: 101,
            name_hour: 100,
            exact: true,
            category: super::EventFeedCategory::Politics,
            kind: EventFeedKind::DecisionTimedOut,
            tags: Vec::new(),
            participants: None,
            participants_missing: None,
            owner_missing: None,
            item: None,
            amount: None,
            cause: None,
            strike: None,
            decision_state: Some(DecisionLifecycleState::Failed { observed_hour: 101 }),
            window: None,
        }),
    ];
    enforce_same_incarnation(&[&opener, &timed_fail], &mut rows);
    assert_eq!(
        rows[0].as_ref().and_then(|row| row.decision_state.as_ref()),
        Some(&DecisionLifecycleState::Active),
        "earlier same-incarnation edge stays unaffected before a later mismatch"
    );
    assert_eq!(
        rows[1].as_ref().and_then(|row| row.decision_state.as_ref()),
        Some(&DecisionLifecycleState::NotRecorded { reason: DECISION_INSTANCE_MISMATCH.to_owned() })
    );
}

#[test]
fn decision_failed_state_handles_same_hour_duplicates_branch_boundaries_and_missing_inputs() {
    use super::{
        DecisionsTail, EventFeedCategory, EventFeedTail, EventFeedTailParams, ScriptTailParams,
        decision_state::{
            DECISION_CONTAINER_NOT_RECORDED, DECISION_INSTANCE_MISMATCH, DECISION_NO_MISSION_LIFECYCLE,
            DecisionLifecycleState, EVENT_FEED_HISTORY_NOT_RECORDED, resolve_all,
        },
    };
    use dashboard_base::{
        products::{LoadContext, Product},
        test_fixtures::{CAMPAIGN, records, source},
    };
    use replay::campaign::Replay;
    use replay_data::{
        event_feed_rows::{
            EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript, EventFeedScriptChange,
            decisions::{DecisionChange, Operation, TARGETED_TIMED, TIMED},
        },
        rows::{Recorded, TableId, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let ger = EventFeedCountry { reference: 9001, id: 9001, instance: 1 };
    let tokens = b"mod_mission_amod_mission_bmod_mission_cmod_mission_dmod_instant_e";
    let span = |index: u32| (index * 13, 13);
    let dec_ev = |sequence: u64, hour: i32, source: EventFeedSource, tok: u32, change: Option<DecisionChange>, instance: Option<u32>, lifecycle: Option<i32>, target: Option<u32>| {
        let (token_start, token_len) = span(tok);
        EventFeedEvent {
            sequence,
            generation: 1,
            hour,
            source,
            actor: ger,
            target: EventFeedCountry::default(),
            cause: Recorded::NotRecorded,
            script: Some(EventFeedScript {
                token_start,
                token_len,
                instance,
                lifecycle,
                country_target: target,
                change: change.map(EventFeedScriptChange::Decision),
                ..EventFeedScript::default()
            }),
        }
    };
    let pack = |read_hour: i32, coverage: EventFeedCoverage, events: Vec<EventFeedEvent>| {
        let count = events.len() as u64;
        EventFeedHistoryRow {
            generation: 1,
            read_hour,
            coverage,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
            outstanding: 0,
            failure: None,
            script_refusals: 0,
            events,
            script_tokens: tokens.to_vec(),
            script_scopes: Vec::new(),
            windows: Default::default(),
        }
    };
    let with_pack = |branch: u64, hour: i32, row: Option<EventFeedHistoryRow>| {
        let mut frame = source(branch, hour, Some(1));
        *frame.tables[TableId::EventFeedHistory.index()] = match row {
            Some(row) => {
                frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
                TableView::Complete(TableRows::EventFeedHistory(vec![row]))
            }
            None => TableView::Unavailable { reason: "older recording lacks event feed history".into() },
        };
        frame
    };

    let b1_h100 = pack(
        100,
        EventFeedCoverage::Complete,
        vec![
            dec_ev(1, 100, EventFeedSource::DecisionActivated, 0, Some(DecisionChange::new(Operation::Store, TIMED)), Some(10), Some(0), None),
            dec_ev(2, 100, EventFeedSource::DecisionActivated, 1, Some(DecisionChange::new(Operation::Store, TIMED)), Some(11), Some(0), None),
            dec_ev(3, 100, EventFeedSource::DecisionActivated, 2, Some(DecisionChange::new(Operation::Store, TARGETED_TIMED)), Some(12), Some(0), Some(9001)),
            dec_ev(4, 100, EventFeedSource::DecisionActivated, 3, Some(DecisionChange::new(Operation::Store, TIMED)), Some(13), Some(0), None),
            dec_ev(9, 100, EventFeedSource::DecisionSelected, 4, None, None, None, None),
        ],
    );
    let b1_h101 = pack(
        101,
        EventFeedCoverage::Complete,
        vec![
            dec_ev(10, 101, EventFeedSource::DecisionTimedOut, 0, Some(DecisionChange::new(Operation::Update, TIMED)), Some(10), Some(1), None),
            dec_ev(11, 101, EventFeedSource::DecisionTimedOut, 1, Some(DecisionChange::new(Operation::Update, TIMED)), Some(11), Some(2), None),
            dec_ev(12, 101, EventFeedSource::DecisionTimedOut, 2, Some(DecisionChange::new(Operation::Update, TARGETED_TIMED)), Some(12), Some(3), Some(9001)),
            dec_ev(13, 101, EventFeedSource::DecisionCompleted, 3, Some(DecisionChange::new(Operation::Update, TIMED)), Some(99), Some(2), None),
        ],
    );
    let b2_h100 = pack(
        100,
        EventFeedCoverage::Complete,
        vec![dec_ev(2, 100, EventFeedSource::DecisionActivated, 1, None, Some(11), Some(0), None)],
    );
    let b2_h101 = pack(
        101,
        EventFeedCoverage::Complete,
        vec![
            dec_ev(11, 101, EventFeedSource::DecisionTimedOut, 1, None, Some(11), Some(2), None),
            dec_ev(14, 100, EventFeedSource::DecisionSelected, 4, None, None, None, None),
        ],
    );

    let replay = Replay::from_records(
        recorder_derive::writers(),
        CAMPAIGN.into(),
        &[
            records(1, vec![with_pack(1, 100, Some(b1_h100)), with_pack(1, 101, Some(b1_h101))]),
            records(2, vec![with_pack(2, 100, Some(b2_h100)), with_pack(2, 101, Some(b2_h101))]),
            records(3, vec![with_pack(3, 100, None), with_pack(3, 101, None)]),
        ],
    )
    .unwrap()
    .into_timelines(&Default::default())
    .unwrap();
    let replay_source = dashboard_base::Source::from_replay(
        dashboard_base::SourceKey { source_id: "decision-failed".into(), revision: "1".into() },
        "synthetic.battleplan".into(),
        Arc::new(replay),
        &AtomicBool::new(false),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let context = LoadContext { content: None };
    let ask = |branch_id: &str| EventFeedTailParams {
        branch_id: branch_id.into(),
        hour: Some(101),
        from: Some(100),
        to: None,
        countries: None,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        categories: vec![EventFeedCategory::WarsPeace, EventFeedCategory::Diplomacy, EventFeedCategory::Politics, EventFeedCategory::Milestones],
        exclude: None,
        expanded: None,
        full: None,
        place_names: None,
    };
    let cards_at = |hour: i32| ScriptTailParams { branch_id: "1".into(), hour: Some(hour), from: Some(100), to: None, countries: None, group: None, search: None, row: None, sort: None, offset: None, limit: 64, events: None, statuses: None };

    let feed_b1_folded = EventFeedTail::load(&replay_source, &ask("1"), &context, &cancel).unwrap();
    let fold_key = match &feed_b1_folded.rows[0] {
        super::EventFeedLine::Fold(fold) => {
            assert_eq!(fold.count, "9");
            fold.key.clone()
        }
        other => panic!("expected 9 consecutive GER decisions on day 4 to fold, got {other:?}"),
    };
    let feed_b1 = EventFeedTail::load(&replay_source, &EventFeedTailParams { expanded: Some(vec![fold_key]), ..ask("1") }, &context, &cancel).unwrap();
    let state_of = |page: &super::EventFeedTailReply, id: &str| {
        page.rows
            .iter()
            .filter_map(|line| match line {
                super::EventFeedLine::Event(row) => row.row.as_ref(),
                _ => None,
            })
            .find(|row| row.id == id)
            .and_then(|row| row.decision_state.clone())
    };
    assert_eq!(state_of(&feed_b1, "native:1:10"), Some(DecisionLifecycleState::Completed));
    assert_eq!(state_of(&feed_b1, "native:1:11"), Some(DecisionLifecycleState::Failed { observed_hour: 101 }));
    assert_eq!(state_of(&feed_b1, "native:1:12"), Some(DecisionLifecycleState::Failed { observed_hour: 101 }));
    assert_eq!(state_of(&feed_b1, "native:1:13"), Some(DecisionLifecycleState::Failed { observed_hour: 101 }));
    assert_eq!(state_of(&feed_b1, "native:1:9"), None, "a list row leaves out a state no list shows");
    let feed_opened = EventFeedTail::load(&replay_source, &EventFeedTailParams { row: Some("native:1:9".into()), ..ask("1") }, &context, &cancel).unwrap();
    assert_eq!(
        state_of(&feed_opened, "native:1:9"),
        Some(DecisionLifecycleState::NotApplicable { reason: DECISION_NO_MISSION_LIFECYCLE.to_owned() }),
        "the opened row keeps it, with its reason"
    );

    let card_end_state = |page: &super::ScriptOccurrenceTailReply, opener_id: &str| {
        page.rows
            .iter()
            .filter_map(|row| row.occurrence.as_ref())
            .find(|occ| occ.id == opener_id)
            .and_then(|occ| occ.edges.last())
            .and_then(|edge| edge.row.decision_state.clone())
    };
    let cards_b1_at_100 = DecisionsTail::load(&replay_source, &cards_at(100), &context, &cancel).unwrap();
    assert_eq!(card_end_state(&cards_b1_at_100, "native:1:2"), Some(DecisionLifecycleState::Active));
    assert_eq!(card_end_state(&cards_b1_at_100, "native:1:4"), Some(DecisionLifecycleState::Active));
    assert_eq!(card_end_state(&cards_b1_at_100, "native:1:9"), None, "a listed card leaves out a state no list shows");
    let card_opened = DecisionsTail::load(&replay_source, &ScriptTailParams { row: Some("native:1:9".into()), ..cards_at(100) }, &context, &cancel).unwrap();
    assert_eq!(
        card_end_state(&card_opened, "native:1:9"),
        Some(DecisionLifecycleState::NotApplicable { reason: DECISION_NO_MISSION_LIFECYCLE.to_owned() }),
        "the opened card keeps it, with its reason"
    );

    let cards_b1 = DecisionsTail::load(&replay_source, &cards_at(101), &context, &cancel).unwrap();
    assert_eq!(DecisionsTail::load(&replay_source, &cards_at(101), &context, &cancel).unwrap(), cards_b1, "a second read of the hour, from the states the index remembers, is the same reply");
    let notes = |reply: &super::ScriptOccurrenceTailReply| reply.rows.iter().flat_map(|row| row.occurrence.iter()).flat_map(|occurrence| &occurrence.edges).filter_map(|edge| edge.row.cause.as_ref()).map(|cause| cause.notes.len()).sum::<usize>();
    let key = cards_b1.rows[0].key.clone();
    let opened = DecisionsTail::load(&replay_source, &ScriptTailParams { row: Some(key.clone()), ..cards_at(101) }, &context, &cancel).unwrap();
    assert_eq!(opened.rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(), [key.as_str()], "the opened row is that row alone");
    assert_eq!(notes(&cards_b1), 0, "list rows leave the notes to the opened row");
    let reopened: usize = cards_b1.rows.iter().map(|row| notes(&DecisionsTail::load(&replay_source, &ScriptTailParams { row: Some(row.key.clone()), ..cards_at(101) }, &context, &cancel).unwrap())).sum();
    assert!(reopened > 0, "opened rows carry them");
    let cause = |reply: &super::ScriptOccurrenceTailReply| reply.rows[0].occurrence.as_ref().and_then(|occurrence| occurrence.edges[0].row.cause.as_ref()).map_or((None, None), |cause| (cause.selected_option, cause.target));
    assert_eq!(cause(&opened), cards_b1.rows.iter().find(|row| row.key == key).and_then(|row| row.occurrence.as_ref().and_then(|occurrence| occurrence.edges[0].row.cause.as_ref())).map_or((None, None), |cause| (cause.selected_option, cause.target)), "a list row keeps the facts its notes carried");
    assert!(DecisionsTail::load(&replay_source, &ScriptTailParams { row: Some("native:9:9".into()), ..cards_at(101) }, &context, &cancel).unwrap().rows.is_empty(), "a row not in the log at the hour is no row");
    assert_eq!(card_end_state(&cards_b1, "native:1:1"), Some(DecisionLifecycleState::Completed));
    assert_eq!(card_end_state(&cards_b1, "native:1:2"), Some(DecisionLifecycleState::Failed { observed_hour: 101 }));
    assert_eq!(card_end_state(&cards_b1, "native:1:3"), Some(DecisionLifecycleState::Failed { observed_hour: 101 }));
    assert_eq!(
        card_end_state(&cards_b1, "native:1:4"),
        Some(DecisionLifecycleState::NotRecorded { reason: DECISION_INSTANCE_MISMATCH.to_owned() }),
        "different instance incarnation across opener and end must withhold failure state"
    );

    let feed_b2 = EventFeedTail::load(&replay_source, &ask("2"), &context, &cancel).unwrap();
    let late = |hour: i32| EventFeedTail::load(&replay_source, &EventFeedTailParams { hour: Some(hour), row: Some("native:1:14".into()), limit: 1, ..ask("2") }, &context, &cancel).unwrap().rows.len();
    assert_eq!((late(100), late(101)), (0, 1), "an event dated 100 but first read at 101 opens only from 101");
    let late_card = |hour: i32| DecisionsTail::load(&replay_source, &ScriptTailParams { branch_id: "2".into(), row: Some("native:1:14".into()), limit: 1, ..cards_at(hour) }, &context, &cancel).unwrap().rows.len();
    assert_eq!(late_card(100), 0, "nor does its card open before it was read");
    assert_eq!(
        state_of(&feed_b2, "native:1:11"),
        Some(DecisionLifecycleState::NotRecorded { reason: DECISION_CONTAINER_NOT_RECORDED.to_owned() }),
        "branch 2 lacks container metadata and must never join branch 1"
    );

    let query_b3 = dashboard_base::queries::query(&replay_source, 3).unwrap();
    let mut bare_timed_out = event(EventFeedSource::DecisionTimedOut as u8, 101, Slot::Of(7), Slot::None);
    bare_timed_out.sequence = 6;
    bare_timed_out.actor = party(9001, Slot::Of(7));
    bare_timed_out.instance = Some(11);
    bare_timed_out.lifecycle = Some(2);
    let mut bare_instant = event(EventFeedSource::DecisionSelected as u8, 100, Slot::Of(7), Slot::None);
    bare_instant.sequence = 9;
    bare_instant.actor = party(9001, Slot::Of(7));
    assert_eq!(
        resolve_all(&query_b3, &[&bare_timed_out, &bare_instant], &mut None).unwrap(),
        vec![
            Some(DecisionLifecycleState::NotRecorded { reason: EVENT_FEED_HISTORY_NOT_RECORDED.to_owned() }),
            Some(DecisionLifecycleState::NotRecorded { reason: EVENT_FEED_HISTORY_NOT_RECORDED.to_owned() }),
        ],
        "missing EventFeedHistory table must refuse by name rather than guessing or returning notApplicable"
    );
}

#[test]
fn feed_tails_answer_one_occurrence_query_by_observation_cutoff_and_occurrence_date() {
    use super::{
        DecisionsTail, EventChoicesTail, EventFeedCategory, EventFeedTail, EventFeedTailParams, ScriptStatus, ScriptTailParams,
        decisions_dated_panel, event_choices_dated_panel, event_feed_dated_panel, index::REQUESTED_EVENT_LOCATORS,
    };
    use dashboard_base::{
        products::{LoadContext, Product},
        test_fixtures::{CAMPAIGN, records, source},
    };
    use dashboard_query::identity::Country;
    use replay::campaign::Replay;
    use replay_data::{
        event_feed_rows::{
            EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedEventIdentity, EventFeedHistoryRow, EventFeedScript, EventFeedScriptChange, ScriptFamily,
            decisions::{DecisionChange, Operation, TIMED},
        },
        rows::{Recorded, TableId, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let ger = EventFeedCountry { reference: 9001, id: 9001, instance: 1 };
    let tokens = b"mod_mission_aevent_choice_1";
    let dec_ev = |sequence: u64, hour: i32, src: EventFeedSource, change: Option<DecisionChange>, instance: Option<u32>, lifecycle: Option<i32>| EventFeedEvent {
        sequence,
        generation: 1,
        hour,
        source: src,
        actor: ger,
        target: EventFeedCountry::default(),
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript {
            token_start: 0,
            token_len: 13,
            instance,
            lifecycle,
            change: change.map(EventFeedScriptChange::Decision),
            ..EventFeedScript::default()
        }),
    };
    let ev_choice = |sequence: u64, hour: i32, src: EventFeedSource, option: Option<i32>| EventFeedEvent {
        sequence,
        generation: 1,
        hour,
        source: src,
        actor: ger,
        target: ger,
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript {
            token_start: 13,
            token_len: 14,
            native_event: Some(EventFeedEventIdentity { kind: 50, id: 7 }),
            option,
            ..EventFeedScript::default()
        }),
    };
    let pack = |read_hour: i32, events: Vec<EventFeedEvent>| {
        let count = events.len() as u64;
        EventFeedHistoryRow {
            generation: 1,
            read_hour,
            coverage: EventFeedCoverage::Complete,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
            outstanding: 0,
            failure: None,
            script_refusals: 0,
            events,
            script_tokens: tokens.to_vec(),
            script_scopes: Vec::new(),
            windows: Default::default(),
        }
    };
    let frames: Vec<_> = (100..=110)
        .map(|hour| {
            let mut frame = source(1, hour, Some(1));
            let events = match hour {
                100 => vec![
                    dec_ev(1, 100, EventFeedSource::DecisionActivated, Some(DecisionChange::new(Operation::Store, TIMED)), Some(10), Some(0)),
                    ev_choice(2, 100, EventFeedSource::ScriptEvent, None),
                ],
                102 => vec![ev_choice(3, 102, EventFeedSource::EventOption, Some(1))],
                103 => vec![dec_ev(4, 103, EventFeedSource::DecisionTimedOut, Some(DecisionChange::new(Operation::Update, TIMED)), Some(10), Some(2))],
                104 => vec![ev_choice(5, 101, EventFeedSource::ScriptEvent, None)],
                105 => vec![ev_choice(6, 105, EventFeedSource::NewsEvent, None)],
                107 => vec![dec_ev(7, 107, EventFeedSource::DecisionActivated, Some(DecisionChange::new(Operation::Store, TIMED)), Some(11), Some(0))],
                _ => Vec::new(),
            };
            let mut read = pack(hour, events);
            match hour {
                108 => read.script_refusals = ScriptFamily::DecisionRandomCount as u8,
                109 => read.script_refusals = ScriptFamily::DecisionLifecycle as u8,
                110 => read.coverage = EventFeedCoverage::Partial,
                _ => {}
            }
            frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
            *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![read]));
            if hour == 106 {
                *frame.tables[TableId::Countries.index()] = TableView::Unavailable { reason: "unread".into() };
            }
            frame
        })
        .collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let replay_source = Arc::new(
        dashboard_base::Source::from_replay(
            dashboard_base::SourceKey { source_id: "feed-tails".into(), revision: "1".into() },
            "synthetic.battleplan".into(),
            Arc::new(replay),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let cancel = AtomicBool::new(false);
    let context = LoadContext { content: None };
    let kinds = super::EventFeedKinds::load(&replay_source, &dashboard_base::products::BranchParams { branch_id: "1".into() }, &context, &cancel).unwrap();
    let kind = |wanted| kinds.kinds.iter().find(|item| item.kind == wanted).unwrap();
    assert_eq!(kind(EventFeedKind::ScriptEventFired).first_observed, Some(100));
    assert_eq!(kind(EventFeedKind::NewsEventFired).first_observed, Some(105));
    assert_eq!(kind(EventFeedKind::EventOptionSelected).first_observed, Some(102));
    assert_eq!(kind(EventFeedKind::WarDeclared).first_observed, None);
    assert!(kind(EventFeedKind::WarDeclared).unavailable.as_deref().unwrap().contains("No events"));
    assert!(kind(EventFeedKind::LawChanged).unavailable.as_deref().unwrap().contains("does not read"));
    let mut unrecorded_frame = source(1, 100, Some(1));
    *unrecorded_frame.tables[TableId::EventFeedHistory.index()] = TableView::Unavailable { reason: "Synthetic native history unavailable".into() };
    let unrecorded = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, vec![unrecorded_frame])])
        .unwrap().into_timelines(&Default::default()).unwrap();
    let unrecorded = dashboard_base::Source::from_replay(
        dashboard_base::SourceKey { source_id: "unrecorded-kinds".into(), revision: "1".into() },
        "synthetic.battleplan".into(), Arc::new(unrecorded), &cancel,
    ).unwrap();
    let unrecorded = super::EventFeedKinds::load(&unrecorded, &dashboard_base::products::BranchParams { branch_id: "1".into() }, &context, &cancel).unwrap();
    let declaration = unrecorded.kinds.iter().find(|item| item.kind == EventFeedKind::WarDeclared).unwrap();
    assert_eq!(declaration.first_observed, None);
    assert!(declaration.unavailable.as_deref().unwrap().contains("Synthetic native history unavailable"));
    let interrupted = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, [100, 102].map(|hour| source(1, hour, Some(1))).into())])
        .unwrap().into_timelines(&Default::default()).unwrap();
    let interrupted = dashboard_base::Source::from_replay(
        dashboard_base::SourceKey { source_id: "interrupted-kinds".into(), revision: "1".into() },
        "synthetic.battleplan".into(), Arc::new(interrupted), &cancel,
    ).unwrap();
    let interrupted = super::EventFeedKinds::load(&interrupted, &dashboard_base::products::BranchParams { branch_id: "1".into() }, &context, &cancel).unwrap();
    let declaration = interrupted.kinds.iter().find(|item| item.kind == EventFeedKind::WarDeclared).unwrap();
    assert!(!declaration.unavailable.as_deref().unwrap().starts_with("No events"));
    let selection = |tag: &str| Some(vec![Country::Tag(tag.into())]);
    let feed = |hour: i32, from: Option<i32>, countries: Option<Vec<Country>>| EventFeedTailParams {
        branch_id: "1".into(),
        hour: Some(hour),
        from,
        to: None,
        countries,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        categories: vec![EventFeedCategory::Politics],
        exclude: None,
        expanded: None,
        full: None,
        place_names: None,
    };
    let cards = |hour: i32, statuses: Option<Vec<ScriptStatus>>, countries: Option<Vec<Country>>| ScriptTailParams {
        branch_id: "1".into(),
        hour: Some(hour),
        from: None,
        to: None,
        countries,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        events: None,
        statuses,
    };
    let hours = |reply: &super::EventFeedTailReply| {
        reply
            .rows
            .iter()
            .map(|line| match line {
                super::EventFeedLine::Event(row) => row.hour,
                super::EventFeedLine::Fold(fold) => fold.hour,
                super::EventFeedLine::More(more) => more.hour,
            })
            .collect::<Vec<_>>()
    };

    let at_106 = EventFeedTail::load(&replay_source, &feed(106, None, None), &context, &cancel).unwrap();
    assert_eq!(at_106.total, "6");
    assert_eq!(hours(&at_106), [105, 103, 102, 101, 100, 100], "ordered by the hour each happened, the event of 101 read at 104 among them");
    let at_103 = EventFeedTail::load(&replay_source, &feed(103, None, None), &context, &cancel).unwrap();
    assert_eq!(hours(&at_103), [103, 102, 100, 100], "the event of 101 does not exist before it was read at 104");
    assert_eq!(at_103.next_change, Some(104));
    let ranged = EventFeedTail::load(&replay_source, &feed(106, Some(102), None), &context, &cancel).unwrap();
    assert_eq!(hours(&ranged), [105, 103, 102], "a range from 102 never lists the event of 101 though it was read at 104");
    let paged = EventFeedTail::load(&replay_source, &EventFeedTailParams { offset: Some("4".into()), limit: 2, ..feed(106, None, None) }, &context, &cancel).unwrap();
    assert_eq!((paged.total.as_str(), hours(&paged)), ("6", vec![100, 100]), "one total and one order on every page");
    let scoped = EventFeedTail::load(&replay_source, &feed(106, None, selection("GER")), &context, &cancel).unwrap();
    assert_eq!(scoped.rows, at_106.rows);
    assert_eq!(EventFeedTail::load(&replay_source, &feed(106, None, selection("ITA")), &context, &cancel).unwrap().total, "0");
    let without = EventFeedTail::load(&replay_source, &EventFeedTailParams { exclude: Some(vec!["decisionActivated".into(), "decisionTimedOut".into()]), ..feed(106, None, None) }, &context, &cancel).unwrap();
    assert_eq!((without.total.as_str(), hours(&without)), ("4", vec![105, 102, 101, 100]), "a kind left out is left out before the count");
    assert!(EventFeedTail::load(&replay_source, &EventFeedTailParams { exclude: Some(vec!["noSuchKind".into()]), ..feed(106, None, None) }, &context, &cancel).is_err(), "an unknown kind is refused by name");
    let activity = super::EventFeedActivity::load(
        &replay_source,
        &super::tails::EventFeedActivityParams { branch_id: "1".into(), hour: 103, from: 101, bin_hours: 2, categories: vec![EventFeedCategory::Politics], exclude: None, countries: None, group: None },
        &context,
        &cancel,
    )
    .unwrap();
    assert_eq!((activity.first_hour, activity.lines[0].before.as_str(), activity.lines[0].bins.clone()), (100, "2", vec![(1, 2)]), "binned by occurrence date, nothing read after 103, the event of 101 not yet");
    let activity_at = |countries: Option<Vec<Country>>| {
        let params = super::tails::EventFeedActivityParams { branch_id: "1".into(), hour: 106, from: 101, bin_hours: 2, categories: vec![EventFeedCategory::Politics], exclude: None, countries, group: None };
        super::EventFeedActivity::load(&replay_source, &params, &context, &cancel).unwrap().missing.iter().map(|gap| (gap.first_hour, gap.last_hour)).collect::<Vec<_>>()
    };
    assert_eq!(activity_at(selection("GER")), [(106, 106)], "the hour no one knows who holds GER is missing from GER's activity, as from its rows");
    assert!(activity_at(None).is_empty(), "and only from a selection that needs a holder");

    let decisions = DecisionsTail::load(&replay_source, &cards(106, None, None), &context, &cancel).unwrap();
    assert_eq!(decisions.total, "1");
    assert_eq!(decisions.rows[0].occurrence.as_ref().unwrap().edges.len(), 2);
    assert_eq!(decisions.rows[0].status, Some(ScriptStatus::Failed), "the timed-out mission's recorded lifecycle says it failed");
    let running_at_102 = DecisionsTail::load(&replay_source, &cards(102, Some(vec![ScriptStatus::Active]), None), &context, &cancel).unwrap();
    assert_eq!((running_at_102.total.as_str(), running_at_102.counts[0].total.as_str(), running_at_102.rows[0].live), ("1", "1", true), "running at 102, counted in Rust");
    let running_at_106 = DecisionsTail::load(&replay_source, &cards(106, Some(vec![ScriptStatus::Active]), None), &context, &cancel).unwrap();
    assert_eq!((running_at_106.total.as_str(), running_at_106.counts[0].total.as_str()), ("0", "0"), "the filter applies at the hour asked, before counting");

    let dec_panel = decisions_dated_panel(&replay_source, &context, &serde_json::json!({ "panel": "decisions-tail", "branchId": "1", "limit": 64 })).unwrap();
    let dec_exact = dec_panel.exact().unwrap();
    let at_100 = (dec_exact.advance)(100, true, &cancel, None);
    assert_eq!(*at_100.value, (dec_exact.cold)(100, &cancel));
    assert_eq!(at_100.value.as_ref().as_ref().unwrap()["rows"][0]["occurrence"]["edges"].as_array().unwrap().len(), 1);
    let at_101 = (dec_exact.advance)(101, false, &cancel, None);
    assert!(!at_101.fresh, "a quiet hour before the next change reads nothing");
    assert_eq!(*at_101.value, (dec_exact.cold)(101, &cancel));
    let at_103 = (dec_exact.advance)(103, false, &cancel, None);
    assert!(at_103.fresh, "the continuing edge at 103 is the next change");
    let exact = dashboard_base::products::Finish::new(&replay_source, &context, DecisionsTail::CONTENT_DEPENDENT)
        .reply(|| DecisionsTail::load(&replay_source, &cards(103, None, None), &context, &cancel)).unwrap();
    assert_eq!(*at_103.value, Ok(serde_json::to_value(exact).unwrap()));
    assert_eq!(at_103.value.as_ref().as_ref().unwrap()["rows"][0]["occurrence"]["edges"].as_array().unwrap().len(), 2);
    assert!(!(dec_exact.advance)(104, false, &cancel, None).fresh);
    let back_100 = (dec_exact.advance)(100, true, &cancel, None);
    assert_eq!(*back_100.value, (dec_exact.cold)(100, &cancel));
    assert_eq!(back_100.value.as_ref().as_ref().unwrap()["rows"][0]["occurrence"]["edges"].as_array().unwrap().len(), 1);

    let choices = EventChoicesTail::load(&replay_source, &cards(106, None, selection("GER")), &context, &cancel).unwrap();
    assert_eq!(choices.total, "4");
    assert_eq!(choices.rows.iter().map(|row| (row.hour, row.live)).collect::<Vec<_>>(), [(105, true), (101, true), (100, true), (102, false)], "awaiting first, then history, each newest first");
    assert_eq!(choices.counts[0].total, "3");
    let by_family = |events| EventChoicesTail::load(&replay_source, &ScriptTailParams { events: Some(events), ..cards(106, None, None) }, &context, &cancel).unwrap();
    use super::tails::ScriptEventKind;
    assert_eq!(by_family(vec![ScriptEventKind::Country]).total, "2", "an orphan is not known to be a country event");
    assert_eq!(by_family(vec![ScriptEventKind::News]).total, "1", "an orphan is not known to be a news event");
    assert_eq!(by_family(vec![ScriptEventKind::Country, ScriptEventKind::News]).total, "3");
    let unknown = by_family(serde_json::from_value(serde_json::json!(["unknown"])).unwrap());
    assert_eq!(unknown.total, "1");
    assert!(!unknown.rows[0].occurrence.as_ref().unwrap().opened);
    let unknown_panel = event_choices_dated_panel(&replay_source, &context, &serde_json::json!({ "panel": "event-choices-tail", "branchId": "1", "limit": 64, "events": ["unknown"] })).unwrap();
    let unknown_exact = unknown_panel.exact().unwrap();
    for hour in [101, 102, 106, 101] {
        let stepped = (unknown_exact.advance)(hour, true, &cancel, None);
        assert_eq!(*stepped.value, (unknown_exact.cold)(hour, &cancel), "orphan family stepped vs cold at {hour}");
        assert_eq!(stepped.value.as_ref().as_ref().unwrap()["total"], if hour < 102 { "0" } else { "1" });
    }
    let awaiting = EventChoicesTail::load(&replay_source, &cards(106, Some(vec![ScriptStatus::Awaiting]), None), &context, &cancel).unwrap();
    assert_eq!(awaiting.total, "3");
    let decided = |hour: i32, status: ScriptStatus| DecisionsTail::load(&replay_source, &cards(hour, Some(vec![status]), None), &context, &cancel).unwrap().total;
    let chose = |hour: i32, status: ScriptStatus| EventChoicesTail::load(&replay_source, &cards(hour, Some(vec![status]), None), &context, &cancel).unwrap().total;
    assert_eq!(
        [107, 108].map(|hour| (decided(hour, ScriptStatus::Active), decided(hour, ScriptStatus::NotRecorded))),
        [("1".into(), "0".into()), ("0".into(), "1".into())],
        "a read refusing a decision family breaks the decision's link, so its running state is not recorded from then"
    );
    assert_eq!((chose(109, ScriptStatus::Awaiting).as_str(), chose(110, ScriptStatus::Awaiting).as_str(), chose(110, ScriptStatus::NotRecorded).as_str()), ("3", "0", "3"), "a decision family refused loses no choice, a partial read may have");
    let lost_choices = EventChoicesTail::load(&replay_source, &cards(110, Some(vec![ScriptStatus::NotRecorded]), None), &context, &cancel).unwrap();
    assert!(lost_choices.rows.iter().all(|row| row.not_recorded_reason.as_deref() == Some("event feed read in part")));
    let lost_feed = EventFeedTail::load(&replay_source, &feed(110, None, None), &context, &cancel).unwrap();
    let lost_feed_reasons: Vec<_> = lost_feed
        .rows
        .iter()
        .filter_map(|line| match line {
            super::EventFeedLine::Event(row) if row.status == Some(ScriptStatus::NotRecorded) => Some(row.not_recorded_reason.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(lost_feed_reasons, [Some("event feed read in part"); 3]);

    let choice_panel = event_choices_dated_panel(&replay_source, &context, &serde_json::json!({ "panel": "event-choices-tail", "branchId": "1", "limit": 64, "countries": ["GER"] })).unwrap();
    let choice_exact = choice_panel.exact().unwrap();
    let finish = dashboard_base::products::Finish::new(&replay_source, &context, EventChoicesTail::CONTENT_DEPENDENT);
    REQUESTED_EVENT_LOCATORS.set(0);
    for (hour, new_locators) in [(100, None), (101, None), (102, None), (104, None), (105, Some(1)), (106, None), (110, None), (100, None), (105, Some(3))] {
        let (stepped, reads) = replay_recording::cells::count_reads(|| (choice_exact.advance)(hour, hour == 100, &cancel, None));
        let followed = REQUESTED_EVENT_LOCATORS.replace(0);
        let exact = finish.reply(|| EventChoicesTail::load(&replay_source, &cards(hour, None, selection("GER")), &context, &cancel)).unwrap();
        let rebuilt = REQUESTED_EVENT_LOCATORS.replace(0);
        assert_eq!(*stepped.value, Ok(serde_json::to_value(exact).unwrap()), "event-choices-tail stepped vs exact at {hour}");
        if let Some(new_locators) = new_locators {
            assert!(stepped.fresh, "the news event enters at 105");
            assert_eq!(rebuilt, 4, "the exact page requests all four event locators");
            assert_eq!(followed, new_locators, "the followed page requests only locators missing from the preceding page");
            assert_eq!(reads.get(&TableId::FeedEvents).map_or(0, |feed| feed.opened), 0, "the followed page retains its event reader");
        }
    }
    let feed_panel = event_feed_dated_panel(&replay_source, &context, &serde_json::json!({ "panel": "event-feed-tail", "branchId": "1", "limit": 64, "categories": ["politics"] })).unwrap();
    let feed_exact = feed_panel.exact().unwrap();
    for hour in [100, 101, 102, 104, 105, 106, 110, 101] {
        let stepped = (feed_exact.advance)(hour, true, &cancel, None);
        assert_eq!(*stepped.value, (feed_exact.cold)(hour, &cancel), "event-feed-tail stepped vs cold at {hour}");
    }
}

#[test]
fn choice_not_recorded_reason_keeps_first_break_before_later_pause() {
    use super::{EventChoicesTail, EventFeedCategory, EventFeedTail, EventFeedTailParams, ScriptTailParams};
    use dashboard_base::{
        products::{LoadContext, Product},
        test_fixtures::{CAMPAIGN, records, source},
    };
    use replay::campaign::Replay;
    use replay_data::{
        event_feed_rows::{
            EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedEventIdentity, EventFeedHistoryRow, EventFeedScript,
        },
        rows::{Recorded, TableId, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let ger = EventFeedCountry { reference: 9001, id: 9001, instance: 1 };
    let tokens = b"ev.first";
    let fired = EventFeedEvent {
        sequence: 1,
        generation: 1,
        hour: 100,
        source: EventFeedSource::ScriptEvent,
        actor: ger,
        target: ger,
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript {
            token_start: 0,
            token_len: tokens.len() as u32,
            native_event: Some(EventFeedEventIdentity { kind: 50, id: 7 }),
            ..EventFeedScript::default()
        }),
    };
    let pack = |read_hour: i32, events: Vec<EventFeedEvent>, coverage: EventFeedCoverage| {
        let count = events.len() as u64;
        EventFeedHistoryRow {
            generation: 1,
            read_hour,
            coverage,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
            outstanding: 0,
            failure: None,
            script_refusals: 0,
            events,
            script_tokens: tokens.to_vec(),
            script_scopes: Vec::new(),
            windows: Default::default(),
        }
    };
    let frames: Vec<_> = (100..=115)
        .map(|hour| {
            let mut frame = source(1, hour, Some(1));
            frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
            *frame.tables[TableId::EventFeedHistory.index()] = match hour {
                100 => TableView::Complete(TableRows::EventFeedHistory(vec![pack(hour, vec![fired.clone()], EventFeedCoverage::Complete)])),
                110 => TableView::Complete(TableRows::EventFeedHistory(vec![pack(hour, Vec::new(), EventFeedCoverage::Partial)])),
                112..=114 => TableView::Unavailable { reason: "recording paused".into() },
                _ => TableView::Complete(TableRows::EventFeedHistory(vec![pack(hour, Vec::new(), EventFeedCoverage::Complete)])),
            };
            frame
        })
        .collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let replay_source = Arc::new(
        dashboard_base::Source::from_replay(
            dashboard_base::SourceKey { source_id: "choice-first-break".into(), revision: "1".into() },
            "synthetic.battleplan".into(),
            Arc::new(replay),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let cancel = AtomicBool::new(false);
    let context = LoadContext { content: None };
    let choice_at = |hour: i32| {
        EventChoicesTail::load(
            &replay_source,
            &ScriptTailParams {
                branch_id: "1".into(),
                hour: Some(hour),
                from: None,
                to: None,
                countries: None,
                group: None,
                search: None,
                row: None,
                sort: None,
                offset: None,
                limit: 16,
                events: None,
                statuses: None,
            },
            &context,
            &cancel,
        )
        .unwrap()
    };
    let feed_at = |hour: i32| {
        EventFeedTail::load(
            &replay_source,
            &EventFeedTailParams {
                branch_id: "1".into(),
                hour: Some(hour),
                from: None,
                to: None,
                countries: None,
                group: None,
                search: None,
                row: None,
                sort: None,
                offset: None,
                limit: 16,
                categories: vec![EventFeedCategory::Politics],
                exclude: None,
                expanded: None,
                full: None,
                place_names: None,
            },
            &context,
            &cancel,
        )
        .unwrap()
    };
    assert_eq!((choice_at(109).rows[0].not_recorded_reason.as_deref(), choice_at(109).rows[0].not_recorded_hour), (None, None));
    assert_eq!((choice_at(110).rows[0].not_recorded_reason.as_deref(), choice_at(110).rows[0].not_recorded_hour), (Some("event feed read in part"), Some(110)));
    assert_eq!(
        (choice_at(115).rows[0].not_recorded_reason.as_deref(), choice_at(115).rows[0].not_recorded_hour),
        (Some("event feed read in part"), Some(110)),
        "a later pause must not replace the earlier partial-read cause or hour"
    );
    let feed_115 = feed_at(115);
    let feed_reason = match &feed_115.rows[0] {
        super::EventFeedLine::Event(row) => row.not_recorded_reason.as_deref(),
        other => panic!("expected event line, got {other:?}"),
    };
    assert_eq!(feed_reason, Some("event feed read in part"));
}

#[test]
fn activity_bins_and_table_rows_count_the_same_resolved_countries_after_renames_and_membership_changes() {
    use super::{EventFeedActivity, EventFeedCategory, EventFeedTail, EventFeedTailParams, tails::EventFeedActivityParams};
    use dashboard_base::{products::{LoadContext, Product}, test_fixtures::{CAMPAIGN, records, source}};
    use dashboard_query::{identity::Country, series::ranked::{GroupKind, GroupMembership, GroupSelection}};
    use replay_data::{
        event_feed_rows::{EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript},
        rows::{CountryLinkRow, CountryRow, FactionMemberRow, FactionRow, HumanControllerRow, Recorded, TableRows, WarRelationRow},
    };
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let frames = (100..=143).map(|hour| {
        let mut frame = source(1, hour, None);
        let member = (110..133).contains(&hour);
        *frame.tables[TableId::Countries.index()] = TableView::Complete(TableRows::Countries(vec![
            CountryRow { id: 9001, canonical_slot: 7, tag: if hour < 120 { b"AAA".to_vec() } else { b"BBB".to_vec() }, ..Default::default() },
            CountryRow { id: 9002, canonical_slot: 8, tag: b"CCC".to_vec(), ..Default::default() },
        ]));
        *frame.tables[TableId::CountryLinks.index()] = TableView::Complete(TableRows::CountryLinks(vec![
            CountryLinkRow { id: 9001, canonical_slot: 7 }, CountryLinkRow { id: 9002, canonical_slot: 8 },
        ]));
        *frame.tables[TableId::Factions.index()] = TableView::Complete(TableRows::Factions(vec![FactionRow { id: 31, name: b"Probe group".to_vec(), ..Default::default() }]));
        *frame.tables[TableId::FactionMembers.index()] = TableView::Complete(TableRows::FactionMembers(if member { vec![FactionMemberRow { country: 9001, faction: 31, ordinal: 0 }] } else { Vec::new() }));
        *frame.tables[TableId::HumanControllers.index()] = TableView::Complete(TableRows::HumanControllers(if member { vec![HumanControllerRow { country: 9001, count: 1 }] } else { Vec::new() }));
        *frame.tables[TableId::WarRelations.index()] = TableView::Complete(TableRows::WarRelations(if member { vec![WarRelationRow { first_country: 9001, second_country: 9002, start_hour: 110, end_hour: 133, ..Default::default() }] } else { Vec::new() }));
        let events = if [101, 113, 125, 137].contains(&hour) {
            vec![EventFeedEvent { sequence: hour as u64, generation: 1, hour, source: EventFeedSource::DecisionSelected,
                actor: EventFeedCountry { reference: 9001, id: 9001, instance: 1 }, target: EventFeedCountry::default(), cause: Recorded::NotRecorded,
                script: Some(EventFeedScript { token_len: 14, ..Default::default() }) }]
        } else { Vec::new() };
        let count = events.len() as u64;
        frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
        *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
            generation: 1, read_hour: hour, coverage: EventFeedCoverage::Complete,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }), events,
            script_tokens: b"probe_decision".to_vec(), ..Default::default()
        }]));
        frame
    }).collect();
    let replay = replay::campaign::Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap().into_timelines(&Default::default()).unwrap();
    let held = Arc::new(dashboard_base::Source::from_replay(dashboard_base::SourceKey { source_id: "activity-selection".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap());
    let context = LoadContext { content: None };
    let cancel = AtomicBool::new(false);
    let mut cases = vec![
        ("saved tag", Some(vec![Country::Slot(7), Country::Tag("AAA".into())]), None, 4),
        ("selected slot", Some(vec![Country::Slot(7)]), None, 4),
        ("all", None, None, 4),
    ];
    for kind in [GroupKind::Humans, GroupKind::Faction, GroupKind::War] {
        for membership in [GroupMembership::Hour, GroupMembership::Ever] {
            let group = GroupSelection { kind, membership, id: (kind == GroupKind::Faction).then(|| "31".into()), tag: (kind == GroupKind::War).then(|| "CCC".into()) };
            cases.push(("dated group", None, Some(group), if membership == GroupMembership::Hour { 2 } else { 3 }));
        }
    }
    for (label, countries, group, expected) in cases {
        let table = EventFeedTail::load(&held, &EventFeedTailParams { branch_id: "1".into(), hour: Some(143), from: Some(100), to: Some(143),
            countries: countries.clone(), group: group.clone(), search: None, row: None, sort: None, offset: None, limit: 64,
            categories: vec![EventFeedCategory::Politics], exclude: None, expanded: None, full: None, place_names: None }, &context, &cancel).unwrap();
        let strip = EventFeedActivity::load(&held, &EventFeedActivityParams { branch_id: "1".into(), hour: 143, from: 100, bin_hours: 12,
            categories: vec![EventFeedCategory::Politics], exclude: None, countries: countries.clone(), group: group.clone() }, &context, &cancel).unwrap();
        let total: u64 = strip.lines.iter().flat_map(|line| &line.bins).map(|&(_, count)| count).sum();
        assert_eq!(total, table.total.parse::<u64>().unwrap(), "{label}: the same population");
        assert_eq!(total, expected, "{label}: nonempty before and after the identity or membership change");
        assert!(strip.missing.is_empty(), "{label}: recorded selection");
        let request = serde_json::json!({
            "panel": "event-feed-activity",
            "branchId": "1",
            "binHours": 12,
            "categories": ["politics"],
            "countries": countries,
            "group": group,
        });
        let panel = super::event_feed_activity_panel(&held, &context, &request).unwrap();
        let exact = panel.exact().unwrap();
        for hour in [100, 113, 133, 143, 114] {
            let mut asked = request.clone();
            let fields = asked.as_object_mut().unwrap();
            fields.remove("panel");
            fields.insert("hour".into(), hour.into());
            fields.insert("from".into(), 100.into());
            let expected = crate::registrations::FEED_EVENTFEEDACTIVITY.answer(&held, asked, &context, &cancel).unwrap();
            let streamed = (exact.advance)(hour, false, &cancel, None);
            assert_eq!(streamed.value.as_ref().as_ref().unwrap(), &expected, "{label} at hour {hour}");
        }
    }
}

#[test]
fn faction_row_with_missing_dated_name_never_falls_back_to_final_name() {
    use super::{EventFeedCategory, EventFeedTail, EventFeedTailParams};
    use dashboard_base::{
        products::{LoadContext, Product},
        test_fixtures::{CAMPAIGN, records, source},
    };
    use replay::campaign::Replay;
    use replay_data::rows::{FactionMemberRow, FactionRow, TableRows};
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let frames: Vec<_> = (100..=104)
        .map(|hour| {
            let mut frame = source(1, hour, Some(1));
            let factions = if hour >= 101 {
                let axis = FactionRow { id: 1, name: if hour >= 103 { b"Axis".to_vec() } else { Vec::new() }, ..Default::default() };
                let templated = FactionRow { id: 2, template_token: Some(77), ..Default::default() };
                if hour >= 103 { vec![axis, templated] } else { vec![axis] }
            } else {
                Vec::new()
            };
            let members = if hour >= 101 {
                vec![FactionMemberRow { faction: 1, country: 9001, ordinal: 0 }]
            } else {
                Vec::new()
            };
            *frame.tables[TableId::Factions.index()] = TableView::Complete(TableRows::Factions(factions));
            *frame.tables[TableId::FactionMembers.index()] = TableView::Complete(TableRows::FactionMembers(members));
            frame
        })
        .collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let replay_source = dashboard_base::Source::from_replay(
        dashboard_base::SourceKey { source_id: "faction-dated-name".into(), revision: "1".into() },
        "synthetic.battleplan".into(),
        Arc::new(replay),
        &AtomicBool::new(false),
    )
    .unwrap();
    let query = dashboard_base::queries::query(&replay_source, 1).unwrap();
    let transitions = super::transitions::load_branch(&query, &AtomicBool::new(false)).unwrap();
    let reply = dashboard_world::products::groups::shared(&replay_source, &query, &LoadContext { content: None }, &AtomicBool::new(false)).unwrap();
    let (made, notes, _, _) = families::group_rows(Some(&reply), None, &transitions, &[], (100, 104), &[super::EventFeedCategory::Diplomacy]);
    let rows: Vec<_> = made.into_iter().map(|entry| entry.row).collect();
    let joined = rows.iter().find(|row| row.kind == EventFeedKind::FactionJoined).expect("faction join row at hour 101");
    assert_eq!(joined.hour, 101);
    let item = joined.item.as_ref().expect("faction item");
    assert_eq!(item.name, None, "must not fall back to the faction's later name when unrecorded at the join hour");
    assert_eq!(item.token, None, "the content identity is the join hour's recorded literal, never the branch's last");
    assert!(item.name_missing.is_some(), "missing dated faction name must carry a reason");
    assert!(notes.iter().any(|(hour, note)| *hour == 103 && note.contains("template token 77")), "{notes:?}");
    let ask = |hour: i32| EventFeedTailParams {
        branch_id: "1".into(),
        hour: Some(hour),
        from: None,
        to: None,
        countries: None,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        categories: vec![EventFeedCategory::Diplomacy],
        exclude: None,
        expanded: None,
        full: None,
        place_names: None,
    };
    let at = |hour: i32| EventFeedTail::load(&replay_source, &ask(hour), &LoadContext { content: None }, &AtomicBool::new(false)).unwrap();
    let early = at(101);
    assert!(!early.warnings.iter().any(|warning| warning.contains("template token 77")), "a refusal first read at 103 is not in the reply at 101: {:?}", early.warnings);
    let joined_key = early
        .rows
        .iter()
        .find_map(|line| match line {
            super::EventFeedLine::Event(row) if row.row.as_ref().is_some_and(|row| row.kind == EventFeedKind::FactionJoined) => Some(row.key.clone()),
            _ => None,
        })
        .expect("faction join listed at 101");
    let opened = |hour: i32| EventFeedTail::load(&replay_source, &EventFeedTailParams { row: Some(joined_key.clone()), limit: 1, ..ask(hour) }, &LoadContext { content: None }, &AtomicBool::new(false)).unwrap();
    let opened_keys: Vec<_> = opened(104)
        .rows
        .into_iter()
        .map(|line| match line {
            super::EventFeedLine::Event(row) => row.key.clone(),
            other => panic!("expected event line, got {other:?}"),
        })
        .collect();
    assert_eq!(opened_keys, [joined_key.as_str()], "a family row opens by its key");
    assert!(opened(100).rows.is_empty(), "and is not in the log before it was observed");
    assert!(early.next_change.is_some_and(|next| next <= 103));
    assert!(at(104).warnings.iter().any(|warning| warning.contains("template token 77")));
}

#[test]
fn notices_kept_with_each_presented_event_add_up_to_the_notices_said_at_once() {
    let say = |notices: &mut Notices, event: u32| {
        notices.sampled("nativeEventCountry", "Native event country", format!("actor of event {event}"), 1);
        notices.sampled("nativeEventCountry", "Native event country", format!("target of event {event}"), 1);
        notices.unavailable("decisionIcon", "Decision icon", "the icon is not in the installation", Some(2));
        notices.unavailable("eventWindows", "Event windows", format!("from read {}", event % 2), None);
    };
    let mut direct = Notices::default();
    let mut kept = Notices::default();
    for event in 0..5 {
        say(&mut direct, event);
        let mut own = Notices::default();
        say(&mut own, event);
        kept.absorb(&own);
    }
    assert_eq!(kept.into_list(), direct.into_list(), "a reply built from kept rows says what presenting them again would");
}

#[test]
fn a_listed_row_leaves_unexpanded_text_reasons_and_unshown_states_to_the_opened_row() {
    let title_gap = super::EventFeedGap {
        field: super::EventFeedGapField::Title,
        ordinal: None,
        key: "FROM.GetName".into(),
        reason: "event scope 1: targeted decision country has no exact occurrence-hour slot".into(),
    };
    let body_gap = super::EventFeedGap {
        field: super::EventFeedGapField::Description,
        ordinal: None,
        key: "ROOT.GetLeader".into(),
        reason: "reads more than an event scope's country name or occurrence date".into(),
    };
    let row = super::EventFeedRow {
        id: "native:1:9".into(),
        hour: 100,
        read_hour: 100,
        name_hour: 100,
        exact: true,
        category: super::EventFeedCategory::Politics,
        kind: EventFeedKind::DecisionActivated,
        tags: Vec::new(),
        participants: None,
        participants_missing: None,
        owner_missing: None,
        item: Some(super::EventFeedItem {
            name: Some(format!("Build in {}", dashboard_text::scripted_text::SCRIPTED_GAP)),
            description: Some(format!("Led by {}.", dashboard_text::scripted_text::SCRIPTED_GAP)),
            gaps: vec![title_gap.clone(), body_gap.clone()],
            ..Default::default()
        }),
        amount: None,
        cause: None,
        strike: None,
        decision_state: Some(super::decision_state::DecisionLifecycleState::NotApplicable { reason: super::decision_state::DECISION_NO_MISSION_LIFECYCLE.to_owned() }),
        window: None,
    };
    let mut decisions = [Some(row.clone())];
    super::tails::listed(&mut decisions, true, true);
    let listed = decisions[0].as_ref().unwrap();
    let item = listed.item.as_ref().unwrap();
    assert_eq!(item.gaps, [title_gap.clone(), body_gap], "the list keeps gaps for every field it keeps");
    assert_eq!(listed.decision_state, None, "a state no list shows is left to the opened row");
    let mut feed = [Some(row)];
    super::tails::listed(&mut feed, false, false);
    let feed_item = feed[0].as_ref().unwrap().item.as_ref().unwrap();
    assert_eq!(feed_item.description, None);
    assert_eq!(feed_item.gaps, [title_gap], "clearing description drops only description gaps, keeping title gaps on the listed row");
}

#[test]
fn scripted_feed_text_keeps_resolved_parts_dates_and_every_gap_across_events_and_decisions() {
    use super::{EventFeedCategory, EventFeedGapField, EventFeedTail, EventFeedTailParams};
    use dashboard_base::{
        products::{Finish, LoadContext, Product},
        test_fixtures::{source, visual_mods::{Machine, RED, png}},
    };
    use dashboard_text::scripted_text::SCRIPTED_GAP;
    use replay_data::{
        event_feed_rows::{EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScope, EventFeedScript, EventFeedText},
        rows::{CountryAppearanceRow, CountryLinkRow, CountryRow, PoliticalPartyRow, PoliticsRow, Recorded, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::atomic::AtomicBool;

    let machine = Machine::new("feed_scripted_gaps");
    machine.write("common/defines/fixture.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}");
    machine.write("common/decisions/categories/fixture.txt", b"SYNTH_CAT={}");
    machine.write(
        "common/decisions/fixture.txt",
        b"SYNTH_CAT={\n  synth_build={allowed={always=yes}}\n  synth_trade={allowed={always=yes} highlight_states={highlight_states_trigger={}}}\n  synth_orders={allowed={always=yes} highlight_states={highlight_states_trigger={unregistered_trigger_key=yes}}}\n}\n",
    );
    machine.write(
        "events/synth_events.txt",
        b"add_namespace = synth\ncountry_event = {\n  id = synth.1\n  title = synth.1.t\n  desc = synth.1.d\n  picture = GFX_synth\n  option = { name = synth.1.a }\n  option = { name = synth.1.b }\n}\n",
    );
    machine.write(
        "localisation/english/synth_l_english.yml",
        "\u{feff}l_english:\n GER:0 \"Synthland\"\n GER_DEF:0 \"the Synthland\"\n GER_ADJ:0 \"Synth\"\n SOV:0 \"Neighboria\"\n SOV_DEF:0 \"the Neighboria\"\n SOV_ADJ:0 \"Neighborian\"\n synth.1.t:0 \"[ROOT.GetName] summit of [GetYear]: [ROOT.GetLeader] and [FROM.GetName]\"\n synth.1.d:0 \"In [GetYear], [ROOT.GetLeader] met [FROM.GetRulingParty].\"\n synth.1.a:0 \"Back [ROOT.GetLeader] in [GetYear]\"\n synth.1.b:0 \"Ask [FROM.GetName]\"\n synth_build:0 \"Build in [FROM.GetName] ([FROM.ROOT.GetName]) for [ROOT.GetName] ([GetYear])\"\n synth_trade:0 \"Trade with [FROM.GetName] ([FROM.ROOT.GetName]) from [ROOT.GetName] ([GetYear])\"\n synth_orders:0 \"Orders for [ROOT.GetName] in [GetYear] ([FROM.GetName])\"\n".as_bytes(),
    );

    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_synth textureFile=gfx/synth.png}}");
    machine.write("gfx/synth.png", &png(RED));

    let hour = game_model::calendar::HOUR_EPOCH + 1939 * 8760;
    let actor = EventFeedCountry { reference: 9001, id: 9001, instance: 1 };
    let mut tokens = Vec::new();
    let push_token = |tokens: &mut Vec<u8>, literal: &[u8]| -> (u32, u32) {
        let start = tokens.len() as u32;
        tokens.extend_from_slice(literal);
        (start, literal.len() as u32)
    };
    let (ev_start, ev_len) = push_token(&mut tokens, b"synth.1");
    let (dec_start, dec_len) = push_token(&mut tokens, b"synth_build");
    let (trade_start, trade_len) = push_token(&mut tokens, b"synth_trade");
    let (orders_start, orders_len) = push_token(&mut tokens, b"synth_orders");
    let scopes = vec![
        EventFeedScope {
            country: 9001,
            country_id: Some(9001),
            country_attempted: true,
            country_names: Some([
                EventFeedText::new(b"Synthland").unwrap(),
                EventFeedText::new(b"the Synthland").unwrap(),
                EventFeedText::new(b"Synth").unwrap(),
            ]),
            bindings: [0, 1, 0],
            ..EventFeedScope::default()
        },
        EventFeedScope {
            country: 9999,
            country_attempted: true,
            bindings: [0, 0, 0],
            ..EventFeedScope::default()
        },
    ];
    let events = vec![
        EventFeedEvent {
            sequence: 1,
            generation: 1,
            hour,
            source: EventFeedSource::ScriptEvent,
            actor,
            target: actor,
            cause: Recorded::NotRecorded,
            script: Some(EventFeedScript {
                token_start: ev_start,
                token_len: ev_len,
                scope_start: 0,
                scope_len: scopes.len() as u32,
                ..EventFeedScript::default()
            }),
        },
        EventFeedEvent {
            sequence: 2,
            generation: 1,
            hour,
            source: EventFeedSource::DecisionSelected,
            actor,
            target: EventFeedCountry::default(),
            cause: Recorded::NotRecorded,
            script: Some(EventFeedScript {
                token_start: dec_start,
                token_len: dec_len,
                country_target: Some(9999),
                ..EventFeedScript::default()
            }),
        },
        EventFeedEvent {
            sequence: 3,
            generation: 1,
            hour,
            source: EventFeedSource::DecisionSelected,
            actor,
            target: EventFeedCountry::default(),
            cause: Recorded::NotRecorded,
            script: Some(EventFeedScript {
                token_start: trade_start,
                token_len: trade_len,
                country_target: Some(9002),
                ..EventFeedScript::default()
            }),
        },
        EventFeedEvent {
            sequence: 4,
            generation: 1,
            hour,
            source: EventFeedSource::DecisionSelected,
            actor,
            target: EventFeedCountry::default(),
            cause: Recorded::NotRecorded,
            script: Some(EventFeedScript {
                token_start: orders_start,
                token_len: orders_len,
                ..EventFeedScript::default()
            }),
        },
    ];
    let mut frame = source(1, hour, Some(1));
    if let TableView::Complete(TableRows::Countries(countries)) = &mut *frame.tables[TableId::Countries.index()] {
        countries.push(CountryRow {
            id: 9002,
            canonical_slot: 8,
            tag: b"SOV".to_vec(),
            border_land_doctrine_level: Recorded::Value(None),
            ..CountryRow::default()
        });
    }
    if let TableView::Complete(TableRows::CountryLinks(links)) = &mut *frame.tables[TableId::CountryLinks.index()] {
        links.push(CountryLinkRow { id: 9002, canonical_slot: 8 });
    }
    *frame.tables[TableId::CountryAppearance.index()] = TableView::Complete(TableRows::CountryAppearance(vec![
        CountryAppearanceRow {
            country: 9001,
            name_tag: b"GER".to_vec(),
            ..Default::default()
        },
        CountryAppearanceRow {
            country: 9002,
            name_tag: b"SOV".to_vec(),
            ..Default::default()
        },
    ]));
    *frame.tables[TableId::Politics.index()] = TableView::Complete(TableRows::Politics(vec![
        PoliticsRow {
            country: 9001,
            ruling_party: Some(3),
            ..Default::default()
        },
        PoliticsRow {
            country: 9002,
            ruling_party: Some(4),
            ..Default::default()
        },
    ]));
    *frame.tables[TableId::PoliticalParties.index()] = TableView::Complete(TableRows::PoliticalParties(vec![
        PoliticalPartyRow {
            country: 9001,
            id: 3,
            token: b"fascism".to_vec(),
            tag: 9001,
            name_default: 1,
            ..Default::default()
        },
        PoliticalPartyRow {
            country: 9002,
            id: 4,
            token: b"communism".to_vec(),
            tag: 9002,
            name_default: 1,
            ..Default::default()
        },
    ]));
    *frame.tables[TableId::Autonomy.index()] = TableView::Complete(TableRows::Autonomy(Vec::new()));
    frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
    *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
        generation: 1,
        read_hour: hour,
        coverage: EventFeedCoverage::Complete,
        counts: Some(EventFeedCounts { acquired: 4, settled: 4, failed: 0, rejected: 0 }),
        outstanding: 0,
        failure: None,
        script_refusals: 0,
        events,
        script_tokens: tokens,
        script_scopes: scopes,
        windows: Default::default(),
    }]));
    let (replay_source, options) = machine.open_frames(vec![frame.clone()]);
    let (prepared_source, prepared_options) = machine.open_frames(vec![frame]);
    let prepared_source = std::sync::Arc::new(prepared_source);
    let prepared_load = LoadContext { content: Some(prepared_options) };
    let context = LoadContext { content: Some(options) };
    let kinds = super::EventFeedKinds::load(&replay_source, &dashboard_base::products::BranchParams { branch_id: "1".into() }, &context, &AtomicBool::new(false)).unwrap();
    let project = kinds.kinds.iter().find(|item| item.kind == EventFeedKind::ProjectFinished).unwrap();
    assert_eq!(project.first_observed, None);
    assert_eq!(project.unavailable.as_deref(), Some("missing NDefines.NCharacter.OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU"));
    let ask = |row: Option<&str>| EventFeedTailParams {
        branch_id: "1".into(),
        hour: Some(hour),
        from: None,
        to: None,
        countries: None,
        group: None,
        search: None,
        row: row.map(str::to_owned),
        sort: None,
        offset: None,
        limit: 64,
        categories: vec![EventFeedCategory::Politics],
        exclude: None,
        expanded: None,
        full: None,
        place_names: None,
    };

    fn event_item<'a>(reply: &'a super::EventFeedTailReply, id: &str) -> &'a super::EventFeedItem {
        reply
            .rows
            .iter()
            .filter_map(|line| match line {
                super::EventFeedLine::Event(row) => row.row.as_ref(),
                _ => None,
            })
            .find(|row| row.id == id)
            .unwrap()
            .item
            .as_ref()
            .unwrap()
    }
    let cancelled = AtomicBool::new(false);
    let (finish, ()) = Finish::open(&replay_source, &context, true, || ());
    let counts = || (
        content_reader_events::events::EVENT_FILE_READS.with(|reads| reads.replace(0)),
        super::text::ART_RESOLUTIONS.with(|draws| draws.replace(0)),
    );
    counts();
    let listed = finish.reply(|| EventFeedTail::load(&replay_source, &ask(None), &context, &cancelled)).unwrap();
    let (files, drawings) = counts();
    assert!(files > 0 && drawings > 0, "cold: {files} files, {drawings} drawings");

    let panel = super::event_feed_dated_panel(&prepared_source, &prepared_load, &serde_json::json!({
        "panel": "event-feed-tail", "branchId": "1", "limit": 64, "categories": ["politics"],
    })).unwrap();
    super::assets::held(&prepared_source, 1, &prepared_load).unwrap().wait().unwrap();
    let followed = (panel.exact().unwrap().advance)(hour, true, &cancelled, None);
    let followed = followed.value.as_ref().as_ref().unwrap();
    assert_eq!(counts(), (0, 0), "prepared step");
    assert_eq!(*followed, serde_json::to_value(&listed).unwrap());
    let ev_listed = event_item(&listed, "native:1:1");
    assert_eq!(ev_listed.name.as_deref(), Some(format!("Synthland summit of 1939: {SCRIPTED_GAP} and {SCRIPTED_GAP}").as_str()));
    assert_eq!(ev_listed.gaps.len(), 2, "listed row keeps both title gaps");
    assert_eq!(ev_listed.gaps[0].key, "ROOT.GetLeader");
    assert_eq!(ev_listed.gaps[1].key, "FROM.GetName");
    assert!(ev_listed.gaps[1].reason.contains("9999"));

    let opened = EventFeedTail::load(&replay_source, &ask(Some("native:1:1")), &context, &AtomicBool::new(false)).unwrap();
    let ev_open = event_item(&opened, "native:1:1");
    assert_eq!(ev_open.gaps.len(), 6, "opened row carries title (2) + body (2) + option 0 (1) + option 1 (1) gaps");
    assert_eq!(
        ev_open.gaps.iter().map(|gap| (gap.field, gap.ordinal, gap.key.as_str())).collect::<Vec<_>>(),
        [
            (EventFeedGapField::Title, None, "ROOT.GetLeader"),
            (EventFeedGapField::Title, None, "FROM.GetName"),
            (EventFeedGapField::Description, None, "ROOT.GetLeader"),
            (EventFeedGapField::Description, None, "FROM.GetRulingParty"),
            (EventFeedGapField::Option, Some(0), "ROOT.GetLeader"),
            (EventFeedGapField::Option, Some(1), "FROM.GetName"),
        ]
    );
    let body = ev_open.description.as_deref().unwrap();
    assert!(body.starts_with(&format!("In 1939, {SCRIPTED_GAP} met {SCRIPTED_GAP}.")), "{body}");
    assert!(body.contains("0: Back [not recorded: ROOT.GetLeader:"), "{body}");
    assert!(body.contains("1: Ask [not recorded: FROM.GetName:"), "{body}");

    let dec_listed = event_item(&listed, "native:1:2");
    assert_eq!(dec_listed.name.as_deref(), Some(format!("Build in {SCRIPTED_GAP} ({SCRIPTED_GAP}) for Synthland (1939)").as_str()), "missing={:?}", dec_listed.name_missing);
    assert_eq!(dec_listed.gaps.len(), 2);
    assert_eq!(dec_listed.gaps[0].field, EventFeedGapField::Title);
    assert_eq!(dec_listed.gaps[0].key, "FROM.GetName");
    assert!(dec_listed.gaps[0].reason.contains("targeted decision country has no exact occurrence-hour slot"), "{:?}", dec_listed.gaps[0]);
    assert_eq!(dec_listed.gaps[1].key, "FROM.ROOT.GetName");
    assert!(dec_listed.gaps[1].reason.contains("targeted decision country has no exact occurrence-hour slot"), "{:?}", dec_listed.gaps[1]);

    let trade_listed = event_item(&listed, "native:1:3");
    assert_eq!(trade_listed.name.as_deref(), Some("Trade with Neighboria (Neighboria) from Synthland (1939)"), "missing={:?}", trade_listed.name_missing);
    assert!(trade_listed.gaps.is_empty(), "{:?}", trade_listed.gaps);

    let orders_listed = event_item(&listed, "native:1:4");
    assert_eq!(orders_listed.name.as_deref(), Some(format!("Orders for Synthland in 1939 ({SCRIPTED_GAP})").as_str()), "missing={:?}", orders_listed.name_missing);
    assert_eq!(orders_listed.gaps.len(), 1);
    assert_eq!(orders_listed.gaps[0].key, "FROM.GetName");
    assert!(orders_listed.gaps[0].reason.contains("unregistered_trigger_key"), "{:?}", orders_listed.gaps[0]);
}

#[test]
fn feed_kinds_name_missing_project_history_after_content_prerequisites_are_met() {
    use dashboard_base::{products::{BranchParams, LoadContext, Product}, test_fixtures::visual_mods::Machine};
    use std::sync::atomic::AtomicBool;

    let machine = Machine::new("feed_project_history_gap");
    machine.write("common/defines/fixture.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NCharacter={OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU={}}}");
    let (source, options) = machine.open();
    let kinds = super::EventFeedKinds::load(&source, &BranchParams { branch_id: "1".into() }, &LoadContext { content: Some(options) }, &AtomicBool::new(false)).unwrap();
    let project = kinds.kinds.iter().find(|item| item.kind == EventFeedKind::ProjectFinished).unwrap();
    assert_eq!(project.first_observed, None);
    assert_eq!(project.unavailable.as_deref(), Some("Events of this kind may not be listed: project histories were not recorded then."));
}

#[test]
fn a_listed_choice_card_names_the_option_its_last_choice_took() {
    let chosen = |option: Option<i32>, kind: EventFeedKind| super::cards::ScriptEdge {
        row: super::EventFeedRow {
            id: "native:1:1".into(),
            hour: 100,
            read_hour: 100,
            name_hour: 100,
            exact: true,
            category: super::EventFeedCategory::Politics,
            kind,
            tags: Vec::new(),
            participants: None,
            participants_missing: None,
            owner_missing: None,
            item: None,
            amount: None,
            cause: None,
            strike: None,
            decision_state: None,
            window: None,
        },
        option,
        lifecycle: None,
        days: None,
        costs: None,
        costs_unread: Vec::new(),
    };
    let select = EventFeedKind::EventOptionSelected;
    assert_eq!(super::tails::listed_choice(&[chosen(Some(-1), select), chosen(Some(1), select)]), Some(1), "immediate effects are not the choice");
    assert_eq!(super::tails::listed_choice(&[chosen(Some(0), select), chosen(Some(2), select)]), Some(2), "the last choice is the one shown");
    assert_eq!(super::tails::listed_choice(&[chosen(Some(1), select), chosen(Some(3), EventFeedKind::ScriptEventFired)]), Some(1));
    assert_eq!(super::tails::listed_choice(&[chosen(Some(-1), select)]), None);
}

#[test]
fn a_kept_row_says_again_which_mod_its_art_came_from() {
    use super::{DecisionsTail, ScriptTailParams};
    use dashboard_base::{
        catalogue::{self, OwnRefusals},
        products::{LoadContext, Product},
        test_fixtures::{
            source,
            visual_mods::{BLUE, Machine, RED, first_pixel, png, select},
        },
    };
    use replay_data::{
        event_feed_rows::{EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript},
        rows::{Recorded, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::atomic::AtomicBool;

    let machine = Machine::new("feed_kept_fallback");
    machine.write("common/defines/fixture.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}");
    machine.write("common/decisions/categories/fixture.txt", b"FIXTURE_CATEGORY={}");
    machine.write("common/decisions/fixture.txt", b"FIXTURE_CATEGORY={fixture_decision={allowed={always=yes}}}");
    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_decision_fixture_decision textureFile=gfx/fixture_decision.png}}");
    machine.write("gfx/fixture_decision.png", &png(RED));
    machine.write_mod("fixture_art", "gfx/fixture_decision.png", &png(BLUE));
    let token = b"fixture_decision";
    let event = EventFeedEvent {
        sequence: 1,
        generation: 1,
        hour: 100,
        source: EventFeedSource::DecisionSelected,
        actor: EventFeedCountry { reference: 9001, id: 9001, instance: 1 },
        target: EventFeedCountry::default(),
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript { token_start: 0, token_len: token.len() as u32, ..EventFeedScript::default() }),
    };
    let mut frame = source(1, 100, Some(1));
    frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
    *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
        generation: 1,
        read_hour: 100,
        coverage: EventFeedCoverage::Complete,
        counts: Some(EventFeedCounts { acquired: 1, settled: 1, failed: 0, rejected: 0 }),
        outstanding: 0,
        failure: None,
        script_refusals: 0,
        events: vec![event],
        script_tokens: token.to_vec(),
        script_scopes: Vec::new(),
        windows: Default::default(),
    }]));
    let (source, mut options) = machine.open_frames(vec![frame]);
    select(&source, &mut options, &["fixture_art"]);
    let context = LoadContext { content: Some(options) };
    let params = ScriptTailParams {
        branch_id: "1".into(),
        hour: Some(100),
        from: None,
        to: None,
        countries: None,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        events: None,
        statuses: None,
    };
    let read = || {
        let mut own = OwnRefusals::default();
        let reply = catalogue::own_refusals(&mut own, || DecisionsTail::load(&source, &params, &context, &AtomicBool::new(false))).unwrap();
        let icon = reply.rows.iter().filter_map(|row| row.occurrence.as_ref()).flat_map(|occurrence| &occurrence.edges).find_map(|edge| edge.row.item.as_ref().and_then(|item| item.icon.clone()));
        let mut warnings = Vec::new();
        own.diagnostics().append_to(&mut warnings);
        (icon.as_deref().map(first_pixel), warnings)
    };
    let (pixel, cold) = read();
    assert_eq!(pixel, Some(BLUE), "the decision draws the selected mod's icon");
    assert!(!cold.is_empty(), "the first read says which mod the icon came from");
    assert_eq!(read(), (pixel, cold), "a read built from kept rows says it too");
}

#[test]
fn a_followed_decisions_tail_carries_the_warnings_the_exact_read_gives_at_each_hour() {
    use super::decisions_dated_panel;
    use dashboard_base::{
        products::LoadContext,
        test_fixtures::{
            source,
            visual_mods::{BLUE, Machine, RED, png, select},
        },
    };
    use replay_data::{
        event_feed_rows::{EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript},
        rows::{Recorded, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::atomic::AtomicBool;

    let machine = Machine::new("feed_followed_warnings");
    machine.write("common/defines/fixture.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}");
    machine.write("common/decisions/categories/fixture.txt", b"FIXTURE_CATEGORY={}");
    machine.write("common/decisions/fixture.txt", b"FIXTURE_CATEGORY={fixture_decision={allowed={always=yes}}}");
    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_decision_fixture_decision textureFile=gfx/fixture_decision.png}}");
    machine.write("gfx/fixture_decision.png", &png(RED));
    machine.write_mod("fixture_art", "gfx/fixture_decision.png", &png(BLUE));
    let token = b"fixture_decision";
    let event = EventFeedEvent {
        sequence: 1,
        generation: 1,
        hour: 100,
        source: EventFeedSource::DecisionSelected,
        actor: EventFeedCountry { reference: 9001, id: 9001, instance: 1 },
        target: EventFeedCountry::default(),
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript { token_start: 0, token_len: token.len() as u32, ..EventFeedScript::default() }),
    };
    let frames = (99..=101)
        .map(|hour| {
            let events = if hour == 100 { vec![event.clone()] } else { Vec::new() };
            let count = events.len() as u64;
            let mut frame = source(1, hour, Some(1));
            frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
            *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
                generation: 1,
                read_hour: hour,
                coverage: EventFeedCoverage::Complete,
                counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
                outstanding: 0,
                failure: None,
                script_refusals: 0,
                events,
                script_tokens: token.to_vec(),
                script_scopes: Vec::new(),
                windows: Default::default(),
            }]));
            frame
        })
        .collect();
    let (source, mut options) = machine.open_frames(frames);
    select(&source, &mut options, &["fixture_art"]);
    let context = LoadContext { content: Some(options) };
    let cancelled = AtomicBool::new(false);
    let warnings = |reply: &serde_json::Value| -> Vec<String> {
        reply["warnings"].as_array().expect("a reply carries its warnings").iter().map(|line| line.as_str().unwrap().to_owned()).collect()
    };
    let exact = |hour: i32| {
        let asked = serde_json::json!({ "branchId": "1", "hour": hour, "limit": 64 });
        warnings(&crate::registrations::FEED_DECISIONSTAIL.answer(&source, asked, &context, &cancelled).unwrap())
    };
    let panel = decisions_dated_panel(&source, &context, &serde_json::json!({ "panel": "decisions-tail", "branchId": "1", "limit": 64 })).unwrap();
    let followed = panel.exact().unwrap();
    let art = |lines: &[String]| lines.iter().any(|line| line.starts_with("Artwork from visual mods"));
    for hour in 99..=101 {
        let read = (followed.advance)(hour, hour == 99, &cancelled, None);
        let held = warnings(read.value.as_ref().as_ref().unwrap());
        assert_eq!(held, exact(hour), "the followed reply at {hour} warns as the exact read of the same hour does");
        assert_eq!(warnings(&(followed.cold)(hour, &cancelled).unwrap()), held, "and so does its cold read");
        assert_eq!(art(&held), hour >= 100, "the decision's mod art is named from the hour it is listed, at {hour}");
        assert_eq!(read.fresh, hour != 101, "hour 101 holds the reply read at 100, with its warnings");
    }
    super::index::forget(&source);
    assert!(!art(&exact(99)), "a cold exact read before the decision does not credit its art");
    assert!(art(&exact(100)), "the exact read credits the art once the decision is listed");
}

#[test]
fn event_feed_folds_runs_on_the_server_across_pages_expansion_playback_filters_and_breaks() {
    use super::{EventFeedCategory, EventFeedLine, EventFeedTail, EventFeedTailParams, event_feed_dated_panel};
    use dashboard_base::{
        products::{LoadContext, Product, occurrences::OccurrenceSort},
        test_fixtures::{CAMPAIGN, records, source, source_with_content, visual_mods::Machine},
    };
    use dashboard_query::log::occurrence::Direction;
    use replay::campaign::Replay;
    use replay_data::{
        event_feed_rows::{
            EventFeedCause, EventFeedCountry, EventFeedCounts, EventFeedCoverage, EventFeedEvent, EventFeedHistoryRow, EventFeedScript, EventPresentation, EventWindows,
        },
        rows::{Recorded, StateRow, TableId, TableRows},
    };
    use replay_frame::frame::TableView;
    use std::sync::{Arc, atomic::AtomicBool};

    let ger = EventFeedCountry { reference: 9001, id: 9001, instance: 1 };
    let missing = EventFeedCountry::default();
    let tokens = b"fold_dec";
    let ev = |sequence: u64, hour: i32, src: EventFeedSource, actor: EventFeedCountry| EventFeedEvent {
        sequence,
        generation: 1,
        hour,
        source: src,
        actor,
        target: EventFeedCountry::default(),
        cause: Recorded::NotRecorded,
        script: Some(EventFeedScript {
            token_start: 0,
            token_len: tokens.len() as u32,
            scope_unrecorded: true,
            ..EventFeedScript::default()
        }),
    };
    let win = |instance: u32, native_hour: Option<i32>| EventPresentation {
        native_hour,
        instance,
        actor: 9001,
        shown: Some(true),
        ..EventPresentation::default()
    };
    let pack = |read_hour: i32, events: Vec<EventFeedEvent>, presentations: Vec<EventPresentation>| {
        let count = events.len() as u64;
        EventFeedHistoryRow {
            generation: 1,
            read_hour,
            coverage: EventFeedCoverage::Complete,
            counts: Some(EventFeedCounts { acquired: count, settled: count, failed: 0, rejected: 0 }),
            outstanding: 0,
            failure: None,
            script_refusals: 0,
            events,
            script_tokens: tokens.to_vec(),
            script_scopes: Vec::new(),
            windows: Recorded::Value(EventWindows { verdicts: Ok(Vec::new()), presentations, presentations_unrecorded: None }),
        }
    };
    let frames: Vec<_> = (96..=170)
        .map(|hour| {
            let mut frame = source(1, hour, Some(1));
            *frame.tables[TableId::States.index()] = TableView::Complete(TableRows::States(vec![StateRow {
                id: 42,
                custom_name: if hour < 106 { b"First Reach".to_vec() } else { b"Second Reach".to_vec() },
                ..Default::default()
            }]));
            frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
            *frame.tables[TableId::EventFeedHistory.index()] = match hour {
                103..=104 => TableView::Unavailable { reason: "recording paused".into() },
                148 => TableView::Unavailable { reason: "gap mid-day".into() },
                _ => {
                    let events = match hour {
                        96 => vec![ev(1, 96, EventFeedSource::DecisionSelected, ger)],
                        97 => vec![ev(2, 97, EventFeedSource::DecisionSelected, ger)],
                        98 => vec![ev(3, 98, EventFeedSource::DecisionSelected, ger)],
                        99 => vec![ev(4, 99, EventFeedSource::DecisionSelected, ger)],
                        100 => vec![ev(5, 100, EventFeedSource::DecisionActivated, ger)],
                        101 => vec![ev(6, 101, EventFeedSource::DecisionActivated, ger)],
                        105 => vec![ev(7, 105, EventFeedSource::ScriptEvent, ger)],
                        106 => vec![EventFeedEvent {
                            script: None,
                            cause: Recorded::Value(EventFeedCause { state: Some(42), ..Default::default() }),
                            ..ev(8, 99, EventFeedSource::StateTransfer, ger)
                        }],
                        120 => (0..35u64).map(|i| ev(100 + i, 120, EventFeedSource::DecisionSelected, ger)).collect(),
                        144 => (0..3u64).map(|i| ev(200 + i, 144, EventFeedSource::DecisionSelected, ger)).collect(),
                        145 => vec![ev(203, 145, EventFeedSource::DecisionSelected, missing)],
                        146 => (0..3u64).map(|i| ev(204 + i, 146, EventFeedSource::DecisionSelected, ger)).collect(),
                        147 => (0..3u64).map(|i| ev(210 + i, 147, EventFeedSource::ScriptEvent, ger)).collect(),
                        149 => (0..3u64).map(|i| ev(213 + i, 149, EventFeedSource::ScriptEvent, ger)).collect(),
                        _ => Vec::new(),
                    };
                    let presentations = match hour {
                        168 => vec![win(1, Some(167)), win(2, Some(167)), win(3, Some(167)), win(4, Some(167)), win(5, Some(168))],
                        170 => vec![win(10, Some(170)), win(11, Some(170)), win(12, Some(170)), win(13, None), win(14, Some(170)), win(15, Some(170)), win(16, Some(170))],
                        _ => Vec::new(),
                    };
                    TableView::Complete(TableRows::EventFeedHistory(vec![pack(hour, events, presentations)]))
                }
            };
            frame
        })
        .collect();
    let dated_frames = frames.iter().take_while(|frame| frame.hour <= 106).cloned().collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let replay_source = Arc::new(
        dashboard_base::Source::from_replay(
            dashboard_base::SourceKey { source_id: "feed-folding".into(), revision: "1".into() },
            "synthetic.battleplan".into(),
            Arc::new(replay),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let cancel = AtomicBool::new(false);
    let context = LoadContext { content: None };
    let ask = |hour: i32| EventFeedTailParams {
        branch_id: "1".into(),
        hour: Some(hour),
        from: None,
        to: None,
        countries: None,
        group: None,
        search: None,
        row: None,
        sort: None,
        offset: None,
        limit: 64,
        categories: vec![EventFeedCategory::Politics],
        exclude: None,
        expanded: None,
        full: None,
        place_names: None,
    };

    let at_99 = EventFeedTail::load(&replay_source, &ask(99), &context, &cancel).unwrap();
    assert_eq!(at_99.total, "4");
    assert!(at_99.rows.iter().all(|line| matches!(line, EventFeedLine::Event(_))), "4 events stay unfolded");

    let at_100 = EventFeedTail::load(&replay_source, &ask(100), &context, &cancel).unwrap();
    assert_eq!(at_100.total, "1");
    let day4_key = match &at_100.rows[0] {
        EventFeedLine::Fold(fold) => {
            assert_eq!((fold.count.as_str(), fold.country.as_str(), fold.kind, fold.hour), ("5", "GER", EventFeedKind::DecisionActivated, 100));
            assert!(fold.first_row.is_some());
            fold.key.clone()
        }
        other => panic!("expected fold at 5th event, got {other:?}"),
    };

    let at_101 = EventFeedTail::load(&replay_source, &EventFeedTailParams { limit: 3, ..ask(101) }, &context, &cancel).unwrap();
    assert_eq!(at_101.total, "1");
    match &at_101.rows[0] {
        EventFeedLine::Fold(fold) => {
            assert_eq!(fold.key, day4_key, "a newer event joining the run keeps the fold key stable");
            assert_eq!((fold.count.as_str(), fold.hour), ("6", 101));
        }
        other => panic!("expected fold at 101, got {other:?}"),
    }

    let filtered = EventFeedTail::load(&replay_source, &EventFeedTailParams { exclude: Some(vec!["decisionActivated".into()]), ..ask(101) }, &context, &cancel).unwrap();
    assert_eq!(filtered.total, "4", "filtering runs before folding: 6 reduced to 4 stays unfolded");
    assert!(filtered.rows.iter().all(|line| matches!(line, EventFeedLine::Event(_))));

    let detail = EventFeedTail::load(&replay_source, &EventFeedTailParams { row: Some("native:1:2".into()), limit: 1, ..ask(101) }, &context, &cancel).unwrap();
    match &detail.rows[0] {
        EventFeedLine::Event(row) => assert_eq!(row.key, "native:1:2"),
        other => panic!("expected single opened event from inside a fold, got {other:?}"),
    }

    let detail_before = EventFeedTail::load(&replay_source, &EventFeedTailParams { row: Some("native:1:7".into()), limit: 1, ..ask(100) }, &context, &cancel).unwrap();
    assert!(detail_before.rows.is_empty());
    assert_eq!(detail_before.next_change, Some(105), "keyed row before observation reports its observation hour as next_change");

    let detail_live = EventFeedTail::load(&replay_source, &EventFeedTailParams { row: Some("native:1:7".into()), limit: 1, ..ask(105) }, &context, &cancel).unwrap();
    assert_eq!((detail_live.rows.len(), detail_live.live.as_str()), (1, "1"));

    let at_105 = EventFeedTail::load(&replay_source, &ask(105), &context, &cancel).unwrap();
    assert_eq!(at_105.total, "2");
    assert_eq!(at_105.missing.len(), 1);
    assert_eq!(at_105.missing[0].before, "1", "gap before count uses folded lines");

    let at_105_open = EventFeedTail::load(&replay_source, &EventFeedTailParams { expanded: Some(vec![day4_key.clone()]), ..ask(105) }, &context, &cancel).unwrap();
    assert_eq!(at_105_open.total, "8");
    assert_eq!(at_105_open.offset, "0");
    assert_eq!(at_105_open.missing[0].before, "1");
    assert_eq!(
        at_105_open.unavailable.iter().find(|item| item.kind == "scriptEventScope").and_then(|item| item.count),
        Some(7),
        "expanded fold header and first child present the shared event once without double-counting notices"
    );
    assert!(matches!(&at_105_open.rows[1], EventFeedLine::Fold(fold) if fold.key == day4_key));
    for (line, sequence) in at_105_open.rows[2..8].iter().zip((1..=6).rev()) {
        match line {
            EventFeedLine::Event(row) => {
                assert_eq!(row.parent.as_deref(), Some(day4_key.as_str()));
                assert_eq!(row.key, format!("native:1:{sequence}"));
            }
            other => panic!("expected expanded child event, got {other:?}"),
        }
    }

    let at_120 = EventFeedTail::load(&replay_source, &ask(120), &context, &cancel).unwrap();
    assert_eq!(at_120.total, "3");
    assert_eq!(at_120.missing[0].before, "2");
    let day5_key = match &at_120.rows[0] {
        EventFeedLine::Fold(fold) => {
            assert_eq!(fold.count, "35");
            fold.key.clone()
        }
        other => panic!("expected day 5 fold, got {other:?}"),
    };
    let at_120_open = EventFeedTail::load(&replay_source, &EventFeedTailParams { expanded: Some(vec![day5_key.clone()]), offset: Some("31".into()), limit: 5, ..ask(120) }, &context, &cancel).unwrap();
    assert_eq!(at_120_open.total, "36");
    assert_eq!(at_120_open.offset, "31");
    assert_eq!(at_120_open.missing[0].before, "35");
    assert_eq!(at_120_open.rows.len(), 5);
    assert!(matches!(&at_120_open.rows[0], EventFeedLine::Event(row) if row.key == "native:1:104" && row.parent.as_deref() == Some(day5_key.as_str())));
    assert!(matches!(&at_120_open.rows[1], EventFeedLine::Event(row) if row.key == "native:1:103" && row.parent.as_deref() == Some(day5_key.as_str())));
    assert!(matches!(&at_120_open.rows[2], EventFeedLine::More(more) if more.parent == day5_key && more.total == "35" && more.remaining == "3"));
    assert!(matches!(&at_120_open.rows[3], EventFeedLine::Event(row) if row.key == "native:1:7" && row.parent.is_none()));
    assert!(matches!(&at_120_open.rows[4], EventFeedLine::Fold(fold) if fold.key == day4_key));

    let at_120_full = EventFeedTail::load(
        &replay_source,
        &EventFeedTailParams { expanded: Some(vec![day5_key.clone()]), full: Some(vec![day5_key.clone()]), offset: Some("35".into()), limit: 4, ..ask(120) },
        &context,
        &cancel,
    )
    .unwrap();
    assert_eq!(at_120_full.total, "39");
    assert_eq!(at_120_full.offset, "35");
    assert_eq!(at_120_full.missing[0].before, "38");
    assert!(matches!(&at_120_full.rows[0], EventFeedLine::Event(row) if row.key == "native:1:100" && row.parent.as_deref() == Some(day5_key.as_str())));
    assert!(matches!(&at_120_full.rows[1], EventFeedLine::More(more) if more.parent == day5_key && more.total == "35" && more.remaining == "0"));

    let asc_open = EventFeedTail::load(
        &replay_source,
        &EventFeedTailParams {
            sort: Some(OccurrenceSort { column: "date".into(), direction: Direction::Ascending }),
            expanded: Some(vec![day5_key.clone()]),
            offset: Some("2".into()),
            limit: 35,
            ..ask(120)
        },
        &context,
        &cancel,
    )
    .unwrap();
    assert!(matches!(&asc_open.rows[0], EventFeedLine::Fold(fold) if fold.key == day5_key));
    let asc_child_keys: Vec<&str> = asc_open.rows[1..33]
        .iter()
        .map(|line| match line {
            EventFeedLine::Event(row) => row.key.as_str(),
            other => panic!("expected child event, got {other:?}"),
        })
        .collect();
    assert_eq!((asc_child_keys[0], asc_child_keys[31]), ("native:1:103", "native:1:134"), "ascending capped expansion shows the newest 32 children");
    assert!(matches!(&asc_open.rows[33], EventFeedLine::More(more) if more.parent == day5_key && more.remaining == "3"));

    let day6 = EventFeedTail::load(&replay_source, &EventFeedTailParams { from: Some(144), ..ask(149) }, &context, &cancel).unwrap();
    assert_eq!(day6.total, "13", "missing-country row and mid-day gap each break a run");
    assert!(day6.rows.iter().all(|line| matches!(line, EventFeedLine::Event(_))));
    assert!(day6.rows.iter().any(|line| matches!(line, EventFeedLine::Event(row) if row.key == "native:1:203")), "missing-country event is never swallowed");
    assert_eq!(day6.missing.len(), 1);
    assert_eq!(day6.missing[0].before, "3");

    let windows_split = EventFeedTail::load(&replay_source, &EventFeedTailParams { from: Some(167), ..ask(168) }, &context, &cancel).unwrap();
    assert_eq!(windows_split.total, "5", "4 windows at hour 167 and 1 at hour 168 read at 168 span two days and do not fold");
    assert!(windows_split.rows.iter().all(|line| matches!(line, EventFeedLine::Event(_))));

    let windows_unclocked = EventFeedTail::load(&replay_source, &EventFeedTailParams { from: Some(169), ..ask(170) }, &context, &cancel).unwrap();
    assert_eq!(windows_unclocked.total, "7", "a presented window with no native clock stays standalone and breaks the run");
    assert!(windows_unclocked.rows.iter().all(|line| matches!(line, EventFeedLine::Event(_))));

    let panel = event_feed_dated_panel(&replay_source, &context, &serde_json::json!({ "panel": "event-feed-tail", "branchId": "1", "limit": 64, "categories": ["politics"] })).unwrap();
    let exact = panel.exact().unwrap();
    for (idx, hour) in [96, 97, 98, 99, 100, 101, 102, 103, 105, 120, 144, 146, 149, 100, 99, 101].into_iter().enumerate() {
        super::index::REQUESTED_EVENT_LOCATORS.set(0);
        let (stepped, reads) = replay_recording::cells::count_reads(|| (exact.advance)(hour, idx == 0, &cancel, None));
        let visited = super::index::REQUESTED_EVENT_LOCATORS.get();
        if idx == 3 {
            assert_eq!(visited, 1, "only the new fourth event is read");
            let opened = reads.get(&TableId::FeedEvents).map_or(0, |feed| feed.opened);
            assert_eq!(opened, 0, "the next page reuses the indexed observation reader");
        }
        if hour == 102 {
            assert!(!stepped.fresh, "quiet hour 102 reuses cached reply");
            assert_eq!(visited, 0, "confirming a quiet projection reads no records");
        }
        if idx == 4 {
            assert!(stepped.fresh, "5th event at hour 100 triggers fresh folded reply");
        }
        let rebuilt = dashboard_base::products::Finish::new(&replay_source, &context, EventFeedTail::CONTENT_DEPENDENT)
            .reply(|| EventFeedTail::load(&replay_source, &ask(hour), &context, &cancel)).unwrap();
        assert_eq!(*stepped.value, Ok(serde_json::to_value(rebuilt).unwrap()), "stepped vs exact product at step {idx} hour {hour}");
    }

    let places = |hour, place_names| EventFeedTailParams { place_names, categories: vec![EventFeedCategory::WarsPeace], ..ask(hour) };
    let before = EventFeedTail::load(&replay_source, &places(105, Some(true)), &context, &cancel).unwrap();
    assert!(before.rows.is_empty(), "the transfer was not observed at hour 105");
    let unnamed = EventFeedTail::load(&replay_source, &places(106, None), &context, &cancel).unwrap();
    let EventFeedLine::Event(transfer) = &unnamed.rows[0] else { panic!("expected the transfer") };
    assert_eq!((transfer.hour, transfer.observed, &transfer.place_missing), (99, 106, &None));
    let refused = EventFeedTail::load(&replay_source, &places(106, Some(true)), &context, &cancel).unwrap();
    let EventFeedLine::Event(transfer) = &refused.rows[0] else { panic!("expected the transfer") };
    assert_eq!(transfer.place_missing.as_deref(), Some("Place names need the recorded content stack and language"));
    let machine = Machine::new("event_feed_places");
    machine.write("localisation/fixture_l_english.yml", b"\xef\xbb\xbfl_english:\n fixture_place:0 \"Fixture place\"\n");
    let (named_source, content) = source_with_content(dated_frames, &machine.root);
    let named_source = Arc::new(named_source);
    let named = dashboard_base::products::Finish::new(&named_source, &content, EventFeedTail::CONTENT_DEPENDENT)
        .reply(|| EventFeedTail::load(&named_source, &places(106, Some(true)), &content, &cancel)).unwrap();
    let EventFeedLine::Event(transfer) = &named.rows[0] else { panic!("expected the transfer") };
    assert_eq!(transfer.place_name.as_deref(), Some("First Reach"));
    let params = serde_json::json!({ "panel": "event-feed-tail", "branchId": "1", "limit": 64, "categories": ["warsPeace"], "placeNames": true });
    let followed = event_feed_dated_panel(&named_source, &content, &params).unwrap();
    if let Some(ready) = dashboard_world::products::place_name_runs::prepare(&named_source, &content).unwrap() { ready.wait().unwrap(); }
    let stepped = (followed.exact().unwrap().advance)(106, true, &cancel, None);
    assert_eq!(*stepped.value, Ok(serde_json::to_value(named).unwrap()));
}

#[test]
fn project_completion_art_warns_when_the_event_or_fold_shows_it() {
    use dashboard_base::{
        products::LoadContext,
        test_fixtures::visual_mods::{BLUE, Machine, RED, first_pixel, png, select},
    };
    use dashboard_research::products::special_projects::test_fixtures::completion_frame;
    use replay_data::{
        event_feed_rows::{EventFeedCounts, EventFeedCoverage, EventFeedHistoryRow},
        rows::{LexicalTokenRow, TableRows},
    };
    use replay_frame::frame::TableView;
    use serde_json::{Value, json};
    use std::sync::{Arc, atomic::AtomicBool};

    let machine = Machine::new("feed_completion_art");
    machine.write("common/defines/fixture.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NCharacter={OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU={}}}");
    machine.write("common/special_projects/projects/fixture.txt", b"fixture_project={icon=GFX_fixture_mod} fixture_other={icon=GFX_fixture_base}");
    machine.write("localisation/fixture_l_english.yml", b"\xef\xbb\xbfl_english:\n fixture_project:0 \"Fixture project\"\n fixture_other:0 \"Other project\"\n");
    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_fixture_mod textureFile=gfx/fixture_mod.png} spriteType={name=GFX_fixture_base textureFile=gfx/fixture_base.png}}");
    machine.write("gfx/fixture_mod.png", &png(RED));
    machine.write("gfx/fixture_base.png", &png(RED));
    machine.write_mod("fixture_art", "gfx/fixture_mod.png", &png(BLUE));
    let opened = || {
        let completed = [(1, 40, 96, 97), (2, 40, 98, 98), (3, 40, 99, 99), (4, 40, 100, 100), (5, 41, 101, 101)];
        let frames = (96..=101).map(|hour| {
            let present = completed.into_iter().filter(|&(_, _, _, read)| read <= hour).map(|(id, definition, date, _)| (id, definition, date));
            let mut frame = completion_frame(hour, present);
            frame.metrics.table_ns[TableId::EventFeedHistory.index()] = replay_frame::frame::READ_TIME_NOT_RECORDED;
            *frame.tables[TableId::EventFeedHistory.index()] = TableView::Complete(TableRows::EventFeedHistory(vec![EventFeedHistoryRow {
                generation: 1,
                read_hour: hour,
                coverage: EventFeedCoverage::Complete,
                counts: Some(EventFeedCounts { acquired: 0, settled: 0, failed: 0, rejected: 0 }),
                outstanding: 0,
                failure: None,
                script_refusals: 0,
                events: Vec::new(),
                script_tokens: Vec::new(),
                script_scopes: Vec::new(),
                windows: Default::default(),
            }]));
            *frame.tables[TableId::LexicalTokens.index()] = TableView::Complete(TableRows::LexicalTokens(vec![
                LexicalTokenRow { token: 40, literal: b"fixture_project".to_vec() },
                LexicalTokenRow { token: 41, literal: b"fixture_other".to_vec() },
            ]));
            frame
        }).collect();
        let (source, mut options) = machine.open_frames(frames);
        select(&source, &mut options, &["fixture_art"]);
        (Arc::new(source), LoadContext { content: Some(options) })
    };
    let cancelled = AtomicBool::new(false);
    let has_mod_note = |reply: &Value| reply["warnings"].as_array().unwrap().iter().any(|line| {
        line.as_str().is_some_and(|line| line.starts_with("Artwork from visual mods") && line.contains("fixture_art"))
    });
    let request = || json!({"branchId": "1", "limit": 64, "categories": ["milestones"]});
    let read = |source: &dashboard_base::Source, load: &LoadContext, hour: i32, mut ask: Value| {
        ask["hour"] = json!(hour);
        crate::registrations::FEED_EVENTFEEDTAIL.answer(source, ask, load, &cancelled).unwrap()
    };
    let (source, load) = opened();
    let (followed_source, followed_load) = opened();
    let mut panel_ask = request();
    panel_ask["panel"] = json!("event-feed-tail");
    let panel = super::event_feed_dated_panel(&followed_source, &followed_load, &panel_ask).unwrap();
    let followed = panel.exact().unwrap();

    for hour in [96, 97, 101] {
        let exact = read(&source, &load, hour, request());
        let step = (followed.advance)(hour, hour == 96, &cancelled, None);
        let held = step.value.as_ref().as_ref().unwrap();
        assert_eq!(held["warnings"], exact["warnings"], "exact and followed at {hour}");
        assert_eq!((followed.cold)(hour, &cancelled).unwrap()["warnings"], exact["warnings"], "followed cold read at {hour}");
        assert_eq!(has_mod_note(&exact), hour == 97, "artwork warnings at {hour}: {}", exact["warnings"]);
        match hour {
            96 => assert!(exact["rows"].as_array().unwrap().is_empty()),
            97 => {
                let line = &exact["rows"][0];
                assert_eq!(line["line"], "event");
                assert_eq!(line["hour"], 96);
                assert_eq!(line["observed"], 97);
                assert_eq!(first_pixel(line["row"]["item"]["icon"].as_str().unwrap()), BLUE);
            }
            _ => {
                let line = &exact["rows"][0];
                assert_eq!(line["line"], "fold");
                assert_eq!(line["count"], "5");
                assert_eq!(first_pixel(line["firstRow"]["item"]["icon"].as_str().unwrap()), RED);
            }
        }
    }
    let mut ascending = request();
    ascending["sort"] = json!({"column": "date", "direction": "ascending"});
    let fold = read(&source, &load, 101, ascending.clone());
    assert_eq!(fold["rows"][0]["line"], "fold");
    assert_eq!(first_pixel(fold["rows"][0]["firstRow"]["item"]["icon"].as_str().unwrap()), BLUE);
    assert!(has_mod_note(&fold), "the cached fold first row credits its own icon");
    let (fresh_source, fresh_load) = opened();
    assert_eq!(read(&fresh_source, &fresh_load, 101, ascending)["warnings"], fold["warnings"]);
}
