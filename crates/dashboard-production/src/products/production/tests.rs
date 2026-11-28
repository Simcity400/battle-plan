use dashboard_world::products::equipment_names as names;
use super::{
    catalogue::{EquipmentDesigns, IndustrialOrganisations, ProductionLicences},
    breakdown::{ProductionBreakdownParams, ProductionLineBreakdown},
    live::ProductionLinesAt,
    *,
};
use dashboard_base::{Source, products::{LoadContext, at_hour::entry}, test_fixtures::{CAMPAIGN, records, source}};
use replay::campaign::Replay;
use replay_data::rows::{
    CountryLinkRow, CountryProductionModifiersRow, CountryRow, ModifierBreakdownNode, ProductionLineModifiersRow, ProductionModifierTree, IndustrialOrganisationDetailsRow, IndustrialOrganisationRow, ProductionLicenseRecordRow, ProductionLineOutputRow, ProductionLineRow, Recorded, TableRows,
    VariantDefinitionRow,
};
use replay_frame::frame::{DecodedFrame, TableView};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

const FIRST: i32 = 1_000;
const LAST: i32 = 1_300;
const UNREAD: (i32, i32) = (1_240, 1_250);

fn line(handle: u64, country: u32, variant: u64, active: i32, priority: i32) -> ProductionLineRow {
    ProductionLineRow { handle, country, kind: 0, variant, mio: 0, active_factories: active, requested_factories: 3, cost_raw: 700_000, speed_raw: 200_000, priority, amount: -1, max_factories: 10, efficiency_raw: 1_638_400, ..Default::default() }
}

fn output(line: u64, country: u32, variant: u64, units: i64, ended: bool) -> ProductionLineOutputRow {
    ProductionLineOutputRow { line, ordinal: 0, country, variant, mio: 0, units, since_hour: FIRST, observed_since_creation: line != 1, ended, exact: true, spent_raw: Recorded::Value(None), purpose: Recorded::Value(1), ..Default::default() }
}

fn frame(hour: i32) -> DecodedFrame {
    let mut frame = source(1, hour, Some(i64::from(hour)));
    if let TableView::Complete(TableRows::Countries(rows)) = &mut *frame.tables[TableId::Countries.index()] {
        rows.push(CountryRow { id: 9002, canonical_slot: 8, tag: b"ITA".to_vec(), ..CountryRow::default() });
    }
    if let TableView::Complete(TableRows::CountryLinks(rows)) = &mut *frame.tables[TableId::CountryLinks.index()] {
        rows.push(CountryLinkRow { id: 9002, canonical_slot: 8 });
    }
    let mut lines = vec![line(1, 11, if hour < 1_100 { 100 } else { 101 }, if hour < 1_150 { 2 } else { 3 }, 1), line(3, 9002, 300, 1, 0)];
    let mut outputs = vec![output(1, 11, if hour < 1_100 { 100 } else { 101 }, i64::from((hour - FIRST) / 24), false), output(3, 9002, 300, 0, false)];
    if (1_050..1_200).contains(&hour) {
        lines.push(line(2, 9001, 200, 1, 0));
        outputs.push(output(2, 9001, 200, i64::from((hour - 1_050) / 24), false));
    }
    if (1_200..1_224).contains(&hour) {
        outputs.push(output(2, 9001, 200, 7, true));
    }
    lines.sort_by_key(|row| row.handle);
    outputs.sort_by_key(|row| (row.line, row.ordinal));
    *frame.tables[TableId::ProductionLines.index()] = if (UNREAD.0..=UNREAD.1).contains(&hour) {
        TableView::Unavailable { reason: "lines unread in the fixture".into() }
    } else {
        TableView::Complete(TableRows::ProductionLines(lines))
    };
    *frame.tables[TableId::ProductionLineOutput.index()] = TableView::Complete(TableRows::ProductionLineOutput(outputs));
    let mut licences = vec![ProductionLicenseRecordRow { owner: 11, ordinal: 0, giver: 9002, lended_cic: 2, required_cic: 1, start_date: FIRST, equipment: vec![300], ..Default::default() }];
    if hour >= 1_200 {
        licences.push(ProductionLicenseRecordRow { owner: 9002, ordinal: 0, giver: 77, lended_cic: 0, required_cic: 0, start_date: 1_200, equipment: vec![100], ..Default::default() });
    }
    *frame.tables[TableId::ProductionLicenseRecords.index()] = TableView::Complete(TableRows::ProductionLicenseRecords(licences));
    *frame.tables[TableId::IndustrialOrganisations.index()] = TableView::Complete(TableRows::IndustrialOrganisations(vec![
        IndustrialOrganisationRow { organisation: 40, country: 11, owner: 9001, ..Default::default() },
        IndustrialOrganisationRow { organisation: 41, country: 9002, owner: 9002, ..Default::default() },
    ]));
    *frame.tables[TableId::IndustrialOrganisationDetails.index()] = if hour < 1_100 {
        TableView::Unavailable { reason: "details unread in the fixture".into() }
    } else {
        let mut details = IndustrialOrganisationDetailsRow { organisation: 40, country: 11, unlocked: vec![1, 2], ..Default::default() };
        details.state.size = 3;
        details.state.funds_raw = i64::from(hour) * 100;
        details.state.points = hour - FIRST;
        details.state.policy = 0x4c17;
        TableView::Complete(TableRows::IndustrialOrganisationDetails(vec![details]))
    };
    let mut designs = vec![VariantDefinitionRow { id: 500, creator_link: 9001, obsolete: u8::from(hour >= 1_200), ..Default::default() }, VariantDefinitionRow { id: 501, creator_link: 9002, ..Default::default() }];
    if hour >= 1_100 {
        designs.push(VariantDefinitionRow { id: 502, creator_link: 11, ..Default::default() });
    }
    *frame.tables[TableId::VariantDefinitions.index()] = TableView::Complete(TableRows::VariantDefinitions(designs));
    frame
}

fn recording() -> Source {
    from_frames((FIRST..=LAST).map(frame).collect())
}

