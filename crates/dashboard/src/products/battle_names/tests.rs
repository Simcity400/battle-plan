use super::*;

#[test]
fn title_preparation_retries_failures_and_refuses_a_retired_binding() {
    use dashboard_base::test_fixtures::{source, source_with_content, visual_mods};
    let game = visual_mods::Machine::new(&uuid::Uuid::new_v4().to_string());
    let (source, load) = source_with_content(vec![source(1, 100, Some(1))], &game.root);
    let source = Arc::new(source);
    let binding = load.content.as_ref().unwrap().presentation.binding();
    let failed = Arc::<Build>::default();
    failed.ready.publish(Err(io::Error::other("Synthetic title failure")));
    *source.state::<Ahead>().lock().unwrap() = Ahead(binding.generation, HashMap::from([(1, Arc::clone(&failed))]));
    for _ in 0..2 {
        assert_eq!(ahead(&source, 1, &load).err().unwrap().to_string(), "Synthetic title failure");
    }
    assert!(prepare_for_rows(&source, 1, &load).unwrap().is_none());
    assert_eq!(for_rows(&source, 1, &load, &false.into()).err().unwrap().to_string(), "Synthetic title failure");

    let retry = start(&source, 1, &load, true).unwrap().unwrap();
    assert!(!Arc::ptr_eq(&retry, &failed));
    let _ = retry.ready.wait();
    binding.retire();
    assert_eq!(start(&source, 1, &load, true).err().unwrap().kind(), io::ErrorKind::Interrupted);
}

#[test]
fn a_map_title_poll_uses_the_recorded_binding_after_its_binding_retires() {
    use dashboard_base::{source::Work, test_fixtures::visual_mods};
    let game = visual_mods::Machine::new("battle_titles_rebind");
    let (source, content) = game.open();
    let source = Arc::new(source);
    source.bind(Some((&content.options, content.allow_unverified)), Default::default());
    let captured = LoadContext { content: source.admit(Work::Recorded) };
    source.bind(None, Default::default());
    source.bind(Some((&content.options, content.allow_unverified)), Default::default());
    let binding = source.binding().unwrap();
    let build = Arc::<Build>::default();
    let titles = Arc::new(number(vec![named(Key::Report(77), 100, "Battle of Copper Hollow")]));
    assert!(build.names.set(titles).is_ok());
    build.ready.publish(Ok(()));
    *source.state::<Ahead>().lock().unwrap() = Ahead(binding.generation, HashMap::from([(1, build)]));
    let titles = ahead(&source, 1, &captured).unwrap().unwrap();
    assert_eq!(titles.get("result:77").unwrap().as_deref(), Ok("Battle of Copper Hollow"));
}

fn named(key: Key, hour: i32, title: &str) -> (Battle, Result<String, String>) {
    let date = match key { Key::Land(_, date, _) | Key::Naval(_, date) => date, Key::Report(_) => hour };
    (
        Battle { key, date, observed: hour, province: Some(1), port_strike: false, reports: Vec::new(), live_hours: None },
        Ok(title.into()),
    )
}

#[test]
fn repeats_and_singletons_use_the_full_ordinal_sequence() {
    let mut battles: Vec<_> = (1..=111)
        .map(|id| named(Key::Land(id, 12, None), 12, "Battle of Copper Hollow"))
        .collect();
    battles.push(named(Key::Naval(8, 14), 14, "Battle of Glass Shore"));
    let names = number(battles);
    for (id, ordinal) in [(1, "First"), (2, "Second"), (10, "Tenth"), (11, "11th"), (12, "12th"), (13, "13th"), (21, "21st"), (111, "111th")] {
        let key = Key::Land(id, 12, None).id();
        assert_eq!(names.get(&key).unwrap().as_deref(), Ok(format!("{ordinal} Battle of Copper Hollow").as_str()));
    }
    assert_eq!(names.get("combat:8:14").unwrap().as_deref(), Ok("Battle of Glass Shore"));
}

