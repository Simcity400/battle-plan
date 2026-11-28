use std::sync::atomic::AtomicBool;
use super::*;
use dashboard_base::test_fixtures::{CAMPAIGN, content_identity, records, source, write_with_facts};
use replay::campaign::Replay;
use replay_data::rows::{CountryAppearanceRow, CountryLinkRow, ProvinceNameRow, ProvinceRow, Recorded, ReservedCountryNamingRow, TableRows, TableSchema};

mod malformed_original_tag;

fn set<T: TableSchema>(frame: &mut DecodedFrame, rows: Vec<T::Row>) {
    *frame.tables[T::ID.index()] = TableView::Complete(T::wrap(rows));
}

fn frame(hour: i32) -> DecodedFrame {
    let mut frame = source(1, hour, Some(0));
    set::<tables::CountryAppearance>(
        &mut frame,
        vec![CountryAppearanceRow {
            country: 9001,
            original_tag_reference: Recorded::Value(Some(1)),
            original_tag: Recorded::Value(Some(if hour < 106 { b"ORI".to_vec() } else { b"ALT".to_vec() })),
            ..Default::default()
        }],
    );
    set::<tables::ReservedCountryNaming>(
        &mut frame,
        vec![ReservedCountryNamingRow {
            tag: b"---".to_vec(),
            original_tag_reference: Recorded::Value(Some(0)),
            ..Default::default()
        }],
    );
    set::<tables::Provinces>(
        &mut frame,
        vec![
            ProvinceRow { id: 17, state: 64, owner: 9001, controller: 9001, victory_points: 10, ..Default::default() },
            ProvinceRow { id: 18, state: 64, owner: 9001, controller: 9001, ..Default::default() },
            ProvinceRow { id: 19, state: 64, owner: 9001, controller: 9001, ..Default::default() },
            ProvinceRow { id: 21, state: 0, owner: 0, controller: 0, ..Default::default() },
            ProvinceRow { id: 22, state: 0, owner: 0, controller: 0, ..Default::default() },
        ],
    );
    set::<tables::ProvinceNames>(&mut frame, vec![ProvinceNameRow { province: 18, name: b"RENAME".to_vec() }]);
    set::<tables::States>(
        &mut frame,
        vec![
            StateRow { id: 64, owner: 9001, controller: 9001, ..Default::default() },
            StateRow { id: 65, owner: 9001, custom_name: b"Custom state".to_vec(), ..Default::default() },
        ],
    );
    frame
}

