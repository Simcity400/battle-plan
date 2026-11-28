use super::*;

#[test]
fn day_history_follows_midnight_and_preserves_unrecorded_days() {
    use dashboard_base::{SourceKey, test_fixtures::{replay, source}};
    use serde_json::json;

    let frames = [10, 23, 48, 71, 72].map(|hour| source(1, hour, None));
    let replay = replay(frames.into()).into_timelines(&Default::default()).unwrap();
    let cancelled = false.into();
    let source = Arc::new(Source::from_replay(
        SourceKey { source_id: "day-history".into(), revision: "1".into() },
        "synthetic.battleplan".into(), Arc::new(replay), &cancelled,
    ).unwrap());
    let load = LoadContext { content: None };
    let unknown = json!({"branchId": "1", "panel": "unregistered-widget"});
    assert_eq!(open(&source, &load, &unknown).err().unwrap(), "No panel \"unregistered-widget\"");
    let params = json!({"branchId":"1", "panel":"convoy-health", "view":"regions", "country":"GER", "regions":[17], "from":10, "to":71});
    let panel = open(&source, &load, &params).unwrap();
    let exact = panel.exact().unwrap();
    for (hour, last) in [(10, None), (23, None), (24, Some(23)), (48, Some(47)), (71, Some(47)), (72, Some(71)), (10, None)] {
        let read = (exact.advance)(hour, hour == 10, &cancelled, None);
        let expected = if let Some(last) = last {
            let request = json!({"branchId":"1", "view":"regions", "country":"GER", "regions":[17], "from":10, "to":last});
            crate::answer::answer(&source, "convoy-health", &request, &load, &cancelled).unwrap()
        } else { Value::Null };
        assert_eq!(read.value.as_ref().as_ref().unwrap(), &expected, "history at {hour}");
        if hour == 72 {
            assert!(expected["gaps"].as_array().unwrap().iter().any(|gap| gap["firstHour"] == 24 && gap["lastHour"] == 47));
        }
    }
}