#[test]
fn land_and_naval_share_titles_in_order_and_a_linked_report_does_not_count_twice() {
    let mut live = named(Key::Naval(3, 10), 10, "Battle of Copper Hollow");
    live.0.reports.push(98);
    live.0.live_hours = Some((10, 13));
    let names = number(vec![
        named(Key::Report(8), 10, "Battle of Copper Hollow"),
        live,
        named(Key::Land(7, 10, Some(11)), 11, "Battle of Copper Hollow"),
        named(Key::Land(7, 10, None), 10, "Battle of Copper Hollow"),
        named(Key::Report(9), 15, "Port Strike in Copper Hollow"),
    ]);
    for (key, title) in [
        ("battle:7:10", "First Battle of Copper Hollow"),
        ("battle:7:10:11", "Second Battle of Copper Hollow"),
        ("combat:3:10", "Third Battle of Copper Hollow"),
        ("result:98", "Third Battle of Copper Hollow"),
        ("result:8", "Fourth Battle of Copper Hollow"),
        ("result:9", "Port Strike in Copper Hollow"),
    ] {
        assert_eq!(names.get(key).unwrap().as_deref(), Ok(title));
    }
    assert_eq!(names.naval(3, 12).unwrap().as_deref(), Ok("Third Battle of Copper Hollow"));
    assert!(names.naval(3, 14).is_none());
}

#[test]
fn a_retained_naval_report_keeps_its_ordinal_when_its_date_changes() {
    use dashboard_base::test_fixtures::{source, source_with_content, visual_mods};
    use replay_data::rows::{NavalCombatRow, NavalResultRow, NavalResultSourceRow, TableId, TableRows};
    use replay_frame::frame::TableView;

    let game = visual_mods::Machine::new(&uuid::Uuid::new_v4().to_string());
    game.write("map/strategicregions/1.txt", b"strategic_region={id=1 name=\"COPPER_COAST\" provinces={17 18}}\n");
    game.write("localisation/battles_l_english.yml", "\u{feff}l_english:\n COPPER_COAST:0 \"Copper Coast\"\n NAVAL_PORT_STRIKE_TITLE:0 \"Port Strike in $NAME$\"\n".as_bytes());
    for linked in [false, true] {
        let frames = [99, 100, 101, 102].map(|hour| {
            let mut frame = source(1, hour, None);
            let strike = |id, province, result_hour| NavalResultRow { id, province, result_hour, retire_hour: 200, flag_a: 1, ..Default::default() };
            let results = match hour {
                99 => vec![],
                100 => vec![strike(77, 17, 100)],
                101 => vec![strike(77, 18, 101)],
                _ => vec![strike(77, 18, 101), strike(78, 17, 102)],
            };
            let combats = (linked && hour == 99).then(|| NavalCombatRow { id: 7, province: 17, ..Default::default() });
            let sources = (linked && hour == 100).then(|| NavalResultSourceRow {
                id: 1, game_hour: 100, result: 77, combat: 7, combat_created_hour: Some(99), ..Default::default()
            });
            for (table, rows) in [
                (TableId::NavalResults, TableRows::NavalResults(results)),
                (TableId::NavalCombats, TableRows::NavalCombats(combats.into_iter().collect())),
                (TableId::NavalResultSources, TableRows::NavalResultSources(sources.into_iter().collect())),
            ] {
                *frame.tables[table.index()] = TableView::Complete(rows);
            }
            frame
        });
        let (source, load) = source_with_content(frames.into(), &game.root);
        let names = source.in_scope(|| read(&source, 1, &load, &false.into())).unwrap();
        let first = "First Port Strike in Copper Coast";
        assert_eq!(names.get("result:77").unwrap().as_deref(), Ok(first));
        assert_eq!(names.get("result:78").unwrap().as_deref(), Ok("Second Port Strike in Copper Coast"));
        if linked {
            assert_eq!(names.get("combat:7:99").unwrap().as_deref(), Ok(first));
            assert_eq!(names.naval(7, 99).unwrap().as_deref(), Ok(first));
        }
    }
}

#[test]
fn a_gap_keeps_the_sequence_and_each_lineage_counts_its_own_battles() {
    let first = || named(Key::Land(1, 10, None), 10, "Battle of Copper Hollow");
    let rename = named(Key::Land(2, 11, None), 11, "Battle of Glass Shore");
    let mut unknown = named(Key::Land(3, 29, None), 29, "Battle of Copper Hollow");
    unknown.1 = Err("Province names were truncated".into());
    let after_gap = named(Key::Land(4, 30, None), 30, "Battle of Copper Hollow");
    let main = number(vec![first(), rename, unknown, after_gap]);
    let fork = number(vec![first(), named(Key::Land(9, 32, None), 32, "Battle of Glass Shore")]);
    assert_eq!(main.get("battle:1:10").unwrap().as_deref(), Ok("First Battle of Copper Hollow"));
    assert_eq!(main.get("battle:4:30").unwrap().as_deref(), Ok("Second Battle of Copper Hollow"));
    assert_eq!(main.get("battle:2:11").unwrap().as_deref(), Ok("Battle of Glass Shore"));
    assert_eq!(main.get("battle:3:29").unwrap().as_ref().unwrap_err().as_ref(), "Province names were truncated");
    assert_eq!(fork.get("battle:1:10").unwrap().as_deref(), Ok("Battle of Copper Hollow"));
    assert!(fork.get("battle:4:30").is_none());
}