fn from_frames(frames: Vec<DecodedFrame>) -> Source {
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    Source::from_replay(dashboard_base::SourceKey { source_id: "production".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap()
}

#[test]
fn catalogue_playback_skips_changes_to_power_and_live_organisation_counters() {
    use dashboard_base::products::at_hour::exact_panel;
    let source = recording();
    let params = json!({ "branchId": "1", "country": "GER" });
    let mut rows = Vec::new();
    for panel in [exact_panel::<EquipmentDesigns>(&source, &load(), &params), exact_panel::<IndustrialOrganisations>(&source, &load(), &params)] {
        let panel = panel.unwrap();
        let reads = dashboard_base::test_fixtures::played(source.replay_ref(), panel.exact().unwrap(), 1_150, 1_160);
        assert_eq!(reads.len(), 11);
        assert!(reads.iter().all(|read| !read.pending && read.value.is_ok()));
        rows.push(reads.iter().skip(1).map(|read| read.rows).sum::<u64>());
    }
    assert_eq!(rows, [0, 0]);
}

#[test]
fn production_licences_played_steps_match_whole_frame_across_edits_groups_damage_seeks_and_branches() {
    use crate::registrations::PRODUCTION_PRODUCTIONLICENCES;
    use dashboard_base::products::at_hour::frame_panel;
    use dashboard_base::test_fixtures::{materialized_frame, played};
    use replay_data::rows::{FactionMemberRow, FactionRow};

    let build_frame = |branch: u64, h: i32, variant_branch: bool| {
        let mut f = frame(h);
        f.branch = branch;
        if let TableView::Complete(TableRows::Countries(rows)) = &mut *f.tables[TableId::Countries.index()] {
            rows.push(CountryRow { id: 9003, canonical_slot: 9, tag: b"FRA".to_vec(), ..CountryRow::default() });
            rows.push(CountryRow { id: 9004, canonical_slot: 10, tag: b"SOV".to_vec(), ..CountryRow::default() });
        }
        if let TableView::Complete(TableRows::CountryLinks(rows)) = &mut *f.tables[TableId::CountryLinks.index()] {
            let slot_9003 = if !variant_branch && h >= FIRST + 8 { 7 } else { 9 };
            rows.push(CountryLinkRow { id: 9003, canonical_slot: slot_9003 });
            rows.push(CountryLinkRow { id: 9004, canonical_slot: 10 });
        }
        *f.tables[TableId::Factions.index()] = TableView::Complete(TableRows::Factions(vec![
            FactionRow { id: 10, name: b"SynBloc".to_vec(), ..FactionRow::default() },
        ]));
        let mut members = vec![FactionMemberRow { country: 9001, faction: 10, ordinal: 0 }];
        if variant_branch || h >= FIRST + 10 {
            members.push(FactionMemberRow { country: 9002, faction: 10, ordinal: 1 });
        }
        *f.tables[TableId::FactionMembers.index()] = TableView::Complete(TableRows::FactionMembers(members));

        let mut licences = if variant_branch {
            vec![ProductionLicenseRecordRow { owner: 9003, ordinal: 0, giver: 11, lended_cic: h, ..Default::default() }]
        } else {
            let ger_giver = if h >= FIRST + 3 { 9003 } else { 9002 };
            let mut list = vec![
                ProductionLicenseRecordRow { owner: 11, ordinal: 0, giver: ger_giver, lended_cic: h, ..Default::default() },
                ProductionLicenseRecordRow { owner: 9002, ordinal: 0, giver: if h % 2 == 0 { 9002 } else { 9004 }, lended_cic: h * 3, ..Default::default() },
            ];
            if h % 3 == 0 {
                list.push(ProductionLicenseRecordRow { owner: 9004, ordinal: 1, giver: 9002, lended_cic: h, ..Default::default() });
            }
            if h == FIRST + 5 || (FIRST + 17..=FIRST + 18).contains(&h) {
                list.push(ProductionLicenseRecordRow { owner: 9002, ordinal: 2, giver: 777, ..Default::default() });
            }
            list
        };
        licences.sort_by_key(|row| (row.owner, row.ordinal));
        *f.tables[TableId::ProductionLicenseRecords.index()] = TableView::Complete(TableRows::ProductionLicenseRecords(licences.clone()));

        if !variant_branch {
            if (FIRST + 12..=FIRST + 14).contains(&h) {
                *f.tables[TableId::ProductionLicenseRecords.index()] = TableView::Unavailable { reason: "licences damaged".into() };
            } else if (FIRST + 15..=FIRST + 16).contains(&h) {
                *f.tables[TableId::ProductionLicenseRecords.index()] = TableView::Truncated {
                    rows: TableRows::ProductionLicenseRecords(licences[..1].to_vec()),
                    missing: 2,
                };
            }
            if (FIRST + 17..=FIRST + 18).contains(&h) {
                let TableView::Complete(TableRows::CountryLinks(links)) = f.tables[TableId::CountryLinks.index()].held().clone() else {
                    panic!("expected country links");
                };
                *f.tables[TableId::CountryLinks.index()] = TableView::Truncated {
                    rows: TableRows::CountryLinks(links),
                    missing: 3,
                };
            }
            if (FIRST + 19..=FIRST + 20).contains(&h) {
                *f.tables[TableId::Countries.index()] = TableView::Unavailable { reason: "countries damaged".into() };
            }
        }
        f
    };

    let branch1: Vec<DecodedFrame> = (FIRST..=FIRST + 23).map(|h| build_frame(1, h, false)).collect();
    let replay = Replay::from_records(
        recorder_derive::writers(),
        CAMPAIGN.into(),
        &[records(1, branch1.clone())],
    )
    .unwrap()
    .into_timelines(&Default::default())
    .unwrap();
    let source = Arc::new(
        Source::from_replay(
            dashboard_base::SourceKey { source_id: "licences-played".into(), revision: "1".into() },
            "synthetic.battleplan".into(),
            Arc::new(replay),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );

    let requests = [
        json!({"branchId": "1", "subject": "GER"}),
        json!({"branchId": "1", "countries": ["GER", "FRA"], "between": true}),
        json!({"branchId": "1", "countries": ["GER", "FRA"], "between": false}),
        json!({"branchId": "1", "group": {"type": "faction", "id": "10", "membership": "hour"}, "between": false}),
    ];
    for params in &requests {
        let mut whole = frame_panel::<ProductionLicences>(&source, &load(), params).unwrap();
        let panel = (PRODUCTION_PRODUCTIONLICENCES.panel.unwrap().open)(&source, &load(), params).unwrap();
        let reader = panel.exact().unwrap();
        let steps = played(source.replay_ref(), reader, FIRST, FIRST + 23);
        let mut previous = None;
        for step in &steps {
            let expected = materialized_frame(&mut *whole, source.replay_ref(), 1, step.hour);
            assert_eq!(*step.value, expected, "{params} at {}", step.hour);
            if let Some(prior) = &previous
                && step.hour != FIRST + 10
                && step.hour != FIRST + 8
                && step.hour != FIRST + 17
                && step.hour != FIRST + 19
                && step.hour != FIRST + 21
            {
                assert_eq!(step.product_calls, u64::from(*prior != expected), "{params} calls at {}", step.hour);
            }
            previous = Some(expected);
        }
        assert_eq!(steps[1].product_calls, 0, "{params}: quiet foreign/unused-column hour must not call product");
        assert_eq!(steps[2].product_calls, 0, "{params}: quiet foreign/unused-column hour must not call product");
        if params["between"] == json!(true) {
            assert_eq!(steps[5].product_calls, 0, "between=true ignores unlinked licence with proven non-chosen owner");
        }
        for h in [FIRST + 2, FIRST + 13, FIRST + 15, FIRST + 17, FIRST + 19, FIRST + 22] {
            let expected = materialized_frame(&mut *whole, source.replay_ref(), 1, h);
            assert_eq!(*(reader.advance)(h, true, &false.into(), None).value, expected, "{params} seek {h}");
        }
    }

    let branch2: Vec<DecodedFrame> = (FIRST..=FIRST + 4).map(|h| build_frame(2, h, true)).collect();
    let multi = Replay::from_records(
        recorder_derive::writers(),
        CAMPAIGN.into(),
        &[records(1, branch1), records(2, branch2)],
    )
    .unwrap()
    .into_timelines(&Default::default())
    .unwrap();
    let multi_source = Arc::new(
        Source::from_replay(
            dashboard_base::SourceKey { source_id: "licences-branches".into(), revision: "1".into() },
            "synthetic.battleplan".into(),
            Arc::new(multi),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let branches = multi_source.metadata().unwrap().branches;
    assert_eq!(branches.len(), 2);
    for branch in &branches {
        let id: u64 = branch.id.parse().unwrap();
        let req = json!({"branchId": branch.id, "subject": "GER"});
        let mut whole_b = frame_panel::<ProductionLicences>(&multi_source, &load(), &req).unwrap();
        let panel_b = (PRODUCTION_PRODUCTIONLICENCES.panel.unwrap().open)(&multi_source, &load(), &req).unwrap();
        let reader_b = panel_b.exact().unwrap();
        for h in FIRST..=FIRST + 4 {
            let expected = materialized_frame(&mut *whole_b, multi_source.replay_ref(), id, h);
            assert_eq!(*(reader_b.advance)(h, h == FIRST, &false.into(), None).value, expected, "branch {} at {h}", branch.id);
        }
    }
}

#[test]
fn stockpile_artwork_played_steps_match_whole_frame_across_edits_damage_seeks_and_branches() {
    use crate::products::stockpile::StockpileArtwork;
    use crate::registrations::STOCKPILE_STOCKPILEARTWORK;
    use dashboard_base::products::at_hour::{AtHour, frame_panel};
    use dashboard_base::test_fixtures::{content_identity, materialized_frame, played, source_with_content, visual_mods::Machine, with_facts};
    use replay_data::rows::{
        AvailableEquipmentVariantRow, CountryAppearanceRow, FactionRow, LexicalTokenRow, PoliticsRow,
        ReservedCountryNamingRow, SimulationFeatureRow, StateRow, VariantUpgradeRow,
    };

    let machine = Machine::new("stockpile-artwork-played");
    machine.write("map/default.map", b"definitions=definition.csv provinces=provinces.bmp terrain=terrain.bmp rivers=rivers.bmp heightmap=heightmap.bmp tree_definition=trees.bmp continent=continent.txt");
    machine.write("map/definition.csv", b"0;0;0;0;land;false;unknown;0\n1;1;0;0;land;false;plains;1\n2;2;0;0;land;false;plains;2\n");
    machine.write("map/continent.txt", b"continents={ syn_west syn_east }");
    machine.write("common/defines/test.lua", b"NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NBuildings={MAX_BUILDING_LEVELS=10}}");
    machine.write("common/units/equipment/upgrades/syn.txt", b"upgrades={}");
    machine.write("common/units/equipment/modules/syn.txt", b"equipment_modules={}");
    machine.write("common/units/equipment/syn.txt", b"equipments={ syn_gun={ is_archetype=yes type=infantry } syn_gun_a={ archetype=syn_gun } }");

    let art = json!({"branchId": "1", "country": "GER", "variants": ["500", "503"]});
    let build_frame = |branch: u64, h: i32, variant_branch: bool| {
        let mut f = frame(h);
        f.branch = branch;
        for table in StockpileArtwork::tables(&serde_json::from_value(art.clone()).unwrap()) {
            if matches!(f.tables[table.index()].held(), TableView::Unavailable { .. }) {
                *f.tables[table.index()] = TableView::Complete(TableRows::empty(table));
            }
        }
        *f.tables[TableId::CountryAppearance.index()] = TableView::Complete(TableRows::CountryAppearance(vec![
            CountryAppearanceRow { country: 9001, graphical_culture: b"syn_gfx".to_vec(), graphical_culture_2d: b"syn_2d".to_vec(), ..Default::default() },
            CountryAppearanceRow { country: 9002, graphical_culture: b"syn_gfx".to_vec(), graphical_culture_2d: b"syn_2d".to_vec(), ..Default::default() },
        ]));
        *f.tables[TableId::ReservedCountryNaming.index()] = TableView::Complete(TableRows::ReservedCountryNaming(vec![
            ReservedCountryNamingRow { dynamic_tag_boundary: Recorded::Value(Some(50)), ..Default::default() },
        ]));
        *f.tables[TableId::Factions.index()] = TableView::Complete(TableRows::Factions(vec![
            FactionRow { id: 10, name: format!("Renamed{h}").into_bytes(), ..Default::default() },
        ]));
        let ger_capital = if !variant_branch && h >= FIRST + 10 { 2 } else { 1 };
        *f.tables[TableId::Politics.index()] = TableView::Complete(TableRows::Politics(vec![
            PoliticsRow { country: 9001, capital: ger_capital, original_capital: ger_capital, ..Default::default() },
            PoliticsRow { country: 9002, capital: 1, original_capital: 1, ..Default::default() },
        ]));
        *f.tables[TableId::States.index()] = TableView::Complete(TableRows::States(vec![
            StateRow { id: 1, continent: Recorded::Value(Some(701)), ..Default::default() },
            StateRow { id: 2, continent: Recorded::Value(Some(702)), ..Default::default() },
        ]));
        *f.tables[TableId::LexicalTokens.index()] = TableView::Complete(TableRows::LexicalTokens(vec![
            LexicalTokenRow { token: 35, literal: b"syn_gun_a".to_vec() },
            LexicalTokenRow { token: 701, literal: b"syn_west".to_vec() },
            LexicalTokenRow { token: 702, literal: b"syn_east".to_vec() },
        ]));
        *f.tables[TableId::SimulationFeatures.index()] = TableView::Complete(TableRows::SimulationFeatures(vec![
            SimulationFeatureRow { dlc_mask: 0, human_ai: (h & 1) as u8, ..Default::default() },
        ]));
        let name_500 = if variant_branch {
            b"BranchTwo".to_vec()
        } else if h < FIRST + 3 {
            b"Alpha".to_vec()
        } else {
            b"Beta".to_vec()
        };
        let name_503 = if variant_branch {
            b"ExplicitBranchTwo".to_vec()
        } else if h < FIRST + 3 {
            b"ExplicitA".to_vec()
        } else {
            b"ExplicitB".to_vec()
        };
        *f.tables[TableId::VariantDefinitions.index()] = TableView::Complete(TableRows::VariantDefinitions(vec![
            VariantDefinitionRow { id: 500, equipment_token: 35, creator_link: 9001, obsolete: (h & 1) as u8, display_name: name_500, ..Default::default() },
            VariantDefinitionRow { id: 501, equipment_token: 35, creator_link: 9002, display_name: format!("Foreign{h}").into_bytes(), ..Default::default() },
            VariantDefinitionRow { id: 502, equipment_token: 35, creator_link: 9001, display_name: b"Challenger".to_vec(), ..Default::default() },
            VariantDefinitionRow { id: 503, equipment_token: 35, creator_link: 9002, display_name: name_503, ..Default::default() },
        ]));
        let level_502 = if !variant_branch && h >= FIRST + 5 { 3 } else { 0 };
        *f.tables[TableId::VariantUpgrades.index()] = TableView::Complete(TableRows::VariantUpgrades(vec![
            VariantUpgradeRow { variant: 500, source_ordinal: 0, definition_token: 35, level: 1 },
            VariantUpgradeRow { variant: 502, source_ordinal: 0, definition_token: 35, level: level_502 },
        ]));
        let mut available = vec![
            AvailableEquipmentVariantRow { country: 9001, roster_kind: 0, ordinal: 0, variant: h as u64 },
            AvailableEquipmentVariantRow { country: 9001, roster_kind: 2, ordinal: 0, variant: 500 },
            AvailableEquipmentVariantRow { country: 9001, roster_kind: 2, ordinal: 1, variant: 502 },
            AvailableEquipmentVariantRow { country: 9002, roster_kind: 0, ordinal: 0, variant: h as u64 },
            AvailableEquipmentVariantRow { country: 9002, roster_kind: 2, ordinal: 0, variant: h as u64 },
        ];
        if !variant_branch && h == FIRST + 7 {
            available.push(AvailableEquipmentVariantRow { country: 777, roster_kind: 2, ordinal: 0, variant: 501 });
        }
        available.sort_by_key(|row| (row.country, row.roster_kind, row.ordinal));
        *f.tables[TableId::AvailableEquipmentVariants.index()] = TableView::Complete(TableRows::AvailableEquipmentVariants(available.clone()));

        if !variant_branch {
            if (FIRST + 12..=FIRST + 14).contains(&h) {
                *f.tables[TableId::VariantDefinitions.index()] = TableView::Unavailable { reason: "variants damaged".into() };
            } else if (FIRST + 15..=FIRST + 16).contains(&h) {
                *f.tables[TableId::AvailableEquipmentVariants.index()] = TableView::Truncated {
                    rows: TableRows::AvailableEquipmentVariants(available[..1].to_vec()),
                    missing: 2,
                };
            } else if (FIRST + 17..=FIRST + 18).contains(&h) {
                *f.tables[TableId::CountryLinks.index()] = TableView::Unavailable { reason: "links damaged".into() };
            }
        }
        f
    };

    let frames_b1: Vec<DecodedFrame> = (FIRST..=FIRST + 21).map(|h| build_frame(1, h, false)).collect();
    let (source, content_load) = source_with_content(frames_b1.clone(), &machine.root);
    let source = Arc::new(source);
    let mut whole = frame_panel::<StockpileArtwork>(&source, &content_load, &art).unwrap();
    let panel = (STOCKPILE_STOCKPILEARTWORK.panel.unwrap().open)(&source, &content_load, &art).unwrap();
    let reader = panel.exact().unwrap();
    let seed = (reader.advance)(FIRST, true, &false.into(), None);
    if seed.pending {
        let prepare = panel.pending_preparation().and_then(|prepare| prepare(FIRST)).expect("pending artwork seed has preparation");
        let (ready, waited) = std::sync::mpsc::sync_channel(1);
        prepare(std::sync::Arc::new(move || { let _ = ready.send(()); }));
        waited.recv().unwrap();
    }
    let steps = played(source.replay_ref(), reader, FIRST, FIRST + 21);
    for step in &steps {
        let expected = materialized_frame(&mut *whole, source.replay_ref(), 1, step.hour);
        assert_eq!(*step.value, expected, "stockpile-artwork at {}", step.hour);
    }
    for quiet in [1usize, 2, 4, 6, 9, 11, 20, 21] {
        assert_eq!(steps[quiet].product_calls, 0, "quiet hour {} must not call product", FIRST + quiet as i32);
    }
    let entry_name = |step: usize, key: &str| {
        steps[step].value.as_ref().as_ref().unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == key)
            .and_then(|entry| entry["name"].as_str())
            .map(str::to_owned)
    };
    assert_eq!(entry_name(2, "variant:503").as_deref(), Some("ExplicitA"));
    assert_eq!(entry_name(3, "variant:503").as_deref(), Some("ExplicitB"));
    let at_4 = steps[4].value.as_ref().as_ref().unwrap();
    let at_5 = steps[5].value.as_ref().as_ref().unwrap();
    assert_ne!(at_4["entries"], at_5["entries"], "VariantUpgrades level change must flip best variant");
    assert!(steps[7].value.as_ref().as_ref().unwrap()["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|s| s.contains("777"))));
    for damaged in [12usize, 13, 14, 15, 16] {
        let h = FIRST + damaged as i32;
        assert!(steps[damaged].value.as_ref().as_ref().unwrap()["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|s| s.contains(&format!("hour {h} ")))));
    }
    for refused in [17usize, 18] {
        assert!(steps[refused].value.as_ref().as_ref().unwrap_err().contains("CountryLinks"));
    }
    for h in [FIRST + 2, FIRST + 6, FIRST + 13, FIRST + 15, FIRST + 17, FIRST + 20] {
        let expected = materialized_frame(&mut *whole, source.replay_ref(), 1, h);
        assert_eq!(*(reader.advance)(h, true, &false.into(), None).value, expected, "seek {h}");
    }

    let (options, identity) = content_identity(&machine.root);
    let mut b1 = records(1, frames_b1);
    let replay_capture_spool::spool::Record::SessionStart(session) = &mut b1[0] else {
        unreachable!()
    };
    session.identity = identity.clone();
    let mut b2 = records(2, (FIRST..=FIRST + 4).map(|h| build_frame(2, h, true)).collect());
    b2.remove(0);
    if let replay_capture_spool::spool::Record::BranchStart(start) = &mut b2[0] {
        start.session = 1;
        start.reason = replay_frame::branches::BranchReason::LoadSignal;
    }
    b1.extend(b2);
    let multi = with_facts(&options, &identity, b1).into_timelines(&Default::default()).unwrap();
    let multi_source = Arc::new(
        Source::from_replay(
            dashboard_base::SourceKey { source_id: "stockpile-artwork-branches".into(), revision: "1".into() },
            "memory.battleplan".into(),
            Arc::new(multi),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let multi_load = LoadContext { content: Some(options) };
    let branches = multi_source.metadata().unwrap().branches;
    assert_eq!(branches.len(), 2);
    let mut first_entries = Vec::new();
    for branch in &branches {
        let id: u64 = branch.id.parse().unwrap();
        let req = json!({"branchId": branch.id, "country": "GER", "variants": ["500", "503"]});
        let mut whole_b = frame_panel::<StockpileArtwork>(&multi_source, &multi_load, &req).unwrap();
        let panel_b = (STOCKPILE_STOCKPILEARTWORK.panel.unwrap().open)(&multi_source, &multi_load, &req).unwrap();
        let reader_b = panel_b.exact().unwrap();
        let seed_b = (reader_b.advance)(FIRST, true, &false.into(), None);
        if seed_b.pending {
            let prepare = panel_b.pending_preparation().and_then(|prepare| prepare(FIRST)).expect("pending artwork branch seed has preparation");
            let (ready, waited) = std::sync::mpsc::sync_channel(1);
            prepare(std::sync::Arc::new(move || { let _ = ready.send(()); }));
            waited.recv().unwrap();
        }
        for h in FIRST..=FIRST + 4 {
            let expected = materialized_frame(&mut *whole_b, multi_source.replay_ref(), id, h);
            let got = (reader_b.advance)(h, h == FIRST, &false.into(), None).value;
            assert_eq!(*got, expected, "branch {} at {h}", branch.id);
            if h == FIRST {
                first_entries.push(got.as_ref().as_ref().unwrap()["entries"].clone());
            }
        }
    }
    assert_ne!(first_entries[0], first_entries[1], "branches must produce distinct artwork names at the same hour");
}

#[test]
fn catalogue_refusals_follow_table_availability_and_missing_row_counts() {
    use dashboard_base::products::at_hour::exact_panel;
    const CAUSE: &str = "unread since hour 1000 in this recording";
    let source = from_frames((FIRST..=FIRST + 5).map(|hour| {
        let mut frame = frame(hour);
        for table in [TableId::VariantDefinitions, TableId::IndustrialOrganisations] {
            let view = &mut *frame.tables[table.index()];
            match hour - FIRST {
                0 => *view = TableView::Unavailable { reason: CAUSE.into() },
                count @ 3..=4 => {
                    let TableView::Complete(rows) = view else { panic!("expected catalogue rows") };
                    *view = TableView::Truncated { rows: rows.clone(), missing: (count - 2) as u32 };
                }
                _ => {}
            }
        }
        *frame.tables[TableId::LexicalTokens.index()] = if hour == FIRST + 1 {
            TableView::Unavailable { reason: CAUSE.into() }
        } else {
            TableView::Complete(TableRows::LexicalTokens(Vec::new()))
        };
        frame
    }).collect());
    let params = json!({ "branchId": "1", "country": "GER" });
    for (panel, title, table, list) in [
        (exact_panel::<EquipmentDesigns>(&source, &load(), &params), "Equipment designs", "VariantDefinitions", "designs"),
        (exact_panel::<IndustrialOrganisations>(&source, &load(), &params), "Industrial organisations", "IndustrialOrganisations", "organisations"),
    ] {
        let panel = panel.unwrap();
        let exact = panel.exact().unwrap();
        let reads = dashboard_base::test_fixtures::played(source.replay_ref(), exact, FIRST, FIRST + 5);
        assert_eq!(reads.len(), 6);
        for read in reads {
            assert!(!read.pending);
            let value = read.value.as_ref().as_ref().unwrap();
            let missing = match read.hour - FIRST {
                0 => Some(format!("{table} is unavailable: {CAUSE}")),
                count @ 3..=4 => Some(format!("{table} is truncated ({} missing rows)", count - 2)),
                _ => None,
            };
            assert_eq!(value["missing"], json!(missing.as_ref().map(|why| format!("{title} are not recorded at this hour: {why}"))));
            assert_eq!(value[list].as_array().unwrap().is_empty(), missing.is_some());
            let lexical = format!("LexicalTokens is unavailable: {CAUSE}");
            if read.hour == FIRST + 1 {
                let name = if list == "designs" { &value[list][0]["name"] } else { &value[list][0] };
                assert_eq!(name["nameUnknown"], lexical);
                assert_eq!(name["iconUnknown"], lexical);
                assert!(value["warnings"].as_array().unwrap().contains(&json!(format!("Production names were not read: {lexical}"))));
            }
            let cold = (exact.cold)(read.hour, &false.into()).unwrap();
            for field in ["missing", list, "warnings"] {
                assert_eq!(value[field], cold[field], "{title} {field} at {}", read.hour);
            }
        }
    }
}

fn moved_frame(hour: i32) -> DecodedFrame {
    let mut frame = frame(hour);
    if hour >= 1_150
        && let TableView::Complete(TableRows::CountryLinks(rows)) = &mut *frame.tables[TableId::CountryLinks.index()]
    {
        for row in rows.iter_mut().filter(|row| row.id == 11) {
            row.canonical_slot = 8;
        }
    }
    frame
}

fn moved_recording() -> Source {
    let frames = (FIRST..=LAST).map(moved_frame).collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    Source::from_replay(dashboard_base::SourceKey { source_id: "production-moved".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap()
}

fn factor(source: &Source, handle: &str, group: &str, hour: i32) -> std::io::Result<breakdown::ProductionLineBreakdownReply> {
    let params = ProductionBreakdownParams { branch_id: "1".into(), country: "GER".into(), handle: handle.into(), group: group.into(), hour };
    <ProductionLineBreakdown as dashboard_base::products::Product>::load(source, &params, &load(), &AtomicBool::new(false))
}

fn breakdown(source: &Source, handle: &str, hour: i32) -> std::io::Result<breakdown::ProductionLineBreakdownReply> {
    factor(source, handle, "efficiency", hour)
}

fn modified_recording() -> Source {
    let frames = (FIRST..=LAST)
        .map(|hour| {
            let mut frame = frame(hour);
            *frame.tables[TableId::ProductionLineModifiers.index()] = TableView::Complete(TableRows::ProductionLineModifiers(vec![ProductionLineModifiersRow {
                line: 1,
                organisation_dirty: (1_100..1_130).contains(&hour),
                ..Default::default()
            }]));
            *frame.tables[TableId::CountryProductionModifiers.index()] = if hour < 1_150 {
                TableView::Complete(TableRows::CountryProductionModifiers(vec![CountryProductionModifiersRow {
                    country: 11,
                    trees: engine_executable::native_values::production_modifiers::KEYS.into_iter().map(|key| ProductionModifierTree { key, nodes: vec![ModifierBreakdownNode { children: 0, value: 0, name: b"synthetic source".to_vec() }], failure: Vec::new() }).collect(),
                    ..Default::default()
                }]))
            } else {
                TableView::Unavailable { reason: "country modifiers unread in the fixture".into() }
            };
            frame
        })
        .collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    Source::from_replay(dashboard_base::SourceKey { source_id: "production-modified".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap()
}

#[test]
fn a_breakdown_is_the_clicked_line_and_its_operands_at_the_clicked_hour_alone() {
    let source = modified_recording();
    let pending = breakdown(&source, "1", 1_120).unwrap();
    assert_eq!(pending.hour, 1_120);
    assert_eq!(pending.group.as_ref().and_then(|group| group.total_raw.as_deref()), Some("5000000"), "the line's stored efficiency at the hour");
    assert!(pending.warnings.iter().any(|warning| warning.starts_with("Organisation bonuses were pending refresh")), "the hour's own warnings reach the reply: {:?}", pending.warnings);
    assert!(pending.warnings.iter().all(|warning| !warning.contains("not recorded")), "operands missing at a LATER hour say nothing about this one: {:?}", pending.warnings);
    let settled = breakdown(&source, "1", 1_140).unwrap();
    assert!(settled.warnings.iter().all(|warning| !warning.starts_with("Organisation bonuses")), "the pending refresh of an earlier hour is not this hour's: {:?}", settled.warnings);
    let unread = breakdown(&source, "1", 1_160).unwrap();
    let kept = unread.group.as_ref().expect("the witnessed efficiency total is kept when the country's modifiers are unread");
    assert_eq!(kept.total_raw.as_deref(), Some("5000000"));
    const UNREAD_MODIFIERS: &str = "Country production modifiers not available at this hour: country modifiers unread in the fixture";
    assert!(kept.parts.iter().all(|part| part.unknown.as_deref() == Some(UNREAD_MODIFIERS)), "the table's own reason names why: {:?}", kept.parts);
    assert!(unread.warnings.iter().any(|warning| warning == UNREAD_MODIFIERS), "and the kept total carries it as a warning: {:?}", unread.warnings);
    assert_eq!(unread.missing, None);
    assert_eq!(factor(&source, "1", "output", 1_160).unwrap().group.and_then(|group| group.total_raw), Some("200000".into()), "the recorded IC rate is the line's own");
    let start = factor(&source, "1", "start", 1_160).unwrap();
    assert_eq!((start.group, start.missing.as_deref()), (None, Some(UNREAD_MODIFIERS)));
    assert!(breakdown(&source, "1", UNREAD.0).is_err(), "the lines are unread at the clicked hour: nothing from another hour stands in");
}

#[test]
fn a_breakdown_opens_for_the_line_the_country_holds_at_the_clicked_hour() {
    let source = recording();
    let alias = breakdown(&source, "1", 1_120).expect("raw country 11's line is GER's: GER's slot holds it");
    assert_eq!(alias.hour, 1_120);
    assert!(breakdown(&source, "3", 1_120).is_err(), "ITA's line is not GER's");
    assert!(breakdown(&source, "2", 1_210).is_err(), "a line that ended at 1200 is not the country's after it");
    let moved = moved_recording();
    assert!(breakdown(&moved, "1", 1_120).is_ok());
    assert!(breakdown(&moved, "1", 1_160).is_err(), "the line leaves GER at the hour its raw country's link moves");
}

fn load() -> LoadContext {
    LoadContext { content: None }
}

fn asked(source: &Source, entry: dashboard_base::products::Registered, request: Value) -> Value {
    let text = (entry.load)(source, "lease", request, &load(), &AtomicBool::new(false)).unwrap();
    let mut reply: Value = serde_json::from_str(&text).unwrap();
    reply["data"].take()
}

fn cells(reply: &Value) -> std::collections::BTreeMap<(String, String), Result<f64, String>> {
    let live = &reply["live"];
    let flat: Vec<usize> = live["cells"].as_array().unwrap().iter().map(|item| item.as_u64().unwrap() as usize).collect();
    let mut out = std::collections::BTreeMap::new();
    let mut at = 0;
    while at < flat.len() {
        let (row, kind, count) = (flat[at], flat[at + 1], flat[at + 2]);
        let mut value: Result<f64, String> = Ok(0.0);
        for source in 0..count {
            let (group, key, column) = (flat[at + 3 + 3 * source], flat[at + 4 + 3 * source], flat[at + 5 + 3 * source]);
            let width = live["groups"][group]["columns"].as_array().unwrap().len();
            let held = &live["groups"][group]["values"][key * width + column];
            value = match (value, held.as_f64()) {
                (Ok(sum), Some(number)) => Ok(sum + number),
                (Err(why), _) => Err(why),
                (Ok(_), None) => Err(held.as_str().unwrap_or("no value").to_owned()),
            };
        }
        let scale = live["kinds"][kind]["scale"].as_f64().unwrap();
        out.insert((live["rows"][row].as_str().unwrap().to_owned(), live["kinds"][kind]["id"].as_str().unwrap().to_owned()), value.map(|sum| sum * scale));
        at += 3 + 3 * count;
    }
    out
}

fn cell(cells: &std::collections::BTreeMap<(String, String), Result<f64, String>>, row: &str, column: &str) -> Result<f64, String> {
    cells.get(&(row.to_owned(), column.to_owned())).cloned().unwrap_or_else(|| Err(format!("no cell ({row}, {column})")))
}

#[test]
fn the_live_table_lists_the_countrys_lines_in_queue_order_and_says_when_they_are_not_read() {
    let source = recording();
    let reply = asked(&source, entry::<ProductionLinesAt>(), json!({ "branchId": "1", "country": "GER", "hour": 1_120 }));
    let handles = |reply: &Value| reply["lines"].as_array().unwrap().iter().map(|line| line["handle"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(handles(&reply), ["2", "1"], "queue order; ITA's line is not GER's");
    assert!(reply["lines"][0]["output"]["parts"][0].get("units").is_none(), "units made move: they are a cell, not the reply's");
    let values = cells(&reply);
    assert_eq!(cell(&values, "2", "output"), Ok(2.0));
    assert_eq!(cell(&values, "1", "output"), Ok(5.0));
    assert_eq!((cell(&values, "1", "active"), cell(&values, "1", "requested")), (Ok(2.0), Ok(3.0)));
    assert_eq!((cell(&values, "1", "speed"), cell(&values, "1", "cost"), cell(&values, "1", "efficiency")), (Ok(200_000.0), Ok(700_000.0), Ok(1_638_400.0)));
    assert!(values.keys().all(|(row, _)| row == "1" || row == "2"), "no cell of ITA's line");
    assert!(reply["missing"].is_null());
    let by_factories = asked(&source, entry::<ProductionLinesAt>(), json!({ "branchId": "1", "country": "GER", "hour": 1_120, "order": "factories" }));
    assert_eq!(handles(&by_factories), ["1", "2"], "most factories first");
    let by_output = asked(&source, entry::<ProductionLinesAt>(), json!({ "branchId": "1", "country": "GER", "hour": 1_120, "order": "output" }));
    assert_eq!(handles(&by_output), ["2", "1"], "equal speeds keep the queue's order");
    let unread = asked(&source, entry::<ProductionLinesAt>(), json!({ "branchId": "1", "country": "GER", "hour": 1_245 }));
    assert!(unread["lines"].as_array().unwrap().is_empty());
    assert!(unread["missing"].as_str().is_some_and(|why| why.contains("not recorded")), "unread lines are missing, never none");
}

#[test]
fn production_lines_hold_through_spending_and_live_values_and_keep_sort_order() {
    use dashboard_base::{products::at_hour::exact_panel, test_fixtures::{played, streamed}};

    let source = from_frames((FIRST..=FIRST + 6).map(|hour| {
        let mut frame = frame(hour);
        if let TableView::Complete(TableRows::ProductionLines(rows)) = &mut *frame.tables[TableId::ProductionLines.index()] {
            rows.insert(1, line(2, 9001, 200, 1, 2));
            rows[0].efficiency_raw += i64::from(hour - FIRST);
            if hour >= FIRST + 3 { rows[0].active_factories = 0; }
            if hour >= FIRST + 4 { rows[0].speed_raw = 100_000; }
        }
        if let TableView::Complete(TableRows::ProductionLineOutput(rows)) = &mut *frame.tables[TableId::ProductionLineOutput.index()] {
            rows[0].units += i64::from(hour - FIRST);
            rows[0].spent_raw = Recorded::Value(Some(100_000 * i64::from(hour - FIRST)));
            rows.insert(1, output(2, 9001, 200, 0, false));
        }
        frame.tables[TableId::LexicalTokens.index()] = TableView::Complete(TableRows::LexicalTokens(Vec::new())).into();
        if hour >= FIRST + 5 {
            frame.tables[TableId::VariantDefinitions.index()] = TableView::Unavailable { reason: "definitions unavailable in the fixture".into() }.into();
        }
        frame
    }).collect());
    for order in ["game", "factories", "output"] {
        let panel = exact_panel::<ProductionLinesAt>(&source, &load(), &json!({"branchId":"1", "country":"GER", "order":order})).unwrap();
        let exact = panel.exact().unwrap();
        let reads = played(source.replay_ref(), exact, FIRST, FIRST + 6);
        assert_eq!(reads.len(), 7);
        for read in reads {
            assert!(!read.pending && read.value.is_ok(), "{order} at {}", read.hour);
            if (FIRST + 1..=FIRST + 4).contains(&read.hour) {
                let sort_changed = matches!((read.hour - FIRST, order), (3, "factories") | (4, "output"));
                assert_eq!(read.product_calls, u64::from(sort_changed), "{order} at {}", read.hour);
                if !sort_changed { assert_eq!(read.rows, 0, "{order} at {}", read.hour); }
            }
            let cold = (exact.cold)(read.hour, &false.into());
            assert_eq!(streamed(&read.value, read.fresh), streamed(&cold, read.fresh), "{order} at {}", read.hour);
            assert_eq!((exact.advance)(read.hour, true, &false.into(), None).value.as_ref(), &cold, "{order} live cells at {}", read.hour);
        }
    }
}

fn compare_panel<P: dashboard_base::products::at_hour::AtHour>(source: &Source, params: Value) {
    let mut as_panel = params.clone();
    as_panel["panel"] = json!(P::KEY);
    let mut panel = dashboard_base::products::at_hour::frame_panel::<P>(source, &load(), &as_panel).unwrap();
    for hour in (FIRST..=LAST).step_by(7).chain([1_049, 1_050, 1_199, 1_200, UNREAD.0, UNREAD.1 + 1]) {
        let mut request = params.clone();
        request["hour"] = json!(hour);
        let mut reply = asked(source, entry::<P>(), request);
        reply.as_object_mut().unwrap().remove("live");
        let replay::campaign::SeekResult::Frame(frame) = source.replay_ref().seek_selected(1, hour, Some(panel.tables())).unwrap() else { panic!("hour {hour} is recorded") };
        assert_eq!(panel.data(&frame), Ok(reply), "{} at hour {hour}", P::KEY);
    }
}

#[test]
fn a_request_reading_the_owners_rows_and_tokens_by_key_answers_as_the_panel_over_whole_tables() {
    let source = recording();
    compare_panel::<ProductionLinesAt>(&source, json!({ "branchId": "1", "country": "GER" }));
    compare_panel::<ProductionLinesAt>(&source, json!({ "branchId": "1", "country": "GER", "order": "factories" }));
    compare_panel::<IndustrialOrganisations>(&source, json!({ "branchId": "1", "country": "GER" }));
    compare_panel::<ProductionLicences>(&source, json!({ "branchId": "1" }));
}

#[test]
fn licences_name_both_parties_by_their_countries_and_organisations_say_when_their_state_is_unread() {
    let source = recording();
    let reply = asked(&source, entry::<ProductionLicences>(), json!({ "branchId": "1", "hour": 1_210, "subject": "GER" }));
    let endpoint = |reply: &Value, at: &Value| reply["endpoints"][at.as_u64().unwrap() as usize].clone();
    let incoming = reply["incoming"]["links"].as_array().unwrap();
    assert_eq!(incoming.len(), 1);
    assert_eq!((endpoint(&reply, &incoming[0]["from"])["tag"].as_str(), incoming[0]["total"].as_str()), (Some("ITA"), Some("1")), "the alias is GER");
    assert!(reply["outgoing"]["missing"].as_str().is_some() && reply["outgoing"]["total"].is_null(), "a licence whose giver cannot be linked may be GER's: that side is unproven, never empty");
    let world = asked(&source, entry::<ProductionLicences>(), json!({ "branchId": "1", "hour": 1_210 }));
    let links = world["links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "a licence with an unlinked giver stays one licence");
    let unlinked = links.iter().find(|link| endpoint(&world, &link["from"])["slot"].is_null()).unwrap();
    assert_eq!((endpoint(&world, &unlinked["to"])["tag"].as_str(), unlinked["total"].as_str()), (Some("ITA"), Some("1")));
    assert!(endpoint(&world, &unlinked["from"])["unknown"].as_str().is_some());
    let early = asked(&source, entry::<IndustrialOrganisations>(), json!({ "branchId": "1", "country": "GER", "hour": 1_050 }));
    let organisations = early["organisations"].as_array().unwrap();
    assert_eq!(organisations.len(), 1, "ITA's organisation is not GER's");
    assert!(organisations[0]["size"].is_null() && organisations[0]["detailsMissing"].as_str().is_some());
    let early_cells = cells(&early);
    assert!(cell(&early_cells, "40", "funds").is_err(), "funds not read are missing, never zero");
    let later = asked(&source, entry::<IndustrialOrganisations>(), json!({ "branchId": "1", "country": "GER", "hour": 1_150 }));
    let organisation = &later["organisations"][0];
    assert_eq!((organisation["size"].as_i64(), organisation["traits"].as_i64()), (Some(3), Some(2)));
    assert!(organisation.get("fundsRaw").is_none(), "funds move daily: they are a cell, not the reply's");
    let later_cells = cells(&later);
    assert_eq!((cell(&later_cells, "40", "funds"), cell(&later_cells, "40", "points")), (Ok(115_000.0), Ok(150.0)));
    assert!(organisation["policyToken"].is_null() && organisation["policyUnknown"].is_null(), "no policy selected is none, not unknown");
}

#[test]
fn a_countrys_designs_at_the_hour_are_its_creators_with_their_obsolete_flags_current_first() {
    let source = recording();
    let designs = |hour: i32| {
        let reply = asked(&source, entry::<EquipmentDesigns>(), json!({ "branchId": "1", "country": "GER", "hour": hour }));
        assert!(reply["designs"].as_array().unwrap().iter().all(|design| design["name"].get("hour").is_none()));
        assert!(reply["designs"].as_array().unwrap().iter().all(|design| design["name"]["name"].is_null() && design["name"]["nameUnknown"].is_string()));
        reply["designs"].as_array().unwrap().iter().map(|design| (design["name"]["id"].as_str().unwrap().to_owned(), design["obsolete"].as_bool().unwrap())).collect::<Vec<_>>()
    };
    assert_eq!(designs(1_050), [("500".to_owned(), false)], "502 is not saved yet; ITA's 501 is not GER's");
    assert_eq!(designs(1_250), [("502".to_owned(), false), ("500".to_owned(), true)]);
}

#[test]
fn weekly_units_need_a_positive_cost() {
    assert_eq!(weekly_units_raw(200_000, 700_000), Some(200_000));
    assert_eq!(weekly_units_raw(1, 0), None);
}

#[test]
fn artwork_queries_fetch_state_continent_tokens_appearance_refs_and_reserved_naming() {
    use crate::products::stockpile::artwork::{StockpileArtwork, StockpileArtworkParams};
    use dashboard_base::products::at_hour::{AtHour, Keys, Owner, ReadsAt};
    use dashboard_economy::products::economy::market::{MarketLendLease, MarketLendLeaseParams, MarketSection};
    use replay_data::rows::{
        CountryAppearanceRow, CountryMarketRow, LexicalTokenRow, PoliticsRow,
        ReservedCountryNamingRow, StateRow, tables,
    };

    assert!(names::TABLES.contains(&TableId::ReservedCountryNaming));
    assert!(names::WHOLE.contains(&TableId::ReservedCountryNaming));
    assert!(
        StockpileArtwork::tables(&StockpileArtworkParams {
            branch_id: "1".into(),
            country: "GER".into(),
            variants: Vec::new(),
        })
        .contains(&TableId::ReservedCountryNaming)
    );

    let mut frame = frame(FIRST);
    *frame.tables[TableId::CountryAppearance.index()] =
        TableView::Complete(TableRows::CountryAppearance(vec![CountryAppearanceRow {
            country: 9001,
            original_tag_reference: Recorded::Value(Some(41)),
            original_tag: Recorded::Value(Some(b"AAA".to_vec())),
            ..Default::default()
        }]));
    *frame.tables[TableId::ReservedCountryNaming.index()] =
        TableView::Complete(TableRows::ReservedCountryNaming(vec![
            ReservedCountryNamingRow {
                original_tag_reference: Recorded::Value(Some(41)),
                original_tag: Recorded::Value(Some(b"AAA".to_vec())),
                dynamic_tag_boundary: Recorded::Value(Some(50)),
                ..Default::default()
            },
        ]));
    *frame.tables[TableId::Politics.index()] = TableView::Complete(TableRows::Politics(vec![
        PoliticsRow {
            country: 9001,
            capital: 12,
            original_capital: 12,
            ..Default::default()
        },
    ]));
    *frame.tables[TableId::States.index()] = TableView::Complete(TableRows::States(vec![
        StateRow {
            id: 12,
            continent: Recorded::Value(Some(701)),
            ..Default::default()
        },
    ]));
    *frame.tables[TableId::LexicalTokens.index()] =
        TableView::Complete(TableRows::LexicalTokens(vec![
            LexicalTokenRow {
                token: 601,
                literal: b"tanks_1".to_vec(),
            },
            LexicalTokenRow {
                token: 701,
                literal: b"europe".to_vec(),
            },
        ]));
    *frame.tables[TableId::VariantDefinitions.index()] =
        TableView::Complete(TableRows::VariantDefinitions(vec![VariantDefinitionRow {
            id: 100,
            equipment_token: 601,
            creator_link: 11,
            ..Default::default()
        }]));
    *frame.tables[TableId::CountryMarket.index()] =
        TableView::Complete(TableRows::CountryMarket(vec![CountryMarketRow {
            country: 9001,
            prices: vec![replay_data::rows::MarketPriceLevel {
                variant: 100,
                level: 1,
            }]
            .into(),
            ..Default::default()
        }]));
    for table in [TableId::PoliticalParties, TableId::Factions, TableId::FactionMembers] {
        *frame.tables[table.index()] = TableView::Complete(TableRows::empty(table));
    }

    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, vec![frame])])
        .unwrap()
        .into_timelines(&Default::default())
        .unwrap();
    let source = Source::from_replay(
        dashboard_base::SourceKey {
            source_id: "artwork-reads".into(),
            revision: "1".into(),
        },
        "synthetic.battleplan".into(),
        Arc::new(replay),
        &AtomicBool::new(false),
    )
    .unwrap();
    let recording = source.replay_ref().recording();

    let live_params = live::ProductionLinesAtParams {
        branch_id: "1".into(),
        country: "GER".into(),
        order: None,
    };
    let live_ctx = ProductionLinesAt::context(&source, &load()).unwrap();
    let state_cols = ProductionLinesAt::projection(&live_params, TableId::States, &[], &live_ctx)
        .expect("States projection");
    assert!(
        !state_cols.is_empty(),
        "live production must project States.continent"
    );
    let app_cols =
        ProductionLinesAt::projection(&live_params, TableId::CountryAppearance, &[], &live_ctx)
            .expect("CountryAppearance projection");
    let expected_app = dashboard_base::products::at_hour::field_columns(
        TableId::CountryAppearance,
        &["original_tag", "original_tag_reference"],
    );
    assert!(
        expected_app.iter().all(|col| app_cols.contains(col)),
        "live production must project CountryAppearance original_tag and original_tag_reference"
    );

    let mut held = DecodedFrame {
        branch: 1,
        hour: FIRST,
        tables: std::array::from_fn(|_| replay_frame::frame::not_selected()),
        metrics: Default::default(),
    };
    let query = dashboard_base::queries::query(&source, 1).unwrap();
    let reads_at = ReadsAt {
        holders: &query.holders,
        run: 0,
        hour: FIRST,
        context: &live_ctx,
        since: None,
    };
    let owner = Some(Owner { id: 9001, slot: 7 });
    for step in 0..8 {
        let step_reads = ProductionLinesAt::reads_in(&live_params, owner, step, &held, &reads_at);
        if step_reads.is_empty() {
            break;
        }
        let projections: Vec<Option<Vec<usize>>> = step_reads
            .iter()
            .map(|read| ProductionLinesAt::projection(&live_params, read.table, &[], &live_ctx))
            .collect();
        let keyed: Vec<replay_lanes::at::Read<'_>> = step_reads
            .iter()
            .zip(&projections)
            .map(|(read, cols)| replay_lanes::at::Read {
                table: read.table,
                columns: cols.as_deref(),
                keys: match &read.keys {
                    Keys::All => replay_lanes::at::Keys::All,
                    Keys::These(keys) => replay_lanes::at::Keys::These(keys),
                    Keys::Range(low, high) => replay_lanes::at::Keys::Range(*low, *high),
                    Keys::Ranges(ranges) => replay_lanes::at::Keys::Ranges(ranges),
                },
            })
            .collect();
        let read = replay_lanes::at::frame(recording, 0, FIRST, &keyed).unwrap();
        for asked in &step_reads {
            held.tables[asked.table.index()] = read.tables[asked.table.index()].clone();
        }
    }
    let lexical = held.complete::<tables::LexicalTokens>().unwrap();
    assert!(
        lexical.iter().any(|row| row.token == 701 && row.literal == b"europe"),
        "live production must fetch distinct capital continent lexical token: {lexical:?}"
    );
    let states = held.complete::<tables::States>().unwrap();
    assert_eq!(states[0].continent, Recorded::Value(Some(701)));
    let appearance = held.complete::<tables::CountryAppearance>().unwrap();
    assert_eq!(appearance[0].original_tag_reference, Recorded::Value(Some(41)));
    assert_eq!(appearance[0].original_tag, Recorded::Value(Some(b"AAA".to_vec())));
    assert!(held.complete::<tables::ReservedCountryNaming>().is_ok());

    let market_params = MarketLendLeaseParams {
        branch_id: "1".into(),
        countries: Vec::new(),
        only: Some(MarketSection::Offers),
        lease_side: None,
        group: None,
        row: None,
    };
    let mut market_held = DecodedFrame {
        branch: 1,
        hour: FIRST,
        tables: std::array::from_fn(|_| replay_frame::frame::not_selected()),
        metrics: Default::default(),
    };
    for step in 0..8 {
        let step_reads = MarketLendLease::reads(&market_params, None, step, &market_held);
        if step_reads.is_empty() {
            break;
        }
        let keyed: Vec<replay_lanes::at::Read<'_>> = step_reads
            .iter()
            .map(|read| replay_lanes::at::Read {
                table: read.table,
                columns: None,
                keys: match &read.keys {
                    Keys::All => replay_lanes::at::Keys::All,
                    Keys::These(keys) => replay_lanes::at::Keys::These(keys),
                    Keys::Range(low, high) => replay_lanes::at::Keys::Range(*low, *high),
                    Keys::Ranges(ranges) => replay_lanes::at::Keys::Ranges(ranges),
                },
            })
            .collect();
        let read = replay_lanes::at::frame(recording, 0, FIRST, &keyed).unwrap();
        for asked in &step_reads {
            market_held.tables[asked.table.index()] = read.tables[asked.table.index()].clone();
        }
    }
    let market_lexical = market_held.complete::<tables::LexicalTokens>().unwrap();
    assert!(
        market_lexical
            .iter()
            .any(|row| row.token == 701 && row.literal == b"europe"),
        "market query must fetch distinct capital continent lexical token: {market_lexical:?}"
    );
    assert!(market_held.complete::<tables::ReservedCountryNaming>().is_ok());
}

#[test]
fn a_truncated_link_table_keeps_a_licence_whose_parties_it_witnessed_and_unlinked_givers_stay_distinct() {
    use super::catalogue::witnessed_licences;
    use dashboard_base::products::flows::{FlowEnd, FlowGap};
    let mut frame = frame(1_210);
    let links = match &*frame.tables[TableId::CountryLinks.index()] {
        TableView::Complete(TableRows::CountryLinks(rows)) => rows.clone(),
        _ => panic!("the fixture records country links"),
    };
    let ger = links.iter().find(|link| link.id == 11).map(|link| link.canonical_slot).unwrap();
    *frame.tables[TableId::CountryLinks.index()] = TableView::Truncated { rows: TableRows::CountryLinks(links), missing: 5 };
    let licences = vec![
        ProductionLicenseRecordRow { owner: 11, ordinal: 0, giver: 9002, ..Default::default() },
        ProductionLicenseRecordRow { owner: 11, ordinal: 1, giver: 77, ..Default::default() },
        ProductionLicenseRecordRow { owner: 11, ordinal: 2, giver: 78, ..Default::default() },
    ];
    *frame.tables[TableId::ProductionLicenseRecords.index()] = TableView::Complete(TableRows::ProductionLicenseRecords(licences.clone()));
    let read = witnessed_licences(&frame).unwrap();
    let ends: Vec<(FlowEnd, FlowEnd)> = read.edges.iter().map(|edge| (edge.from, edge.to)).collect();
    assert_eq!(ends, [(FlowEnd::Slot(8), FlowEnd::Slot(ger)), (FlowEnd::Raw(77), FlowEnd::Slot(ger)), (FlowEnd::Raw(78), FlowEnd::Slot(ger))], "both parties witnessed: the licence stands; two unlinked givers are two partners");
    assert!(read.unlinked[&77].contains("read only in part at this hour (5 rows not read)"), "an unlinked giver says why it is unlinked");
    assert!(read.gaps.is_empty());
    *frame.tables[TableId::ProductionLicenseRecords.index()] = TableView::Truncated { rows: TableRows::ProductionLicenseRecords(licences[..1].to_vec()), missing: 2 };
    let read = witnessed_licences(&frame).unwrap();
    assert_eq!(read.edges.len(), 1, "a truncated licence table keeps the licence it witnessed");
    assert!(matches!(&read.gaps[..], [gap] if gap.partial && gap.from == FlowGap::everything(String::new()).from && gap.reason.ends_with("read only in part at this hour (2 rows not read)")));
    *frame.tables[TableId::ProductionLicenseRecords.index()] = TableView::Unavailable { reason: "reader fault".into() };
    assert!(witnessed_licences(&frame).is_err());
}

#[test]
fn a_licence_subject_whose_holder_is_not_known_is_not_recorded_with_the_reason_though_the_licences_are_withheld_too() {
    let gapped = |hour: i32| {
        let mut frame = frame(hour);
        if (1_210..=1_220).contains(&hour) {
            *frame.tables[TableId::Countries.index()] = TableView::Unavailable { reason: "countries unread in the fixture".into() };
            *frame.tables[TableId::ProductionLicenseRecords.index()] = TableView::Unavailable { reason: "licences unread in the fixture".into() };
        }
        frame
    };
    let frames = (FIRST..=LAST).map(gapped).collect();
    let replay = Replay::from_records(recorder_derive::writers(), CAMPAIGN.into(), &[records(1, frames)]).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Source::from_replay(dashboard_base::SourceKey { source_id: "production-gap".into(), revision: "1".into() }, "synthetic.battleplan".into(), Arc::new(replay), &AtomicBool::new(false)).unwrap();
    let recorded = dashboard_base::queries::query(&source, 1).unwrap().holders.unknown_reason(1_215).map(str::to_owned).expect("the fixture's unread Countries hours are a holder gap");
    let during = asked(&source, entry::<ProductionLicences>(), json!({ "branchId": "1", "hour": 1_215, "subject": "GER" }));
    assert_eq!(during["missing"].as_str(), Some(format!("Who holds GER is not known: {recorded}").as_str()), "the holder reason, never the licence table's");
    let after = asked(&source, entry::<ProductionLicences>(), json!({ "branchId": "1", "hour": 1_230, "subject": "GER" }));
    let incoming = after["incoming"]["links"].as_array().unwrap();
    assert_eq!(incoming.len(), 1, "after the gap the licence from ITA reads again");
    assert_eq!(after["endpoints"][incoming[0]["from"].as_u64().unwrap() as usize]["tag"].as_str(), Some("ITA"));
}