#[test]
fn dated_products_seed_advance_confirm_and_reseed_across_a_recording_gap() {
    use dashboard_base::test_fixtures::{source, source_with_content};
    use dashboard_follow::panel::{dated, product::Delivery};
    use replay_data::rows::*;
    use replay_frame::frame::TableView;
    use serde_json::json;

    let root = std::env::temp_dir().join(format!("dated-products-{}", uuid::Uuid::new_v4()));
    let defines = root.join("common/defines");
    std::fs::create_dir_all(&defines).unwrap();
    std::fs::write(defines.join("test.lua"), "NDefines={NCharacter={OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU={}},NGame={MAX_SCRIPTED_LOC_RECURSION=23},NSupply={MAX_RAILWAY_LEVEL=7}}").unwrap();
    let frames = [100, 101, 102, 103, 110, 111].map(|hour| {
        let mut frame = source(1, hour, None);
        for table in TableId::ALL {
            if !table.is_derived() && table.key_bits() != 0 && matches!(frame.tables[table.index()].held(), TableView::Unavailable { .. }) {
                *frame.tables[table.index()] = TableView::Complete(TableRows::empty(table));
            }
        }
        let changed = i32::from(hour >= 102);
        let set = |frame: &mut replay_frame::frame::DecodedFrame, table: TableId, rows| {
            *frame.tables[table.index()] = TableView::Complete(rows);
        };
        set(&mut frame, TableId::IntelligenceAgencies, TableRows::IntelligenceAgencies(vec![IntelligenceAgencyRow { country: 9001, created: 1, active_count: changed, ..Default::default() }]));
        set(&mut frame, TableId::CountryGameScoreInputs, TableRows::CountryGameScoreInputs(vec![CountryGameScoreInputRow {
            country: 9001, army_count: Some(changed), navy_count: Some(0), land_count: Some(0),
            air_state: CountryGameScoreInputRow::AIR_ABSENT, ..Default::default()
        }]));
        set(&mut frame, TableId::GameScoreMultipliers, TableRows::GameScoreMultipliers(vec![GameScoreMultiplierRow {
            army_raw: Some(1000), navy_raw: Some(0), land_raw: Some(0), air_raw: Some(0),
            industry_raw: Some(0), total_raw: Some(100_000), ..Default::default()
        }]));
        set(&mut frame, TableId::CountryOccupationReserveManpower, TableRows::CountryOccupationReserveManpower(vec![CountryOccupationReserveManpowerRow { country: 9001, source_country: 11, amount: changed, ..Default::default() }]));
        set(&mut frame, TableId::DoctrineFolders, TableRows::DoctrineFolders(vec![DoctrineFolderRow { slot: 7, folder: 1, ..Default::default() }]));
        set(&mut frame, TableId::DoctrineTracks, TableRows::DoctrineTracks(vec![DoctrineTrackRow { slot: 7, mastery: changed.into(), ..Default::default() }]));
        let mut lines = vec![ProductionLineRow { country: 9001, handle: 1, active_factories: changed, ..Default::default() }];
        if hour >= 102 { lines.push(ProductionLineRow { country: 9001, handle: 2, ..Default::default() }); }
        set(&mut frame, TableId::ProductionLines, TableRows::ProductionLines(lines));
        let states = (1..=12).map(|id| StateRow {
            id, owner: if id == 1 && hour >= 102 { 9001 } else { 0 }, ..Default::default()
        }).collect();
        set(&mut frame, TableId::States, TableRows::States(states));
        set(&mut frame, TableId::ResearchInputs, TableRows::ResearchInputs(vec![ResearchInputRow { country: 9001, banked_days_raw: changed.into(), ..Default::default() }]));
        let mut project = ProjectRow { owner: 9001, local_id: 1, started: 1, ..Default::default() };
        project.state.prototype_progress = changed;
        set(&mut frame, TableId::Projects, TableRows::Projects(vec![project]));
        let mut samples: Vec<_> = [TableId::ResearchInputs, TableId::Projects, TableId::ResearchModifiers, TableId::ResearchAppliedBonuses].map(|table| SliceReadRow { table: table.index() as u32, owner: 9001, sample_hour: 100 + changed * 2 }).into();
        samples.sort_by_key(|row| row.table);
        set(&mut frame, TableId::SliceReads, TableRows::SliceReads(samples));
        frame
    });
    let (source, load) = source_with_content(frames.into(), &root);
    let source = Arc::new(source);
    for name in ["special-project-details", "special-projects", "doctrine-mastery", "research-slots", "production-lines-at", "equipment-designs", "industrial-organisations", "market-lendlease", "nuclear-arsenal", "game-score", "occupation-reserve-manpower", "agency-comparison", "territory-share", "lend-lease-balance", "occupied-states"] {
        let mut params = json!({"branchId": "1", "panel": name});
        match name {
            "nuclear-arsenal" | "game-score" | "occupation-reserve-manpower" | "lend-lease-balance" | "occupied-states" => {}
            "agency-comparison" => params["includeUncreated"] = true.into(),
            "market-lendlease" => params["countries"] = json!(["GER"]),
            "territory-share" => params["measure"] = "ownedStates".into(),
            _ => params["country"] = "GER".into(),
        }
        let panel = open(&source, &load, &params).unwrap_or_else(|error| panic!("{name}: {error}"));
        let exact = panel.exact().unwrap();
        let mut delivery = Delivery::default();
        let mut previous = None;
        for (hour, seek) in [(100, true), (101, false), (102, false), (102, false), (104, false), (106, false), (110, false), (100, true)] {
            let mut read = (exact.advance)(hour, seek, &false.into(), None);
            if read.pending {
                let wake = panel.pending_preparation().and_then(|prepare| prepare(hour)).expect("pending seed has preparation");
                let (ready, waited) = std::sync::mpsc::sync_channel(1);
                wake(Arc::new(move || { let _ = ready.send(()); }));
                waited.recv().unwrap();
                read = (exact.advance)(hour, seek, &false.into(), None);
            }
            let cold = (exact.cold)(hour, &false.into());
            let shown = |data: &dashboard_follow::panel::PanelData, values: bool| data.clone().map(|mut reply| {
                for group in reply.pointer_mut("/live/groups").and_then(Value::as_array_mut).into_iter().flatten() {
                    if !values && let Some(group) = group.as_object_mut() { group.remove("values"); }
                }
                reply
            });
            assert!(read.fresh || !seek, "{name}: a seed reads its live values at its hour");
            let packet = delivery.packet((0, 1, 1), 1, hour, seek, &read.value);
            let mut expected = shown(&cold, read.fresh);
            if name == "special-projects" && let Ok(data) = &*read.value {
                let through = i32::try_from(data["shown"]["through"].as_i64().unwrap()).unwrap();
                assert!(through >= hour);
                expected = (exact.cold)(through, &false.into());
            }
            if name == "special-project-details" && !read.fresh && let (Ok(data), Ok(reply)) = (&*read.value, &mut expected) {
                reply["hour"] = data["hour"].clone();
            }
            if name == "territory-share" && hour == 101 {
                assert!(!packet.changed, "unchanged ownership confirms the hour 100 reply");
                let reply = expected.as_mut().unwrap();
                assert_eq!(reply["hour"], hour);
                reply["hour"] = 100.into();
            }
            assert_eq!(shown(&read.value, read.fresh), expected, "{name} at {hour}");
            if (104..110).contains(&hour) {
                assert!(cold.as_ref().unwrap_err().contains("hour is not recorded"), "{name} gap at {hour}");
            } else {
                assert!(cold.is_ok(), "{name} at {hour}: {cold:?}");
            }
            assert_eq!(packet.changed, seek || previous.as_ref() != Some(&*read.value), "{name} at {hour}");
            let body = match &*read.value { Ok(v) => rmp_serde::to_vec(v).unwrap(), Err(e) => e.as_bytes().to_vec() };
            assert_eq!(delivery.body(), Some(body.as_slice()), "{name} wire record at {hour}");
            assert_eq!(dated::records(&packet.bytes).unwrap()[0].hour, hour);
            previous = Some((*read.value).clone());
        }
    }
    let ask = json!({"panel": "series-rank", "branchId": "1", "family": {"kind": "country-game-scores", "measure": "army"}, "countries": ["GER"]});
    let panel = open(&source, &load, &ask).unwrap();
    let rank = panel.exact().unwrap();
    let seed = (rank.advance)(102, true, &false.into(), None);
    let (cold, reads) = replay_recording::cells::count_reads(|| (rank.cold)(102, &false.into()));
    assert_eq!(seed.value.as_ref(), &cold);
    assert!(cold.unwrap()["rows"][0]["readings"][0]["run"].is_object());
    assert_eq!(reads[&TableId::CountryGameScores].opened, 1, "the exact oracle opens its own reader even after the followed read");
    drop(source);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn military_ranks_reuse_reading_runs_and_keep_the_exact_reply() {
    use dashboard_base::{SourceKey, test_fixtures::{CAMPAIGN, records, source}};
    use replay::campaign::Replay;
    use replay_data::rows::*;
    use replay_frame::frame::TableView;
    use serde_json::json;

    let first = 100;
    let second = first + 750;
    let frames = (first..=first + 724).chain(second..=second + 3).map(|hour| {
        let mut frame = source(1, hour, None);
        if let TableView::Complete(TableRows::Countries(rows)) = &mut *frame.tables[TableId::Countries.index()] {
            rows.push(CountryRow { id: 9002, canonical_slot: 8, tag: b"ITA".to_vec(), ..Default::default() });
        }
        if let TableView::Complete(TableRows::CountryLinks(rows)) = &mut *frame.tables[TableId::CountryLinks.index()] {
            rows.push(CountryLinkRow { id: 9002, canonical_slot: 8 });
        }
        let value = |slot| match (hour - first, slot) {
            (2, _) => Recorded::Value(30),
            (6..=7, 8) => Recorded::Value(i64::from(hour) + 100),
            (_, 7) => Recorded::Value(i64::from(hour) + 20),
            _ => Recorded::Value(i64::from(hour)),
        };
        let army = [7, 8].map(|slot| ArmyTotalRow { slot, owner_manpower: value(slot), ..Default::default() });
        let casualties = [7, 8].map(|slot| CasualtyTotalRow {
            slot, id: 8994 + slot, dealt: Recorded::Value(12), taken: value(slot), unknown: Recorded::Value(1), partial: u8::from(slot == 8),
        });
        for rows in [TableRows::ArmyTotals(army.into()), TableRows::CasualtyTotals(casualties.into())] {
            let table = rows.table();
            *frame.tables[table.index()] = if hour == second + 3 {
                TableView::Unavailable { reason: "fixture military readings unavailable".into() }
            } else {
                TableView::Complete(rows)
            };
        }
        frame
    }).collect();
    let current = replay_recording::schema::columns_at(replay_data::FORMAT_VERSION).unwrap();
    let mut inputs = current.clone();
    for table in [TableId::ArmyTotals, TableId::CasualtyTotals] { inputs[table.index()] = 0; }
    let writers = recorder_derive::writers_within(&inputs, &current);
    let replay = Replay::from_records(writers, CAMPAIGN.into(), &[records(1, frames)])
        .unwrap().into_timelines(&Default::default()).unwrap();
    let edge = replay.index.iter().find(|block| block.run == 0 && block.page == 1).unwrap().first_hour;
    let cancelled = false.into();
    let source = Arc::new(Source::from_replay(
        SourceKey { source_id: "military-ranks".into(), revision: "1".into() },
        "synthetic.battleplan".into(), Arc::new(replay), &cancelled,
    ).unwrap());
    let load = LoadContext { content: None };
    for (table, kind, measure, details) in [
        (TableId::CasualtyTotals, "casualty-totals", "dealtAndTaken", &["dealt", "taken", "unknown", "partial"][..]),
        (TableId::CasualtyTotals, "casualty-totals", "taken", &["unknown", "partial"][..]),
        (TableId::ArmyTotals, "army-totals", "ownerManpower", &[][..]),
    ] {
        for top in [2, 1] {
            let ask = json!({"panel": "series-rank", "branchId": "1", "family": {"kind": kind, "measure": measure}, "top": top, "measures": details});
            let panel = open(&source, &load, &ask).unwrap();
            let rank = panel.exact().unwrap();
            let steps = if top == 2 {
                vec![(first + 1, false), (first + 2, true), (edge - 1, true), (edge, false), (second - 1, false),
                    (second, false), (second + 1, true), (second + 3, false), (second, false), (second + 1, true)]
            } else {
                vec![(first + 4, false), (first + 5, true), (first + 6, false), (first + 7, true)]
            };
            let mut previous = None;
            for (hour, reused) in steps {
                let count = |runs: replay_recording::cells::ReadCounts| runs.get(&table).map_or(0, |stats| stats.opened) as usize;
                let seek = previous.is_none_or(|before| hour < before);
                let (followed, warm) = replay_recording::cells::count_reads(|| (rank.advance)(hour, seek, &cancelled, None));
                let (exact, cold) = replay_recording::cells::count_reads(|| (rank.cold)(hour, &cancelled));
                previous = Some(hour);
                let exact = exact.unwrap();
                assert_eq!(rmp_serde::to_vec(followed.value.as_ref().as_ref().unwrap()).unwrap(), rmp_serde::to_vec(&exact).unwrap(), "{kind}/{measure} at {hour}");
                assert_eq!(count(cold), count(warm) + if reused { details.len() + 1 } else { 0 }, "reader opens for {kind}/{measure} at {hour}");
                if hour == first + 2 {
                    assert_eq!(exact["rows"][0]["country"], "GER");
                    assert_eq!(exact["rows"][1]["country"], "ITA");
                    if measure == "dealtAndTaken" {
                        let readings = &exact["rows"][0]["readings"];
                        assert_eq!(readings[0]["run"]["valueRaw"], "42");
                        assert_eq!(readings[1]["run"]["valueRaw"], "12");
                        assert_eq!(readings[2]["run"]["valueRaw"], "30");
                    }
                }
                if hour == second - 1 {
                    assert!(exact["refused"].as_str().is_some(), "{kind}/{measure} at {hour}");
                }
                if hour == second + 3 {
                    assert!(exact["refused"].as_str().unwrap().contains("fixture military readings unavailable"));
                }
                if top == 1 {
                    assert_eq!(exact["rows"][0]["country"], if hour < first + 6 { "GER" } else { "ITA" });
                }
            }
        }
    }
}

#[test]
fn each_ui_panel_key_has_one_registered_reader() {
    let folder = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let contract = std::fs::read_to_string(folder.join("../../ui/src/generated/dashboard.ts")).unwrap();
    let keys = crate::contract_tests::product_keys(&contract, "PANEL_KEYS");
    assert_eq!(keys, READERS.keys().copied().collect::<Vec<_>>());
}