#[test]
fn named_battle_rows_match_the_brokers_at_the_recorded_hours() {
    use dashboard_base::{presentation::Presentation, source::Work, test_fixtures::{source, source_with_content, visual_mods}};
    use dashboard_land::registrations::LAND_BATTLES_TAIL;
    use dashboard_world::products::place_name_runs::{PlaceNameRuns, PlaceNameRunsParams};
    use replay_data::rows::{LandCombatRow, ProvinceRow, StateRow, TableId, TableRows};
    use replay_frame::frame::TableView;
    use serde_json::json;

    let game = visual_mods::Machine::new("named_land_rows");
    let frames = (100..=103).map(|hour| {
        let mut frame = source(1, hour, Some(1));
        for (table, rows) in [
            (TableId::LandCombats, TableRows::LandCombats(vec![LandCombatRow { id: hour as u64, province: 3, ..Default::default() }])),
            (TableId::Provinces, TableRows::Provinces(vec![ProvinceRow { id: 3, state: 7, ..Default::default() }])),
            (TableId::States, TableRows::States(vec![StateRow {
                id: 7, custom_name: if hour == 100 { b"Copper Hollow".to_vec() } else { b"Glass Shore".to_vec() }, ..Default::default()
            }])),
        ] {
            frame.metrics.table_ns[table.index()] = 1;
            *frame.tables[table.index()] = TableView::Complete(rows);
        }
        frame
    }).collect();
    let (source, load) = source_with_content(frames, &game.root);
    let source = Arc::new(source);
    let content = load.content.as_ref().unwrap();
    source.bind(Some((&content.options, content.allow_unverified)), Presentation::default());
    source.content(&content.options).unwrap();
    let load = dashboard_base::products::LoadContext { content: source.admit(Work::Content(Arc::new(AtomicBool::new(false)))) };
    let cancel = AtomicBool::new(false);
    let binding = load.content.as_ref().unwrap().presentation.binding();
    let build = Arc::<Build>::default();
    let mut refused = named(Key::Land(102, 102, None), 102, "");
    refused.1 = Err("Province names were truncated".into());
    let found = named(Key::Land(100, 100, None), 100, "Battle of Copper Hollow");
    let renamed = named(Key::Land(101, 101, None), 101, "Battle of Glass Shore");
    assert!(build.names.set(Arc::new(number(vec![found, renamed, refused]))).is_ok());
    build.ready.publish(Ok(()));
    *source.state::<Ahead>().lock().unwrap() = Ahead(binding.generation, HashMap::from([(1, build)]));
    let product = crate::products::find("land-battles-tail").unwrap();
    if let Some(ready) = dashboard_world::products::place_name_runs::prepare(&source, &load).unwrap() { ready.wait().unwrap(); }
    let panel = crate::panel::open(&source, &load, &json!({"panel": "land-battles-tail", "branchId": "1", "rowNames": true, "limit": 1})).unwrap();
    let advance = &panel.exact().unwrap().advance;
    let places = source.in_scope(|| PlaceNameRuns::load(&source, &PlaceNameRunsParams {
        branch_id: "1".into(), land: false, from: 100, to: 103, states: vec![7], provinces: vec![],
    }, &load, &cancel)).unwrap();
    for hour in 100..=103 {
        let plain = json!({"branchId": "1", "limit": 1, "hour": hour});
        assert_eq!((product.load)(&source, "test", plain.clone(), &load, &cancel).unwrap(),
            (LAND_BATTLES_TAIL.load)(&source, "test", plain.clone(), &load, &cancel).unwrap());
        let mut request = plain;
        request["rowNames"] = true.into();
        let named = source.in_scope(|| product.answer(&source, request, &load, &cancel)).unwrap();
        let followed = advance(hour, false, &cancel, None);
        assert_eq!(followed.value.as_ref().as_ref().unwrap(), &named);
        let row = &named["rows"][0];
        assert_eq!(row["hour"], hour);
        let id = format!("battle:{hour}:{hour}");
        let title = BattleNames::load(&source, &BattleNamesParams { branch_id: "1".into(), battles: vec![id.clone()] }, &load, &cancel).unwrap();
        let title = &title.titles[&id];
        assert_eq!(row["title"].as_str(), title.name.as_deref());
        assert_eq!(row["titleMissing"].as_str(), title.missing.as_deref());
        let state = places.states[0].runs.iter().find(|run| run.first_hour <= hour && hour <= run.last_hour).unwrap();
        assert_eq!(row["placeName"].as_str(), state.name.as_deref());
        assert_eq!(row["placeMissing"].as_str(), state.missing.as_deref());
    }
}