fn fixture_source(frames: Vec<DecodedFrame>) -> (Source, LoadContext, std::path::PathBuf, impl Drop) {
    let mut definitions = String::from("0;0;0;0;land;false;unknown;0\n");
    for id in 1..=22_u32 {
        let (kind, coastal, terrain, continent) = if id >= 21 { ("sea", "false", "ocean", 0) } else { ("land", if id == 17 { "true" } else { "false" }, "plains", 1) };
        definitions.push_str(&format!("{id};{};0;0;{kind};{coastal};{terrain};{continent}\n", id * 10));
    }
    let (_cat, folder) = dashboard_base::catalogue::tests::folder_catalogue(&[
        ("hoi4.exe", "test"),
        ("user/settings.txt", "language=l_english force_pow2_textures=no"),
        ("user/dlc_load.json", r#"{"enabled_mods":[],"disabled_dlcs":[]}"#),
        (
            "common/defines/00_defines.lua",
            "NDefines = { NGame = { MAX_SCRIPTED_LOC_RECURSION = 23 }, NBuildings = { MAX_BUILDING_LEVELS = 13 }, NNavy = { BATTLE_NAME_VP_FACTOR = 1.0, BATTLE_NAME_VP_CUTOFF = 5.0 } }",
        ),
        ("map/default.map", "definitions=definition.csv provinces=provinces.bmp terrain=terrain.bmp rivers=rivers.bmp heightmap=heightmap.bmp tree_definition=trees.bmp"),
        ("map/definition.csv", &definitions),
        ("map/strategicregions/1.txt", "strategic_region={id=1 name=\"REG_SEA\" provinces={17 21}}"),
        ("map/strategicregions/2.txt", "strategic_region={id=2 name=\"REG_WITHOUT_NAME\" provinces={20}}"),
        ("history/states/mod.txt", "state={id=64 name=MOD_STATE provinces={17 18 19}}"),
        (
            "localisation/mod_l_english.yml",
            "\u{feff}l_english:\n MOD_STATE:0 \"Mod state\"\n ORI_VICTORY_POINTS_17:0 \"Original town\"\n ALT_VICTORY_POINTS_17:0 \"Switched town\"\n GER_VICTORY_POINTS_17:0 \"Own-tag town\"\n RENAME:0 \"Renamed town\"\n EMPTY_RENAME:0 \"\"\n REG_SEA:0 \"Sample Sea\"\n NAVAL_BATTLE_TITLE:0 \"Battle of $NAME$\"\n NAVAL_PORT_STRIKE_TITLE:0 \"Port Strike in $NAME$\"\n",
        ),
    ]);
    let mut bitmap = [0_u8; 58];
    bitmap[..2].copy_from_slice(b"BM");
    for (offset, value) in [(2, 58_u32), (10, 54), (14, 40), (18, 1), (22, 1)] {
        bitmap[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bitmap[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bitmap[28..30].copy_from_slice(&24_u16.to_le_bytes());
    std::fs::write(folder.0.join("map/provinces.bmp"), bitmap).unwrap();
    let (mut options, identity) = content_identity(&folder.0);
    let mut input = records(1, frames);
    let replay_capture_spool::spool::Record::SessionStart(session) = &mut input[0] else { unreachable!() };
    session.identity = identity.clone();
    let saved = std::env::temp_dir().join(format!("place-names-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&saved).unwrap();
    let path = saved.join("synthetic.battleplan");
    options.options.cache_dir = saved.join("cache");
    write_with_facts(&path, &options, &identity, input);
    let replay = Replay::open_newest(&path).unwrap().into_timelines(&Default::default()).unwrap();
    let source = Source::from_replay(dashboard_base::SourceKey { source_id: "place-names".into(), revision: "1".into() }, path, Arc::new(replay), &AtomicBool::new(false)).unwrap();
    (source, LoadContext { content: Some(options) }, saved, folder)
}

#[test]
fn the_region_catalogue_keeps_named_regions_when_their_membership_is_unread() {
    use super::super::sea_regions::{SeaContent, catalogue_entries};
    let (source, load, saved, _folder) = fixture_source(vec![frame(100)]);
    let content = Content::read(&source, &load).unwrap();
    let sea = SeaContent::read(&source, &load).unwrap();
    let known = serde_json::to_value(&catalogue_entries(&content, &sea)[0]).unwrap();
    assert_eq!(known["name"], "Sample Sea");
    assert_eq!(known["sea"], true);
    assert_eq!(known["land"], true);
    assert_eq!(known.get("membershipMissing"), Some(&serde_json::Value::Null));
    let reason = "fixture region membership read failed";
    let sea = SeaContent {
        sea: Err(reason.into()), land: Err(reason.into()), all: sea.all.clone(), minima: BTreeMap::new(), ratio: Ok(0), notes: vec![],
    };
    let unknown = serde_json::to_value(&catalogue_entries(&content, &sea)[0]).unwrap();
    assert_eq!(unknown["name"], known["name"]);
    assert_eq!(unknown["sea"], serde_json::Value::Null);
    assert_eq!(unknown["land"], serde_json::Value::Null);
    assert_eq!(unknown["membershipMissing"], reason);
    std::fs::remove_dir_all(saved).unwrap();
}

#[test]
fn pending_place_preparation_cannot_bind_a_replaced_folder() {
    use dashboard_base::{presentation::Presentation, source::Work};
    use std::sync::mpsc;

    let (source, old, saved, _folder) = fixture_source(vec![frame(100)]);
    let source = Arc::new(source);
    let options = old.content().unwrap();
    source.bind(Some((&options.options, options.allow_unverified)), Presentation::default());
    let old = LoadContext { content: source.admit(Work::Recorded) };
    let old_catalogue = source.in_scope(|| Content::read(&source, &old)).unwrap();

    let machine = dashboard_base::test_fixtures::visual_mods::Machine::new("place_preparation_binding");
    let old_options = &old.content().unwrap().options;
    let mut new_options = old_options.clone();
    new_options.game_dir = machine.root.clone();
    let (entered, has_entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let ready = Content::prepare_with(&source, &old, move || {
        entered.send(()).unwrap();
        released.recv().unwrap();
    }).unwrap().unwrap();
    has_entered.recv().unwrap();
    source.bind(Some((&new_options, old.content().unwrap().allow_unverified)), Presentation::default());
    let current = source.content(&new_options).unwrap();
    release.send(()).unwrap();

    let error = ready.wait().unwrap_err();
    assert!(source.bound_content(old_options).is_none());
    assert!(Arc::ptr_eq(&current, &source.bound_content(&new_options).unwrap()));
    assert_eq!(old_catalogue.cat.content_loads(), 0);
    assert_eq!(dashboard_core::refusal(&error), Some("Place names unavailable: the content selection changed during preparation"));
    drop(source);
    std::fs::remove_dir_all(saved).unwrap();
}

#[test]
fn places_are_named_by_rename_victory_point_then_state_and_a_place_not_read_is_a_reason() {
    use dashboard_base::{presentation::Presentation, source::Work};

    let (source, load, saved, _folder) = fixture_source((100..=110).map(frame).collect());
    let source = Arc::new(source);
    let options = load.content().unwrap();
    source.bind(Some((&options.options, options.allow_unverified)), Presentation::default());
    let load = LoadContext { content: source.admit(Work::Recorded) };
    let query = dashboard_base::queries::query(&source, 1).unwrap();
    let content = source.in_scope(|| {
        use super::super::place_name_runs::{PlacePoint, names_at};
        let content = Content::read(&source, &load).unwrap();
        let before = content.cat.content_loads();
        let rename = PlacePoint::Province(18, 105);
        let cancel = AtomicBool::new(false);
        let names = names_at(&source, &query, &load, &BTreeSet::from([rename]), &cancel).unwrap();
        assert_eq!(names[&rename].name.as_deref(), Some("Renamed town"));
        assert_eq!(content.cat.content_loads(), before, "a renamed point needs no catalogue warnings or state templates");
        if let Some(ready) = Content::prepare(&source, &load).unwrap() { ready.wait().unwrap(); }
        assert_eq!(content.cat.content_loads(), before + 1, "preparation warms the catalogue used by the point reader");
        assert!(Content::prepare(&source, &load).unwrap().is_none(), "preparation is shared");
        let state = PlacePoint::State(64, 105);
        let names = names_at(&source, &query, &load, &BTreeSet::from([state]), &cancel).unwrap();
        assert_eq!(names[&state].name.as_deref(), Some("Mod state"));
        assert!(Arc::ptr_eq(&content, &Content::read(&source, &load).unwrap()));
        assert_eq!(content.cat.content_loads(), before + 1, "the first state point reuses the warmed templates");
        assert!(content.regions.get().is_none(), "state points leave region membership unread");
        content
    });
    let mut frames = at_hour::Frames::new(&query).unwrap();
    let frame = read_hour(&query, &mut frames, 105, &[17, 18, 19, 20], &[64, 65, 66]).unwrap();
    let places = Places::new(&frame);
    let names = [17, 18, 19, 20].map(|id| places.province(&content, id, false));
    assert_eq!(names.each_ref().map(|p| p.name.as_deref()), [Some("Original town"), Some("Renamed town"), Some("Mod state"), None], "{names:?}");
    assert!(names[..3].iter().all(|p| p.missing.is_none()));
    assert_eq!(names[3].missing.as_deref(), Some("Not in this recording: province 20 at this hour"));
    assert!(read_hour(&query, &mut frames, 500, &[17], &[]).err().unwrap().starts_with("Not in this recording"));
    let states = [64, 65, 66].map(|id| places.state(&content, id));
    assert_eq!(states.each_ref().map(|s| s.name.as_deref()), [Some("Mod state"), Some("Custom state"), None]);
    assert_eq!(states[2].missing.as_deref(), Some("Not in this recording: state 66 at this hour"));

    let run = query.timeline.stretches[0].run;
    let history = PlaceHistory::load(query.replay.recording(), run, 100, 110, Keys::All, Keys::All, true, &AtomicBool::new(false)).unwrap();
    assert_eq!(history.province_name(&content, 18, ProvinceMode::Place, 105).name.as_deref(), Some("Renamed town"));
    assert!(content.regions.get().is_none(), "state names and recorded renames need no region index");
    assert_eq!(history.province_name(&content, 21, ProvinceMode::Place, 105).name.as_deref(), Some("Sample Sea"));
    let regions = content.regions.get().unwrap().as_ref().unwrap();
    let resolved_regions = || regions.by_region.values().filter(|name| name.get().is_some()).count();
    assert_eq!(resolved_regions(), 1, "a fallback resolves just its strategic region");
    let absent = super::super::region_names::region_name(&content.cat, &regions.definitions, 2).unwrap_err();
    assert!(absent.contains("REG_WITHOUT_NAME"), "{absent}");
    for _ in 0..2 {
        assert_eq!(content.region(2).as_deref(), Err(&absent), "the result retains the missing region name reason");
        assert_eq!(resolved_regions(), 2);
    }
    assert_eq!(history.province_change_hours(17, 100, 110), vec![100, 106]);
    assert_eq!(history.province_name(&content, 17, ProvinceMode::Land, 105).name.as_deref(), Some("Original town"));
    assert_eq!(history.province_name(&content, 17, ProvinceMode::Land, 106).name.as_deref(), Some("Switched town"));
    assert_eq!(history.land_battle_title(&content, 17, 105).as_deref(), Ok("Battle of Original town"));
    assert_eq!(history.naval_battle_title(&content, 21, false, 105).as_deref(), Ok("Battle of Original town"));
    assert_eq!(history.naval_battle_title(&content, 21, true, 105).as_deref(), Ok("Port Strike in Sample Sea"));
    assert_eq!(history.province_name(&content, 22, ProvinceMode::Naval, 105).name.as_deref(), Some("22"));
    assert!(history.province_name(&content, 22, ProvinceMode::Land, 105).name.is_none());

    drop(query);
    drop(source);
    std::fs::remove_dir_all(&saved).unwrap();
}

#[test]
fn a_dated_place_does_not_open_an_original_tag_reader_for_another_country() {
    use replay_data::rows::CountryRow;

    let mut at = frame(105);
    set::<tables::Countries>(&mut at, vec![
        CountryRow { id: 9001, canonical_slot: 7, tag: b"GER".to_vec(), ..Default::default() },
        CountryRow { id: 9002, canonical_slot: 8, tag: b"ENG".to_vec(), ..Default::default() },
    ]);
    set::<tables::CountryLinks>(&mut at, vec![
        CountryLinkRow { id: 9001, canonical_slot: 7 },
        CountryLinkRow { id: 9002, canonical_slot: 8 },
    ]);
    set::<tables::CountryAppearance>(&mut at, vec![
        CountryAppearanceRow { country: 9001, original_tag_reference: Recorded::Value(Some(0)), ..Default::default() },
        CountryAppearanceRow { country: 9002, original_tag_reference: Recorded::Value(Some(1)), original_tag: Recorded::Value(Some(b"ORI".to_vec())), ..Default::default() },
    ]);
    let (source, load, saved, _folder) = fixture_source(vec![at]);
    let query = dashboard_base::queries::query(&source, 1).unwrap();
    let content = Content::read(&source, &load).unwrap();
    let run = query.timeline.stretches[0].run;
    let (history, reads) = replay_recording::cells::count_reads(|| {
        PlaceHistory::load(query.replay.recording(), run, 105, 105, Keys::These(&[17]), Keys::These(&[]), false, &false.into()).unwrap()
    });
    assert_eq!(history.land_battle_title(&content, 17, 105).as_deref(), Ok("Battle of Own-tag town"));
    assert_eq!(reads[&TableId::CountryAppearance].opened, 1);
    drop(source);
    std::fs::remove_dir_all(saved).unwrap();
}

#[test]
fn truncated_tables_use_witnessed_rows_refuse_absent_renames_and_preserve_vp_tag_fallback_for_place_mode() {
    let frames: Vec<_> = (99..=103)
        .map(|hour| {
            let mut f = frame(hour);
            match hour {
                99 => set::<tables::ProvinceNames>(&mut f, Vec::new()),
                100 => {
                    *f.tables[TableId::ProvinceNames.index()] = TableView::Truncated {
                        rows: TableRows::ProvinceNames(vec![ProvinceNameRow { province: 18, name: b"RENAME".to_vec() }]),
                        missing: 2,
                    };
                }
                101 => {
                    *f.tables[TableId::CountryLinks.index()] = TableView::Truncated {
                        rows: TableRows::CountryLinks(vec![CountryLinkRow { id: 9001, canonical_slot: 7 }]),
                        missing: 1,
                    };
                }
                102 => {
                    set::<tables::CountryAppearance>(&mut f, Vec::new());
                }
                _ => {
                    set::<tables::ProvinceNames>(&mut f, vec![ProvinceNameRow { province: 18, name: b"EMPTY_RENAME".to_vec() }]);
                }
            }
            f
        })
        .collect();
    let (source, load, saved, _folder) = fixture_source(frames);
    let query = dashboard_base::queries::query(&source, 1).unwrap();
    let content = Content::read(&source, &load).unwrap();
    let mut frames = at_hour::Frames::new(&query).unwrap();
    let at_trunc_rename = read_hour(&query, &mut frames, 100, &[18, 19], &[]).unwrap();
    let places = Places::new(&at_trunc_rename);
    assert_eq!(places.province(&content, 18, false).name.as_deref(), Some("Renamed town"));
    let refused = places.province(&content, 19, false);
    assert_eq!(refused.name, None, "a truncated rename table never guesses the state name");
    assert!(refused.missing.as_deref().is_some_and(|why| why.contains("truncated")));

    let at_trunc_link = read_hour(&query, &mut frames, 101, &[17], &[]).unwrap();
    assert_eq!(Places::new(&at_trunc_link).province(&content, 17, false).name.as_deref(), Some("Original town"));

    let run = query.timeline.stretches[0].run;
    let history = PlaceHistory::load(query.replay.recording(), run, 100, 103, Keys::All, Keys::All, false, &AtomicBool::new(false)).unwrap();
    assert_eq!(history.province_name(&content, 18, ProvinceMode::Place, 100).name.as_deref(), Some("Renamed town"));
    let hist_refused = history.province_name(&content, 19, ProvinceMode::Place, 100);
    assert_eq!(hist_refused.name, None);
    assert!(hist_refused.missing.as_deref().is_some_and(|why| why.contains("truncated")));
    assert_eq!(history.province_name(&content, 17, ProvinceMode::Place, 101).name.as_deref(), Some("Original town"), "a witnessed link in a truncated CountryLinks table resolves its VP tag");
    let vp_fallback = history.province_name(&content, 19, ProvinceMode::Place, 102);
    assert_eq!(vp_fallback.name.as_deref(), Some("Mod state"));
    assert!(vp_fallback.missing.is_some());
    assert!(history.land_battle_title(&content, 19, 102).is_err(), "battle titles refuse when VP tag lookup fails");
    assert_eq!(history.province_name(&content, 18, ProvinceMode::Naval, 103).name.as_deref(), Some(""), "naval mode returns an empty translated rename immediately");
    assert_eq!(content.naval_battle_title_text("", false).as_deref(), Err(&"Battle place name not recorded".to_owned()));

    drop(query);
    drop(source);
    std::fs::remove_dir_all(&saved).unwrap();
}