#[test]
fn named_ship_sinkings_match_the_battle_broker_in_followed_and_exact_pages() {
    use dashboard_base::test_fixtures::{source, source_with_content, visual_mods};
    use dashboard_naval::registrations::NAVAL_SUNK_TAIL;
    use replay_data::rows::{NavalCombatRow, SunkShipEntry, SunkShipsRow, TableId, TableRows};
    use replay_frame::frame::TableView;
    use serde_json::json;

    let game = visual_mods::Machine::new("named_sinking_rows");
    let frames = (100..=103).map(|hour| {
        let mut frame = source(1, hour, Some(1));
        let combat = if hour < 102 { 20 } else { 22 };
        let entries = match hour {
            101 | 102 => vec![SunkShipEntry { battle: combat, sink_hour: hour, ship_name: b"Meridian".to_vec(), ..Default::default() }],
            _ => Vec::new(),
        };
        for (table, rows) in [
            (TableId::NavalCombats, TableRows::NavalCombats(vec![NavalCombatRow { id: combat, province: 3, ..Default::default() }])),
            (TableId::SunkShips, TableRows::SunkShips(vec![SunkShipsRow { entries }])),
        ] {
            frame.metrics.table_ns[table.index()] = 1;
            *frame.tables[table.index()] = TableView::Complete(rows);
        }
        frame
    }).collect();
    let (source, load) = source_with_content(frames, &game.root);
    let source = Arc::new(source);
    let cancel = AtomicBool::new(false);
    let binding = load.content.as_ref().unwrap().presentation.binding();
    let build = Arc::<Build>::default();
    let mut refused = named(Key::Naval(22, 102), 102, "");
    refused.1 = Err("Battle province not recorded".into());
    assert!(build.names.set(Arc::new(number(vec![
        named(Key::Naval(20, 100), 100, "Battle of Copper Hollow"), refused,
    ]))).is_ok());
    build.ready.publish(Ok(()));
    *source.state::<Ahead>().lock().unwrap() = Ahead(binding.generation, HashMap::from([(1, build)]));

    let product = crate::products::find("ships-sunk-tail").unwrap();
    let panel = crate::panel::open(&source, &load, &json!({"panel":"ships-sunk-tail","branchId":"1","rowNames":true,"limit":1})).unwrap();
    let advance = &panel.exact().unwrap().advance;
    for hour in 100..=103 {
        let plain = json!({"branchId":"1","limit":1,"hour":hour});
        assert_eq!((product.load)(&source, "plain", plain.clone(), &load, &cancel).unwrap(),
            (NAVAL_SUNK_TAIL.load)(&source, "plain", plain.clone(), &load, &cancel).unwrap());
        let mut request = plain;
        request["rowNames"] = true.into();
        let exact = source.in_scope(|| product.answer(&source, request, &load, &cancel)).unwrap();
        let followed = advance(hour, false, &cancel, None);
        assert_eq!(followed.value.as_ref().as_ref().unwrap(), &exact);
        let rows = exact["rows"].as_array().unwrap();
        if hour == 100 {
            assert!(rows.is_empty());
            continue;
        }
        let row = &rows[0];
        let key = if hour == 101 { "combat:20:100" } else { "combat:22:102" };
        assert_eq!(row["battle"]["id"], key);
        let titles = BattleNames::load(&source, &BattleNamesParams { branch_id: "1".into(), battles: vec![key.into()] }, &load, &cancel).unwrap();
        assert_eq!(row["title"].as_str(), titles.titles[key].name.as_deref());
        assert_eq!(row["titleMissing"].as_str(), titles.titles[key].missing.as_deref());
        if hour == 103 { assert!(!followed.fresh, "a quiet step holds the named reply"); }
    }
}
