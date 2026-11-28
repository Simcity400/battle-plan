use content_reader_key_names::key_name_products::KeyNamesExt as _;
use super::*;
use crate::fixtures::{Folder, folder_catalogue};
use replay_data::{rows::LexicalTokenRow, rows::TableId};
use crate::fixtures::game;

fn decision() -> Decision {
    let id = b"sample_decision".to_vec();
    Decision {
        seed_component: content_reader_politics::decisions::seed_component(&id),
        name: id.clone(),
        id,
        ..Default::default()
    }
}

fn event_fixture() -> (Fixture, content_reader_politics::decisions::Database, Folder) {
    event_files_fixture(&[])
}

#[test]
fn custom_effect_tooltip_binds_nested_arguments_in_the_caller_scope() {
    game::enter();

    let (mut fixture, _, _folder) = event_files_fixture(&[(
        "localisation/bound_l_english.yml",
        concat!(
            "\u{feff}l_english:\n BOUND_ROOT:0 \"$NESTED$ | $NUMBER|0$ | $TARGET$ | $CHARACTER$\"\n",
            " BOUND_CHILD:0 \"<$WORD$>\"\n BOUND_WORD:0 \"[ROOT.GetTag]\"\n",
            " BOUND_WRONG:0 \"wrong\"\n BOUND_FIRST:0 \"first\"\n BOUND_SECOND:0 \"second\"\n"
        ),
    )]);
    set::<tables::LexicalTokens>(
        &mut fixture,
        vec![LexicalTokenRow {
            token: 83,
            literal: b"bound_integer".to_vec(),
        }],
    );
    let names = content_reader_script::key_names::KeyNames::default();
    let content = Content {
        key_names: Some(&names),
        ..Default::default()
    };
    let output = fixture.render_native(
        "fixture_written_tooltip={localization_key=BOUND_WRONG NESTED={WORD=BOUND_WORD localization_key=BOUND_CHILD} NUMBER=token:bound_integer TARGET=[ROOT.GetName] CHARACTER=AAA localization_key=BOUND_ROOT}",
        &mut super::super::tests::fresh(),
        content,
    );
    assert_eq!(output.text, "<AAA> | 83 | Alpha | Alpha");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);

    let output = fixture.render_native(
        "fixture_written_tooltip={localization_key=BOUND_CHILD WORD=BOUND_FIRST WORD=BOUND_SECOND}",
        &mut super::super::tests::fresh(),
        content,
    );
    assert_eq!(output.text, "<first>");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn custom_effect_tooltip_idea_callback_reuses_wrapper_and_missing_name() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[(
        "localisation/bound_l_english.yml",
        concat!(
            "\u{feff}l_english:\n BOUND_IDEA:0 \"Spirit [ROOT.GetTag]\"\n",
            " EFFECT_SHOW_IDEA_TOOLTIP:0 \"<$NAME$>\"\n BOUND_CHILD:0 \"{{$WORD$}\"\n"
        ),
    )]);
    let parsed = content_reader_politics::ideas::parse_ideas(
        b"ideas={country={BOUND_IDEA={}}}",
        &[],
        &Default::default(),
    );
    let ideas = content_reader_politics::ideas::IdeaContent::from_parsed(Vec::new(), parsed.ideas);
    let lists = content_reader_politics::names::NameLists::default();
    let names = content_reader_script::key_names::KeyNames::default();
    let content = Content {
        ideas: Some(&ideas),
        name_lists: Some(&lists),
        key_names: Some(&names),
        ..Default::default()
    };
    for (body, expected) in [
        ("fixture_written_tooltip=idea_desc|BOUND_IDEA", "<Spirit AAA>"),
        (
            "fixture_written_tooltip={localization_key=BOUND_CHILD WORD=idea_desc|BOUND_IDEA}",
            "{<Spirit AAA>}",
        ),
        ("fixture_written_tooltip=idea_desc|missing_idea", ""),
        ("fixture_written_tooltip={WORD=BOUND_IDEA}", ""),
    ] {
        let output = fixture.render_native(body, &mut super::super::tests::fresh(), content);
        assert_eq!(output.text, expected, "{body}");
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    }
}

#[test]
fn custom_effect_tooltip_refuses_only_the_unavailable_bound_fragment() {
    game::enter();
    let (fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_written_tooltip={localization_key=TIP VALUE=unsupported_formatter|something} fixture_written_tooltip=TIP",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "detail");
    assert_eq!(output.refusals.values().sum::<usize>(), 1);
    assert!(
        output
            .refusals
            .keys()
            .any(|why| why.contains("unsupported_formatter"))
    );
}

#[test]
fn unscoped_bound_callbacks_use_null_context_text() {
    game::enter();
    let (fixture, _, _folder) = event_fixture();
    fixture.with_renderer(
        &mut super::super::tests::fresh(),
        Content::default(),
        |renderer| {
            for callback in ["idea_desc", "tech_effect"] {
                let item =
                    content_reader_parser::parser::parse(format!("name={callback}|missing").as_bytes())
                        .unwrap()
                        .items
                        .remove(0);
                let bound = content_reader_script::localization_binding::BoundLocalization::from_value(&item.value);
                assert_eq!(
                    renderer.bound_localisation(&bound, Scopes::of(Scope::Country(1)), false),
                    Ok("NULL".to_owned())
                );
            }
        },
    );
}

#[test]
fn format99_inactive_decision_seed_uses_first_stored_random_item() {
    game::enter();
    let (mut fixture, database) = decision_fixture(Some(None));
    set::<tables::Decisions>(&mut fixture, Vec::new());
    let definition = database.decision(b"SYN_DECISION").unwrap().unwrap();
    set::<tables::DecisionRandomItems>(
        &mut fixture,
        vec![
            DecisionRandomItemRow {
                country: 100,
                ordinal: 2,
                token: b"syn_decision".to_vec(),
                count: 9,
                target: 2,
            },
            DecisionRandomItemRow {
                country: 100,
                ordinal: 0,
                token: b"SYN_DECISION".to_vec(),
                count: -3,
                target: 2,
            },
            DecisionRandomItemRow {
                country: 200,
                ordinal: 0,
                token: b"SYN_DECISION".to_vec(),
                count: 70,
                target: 2,
            },
        ],
    );
    fixture.with_renderer(
        &mut super::super::tests::fresh(),
        Content::default(),
        |renderer| {
            assert_eq!(
                renderer
                    .decision_description_seed(definition, None, 1, 2)
                    .map_err(str::to_owned),
                decision_seed(0, definition, Some(Some(-3)), 2)
            );
            assert_eq!(
                renderer
                    .decision_description_seed(definition, None, 1, 3)
                    .map_err(str::to_owned),
                decision_seed(0, definition, Some(None), 3)
            );
            let selected = renderer
                .decision_seeded_scope(definition, 1, Some(2), 0, false)
                .unwrap();
            let graph = renderer.graph.borrow();
            let own = &graph.scopes[selected.cursor.unwrap()];
            let seed = decision_seed(0, definition, Some(Some(-3)), 2).unwrap();
            assert_eq!(own.random_seed, random::scope_seed(seed));
            assert_eq!(own.random_counter, random::scope_counter(seed));
            assert!(own.random_seed_refusal.is_none());
        },
    );
}

#[test]
fn format99_unlock_decision_reads_instant_flag_only_for_positive_delay() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_fixture();
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.complete_effect = content_reader_parser::parser::parse(b"fixture_written_tooltip=TIP")
        .unwrap()
        .items;
    definition.days_remove = b"7".to_vec();
    let names = content_reader_script::key_names::KeyNames::default();
    let content = Content {
        decisions: Some(&database),
        key_names: Some(&names),
        ..Default::default()
    };
    for (instant, days) in [(0, 7), (255, 1)] {
        set::<tables::SimulationFeatures>(
            &mut fixture,
            vec![SimulationFeatureRow {
                instant_decisions: Recorded::Value(Some(instant)),
                ..Default::default()
            }],
        );
        let output = fixture.render_native(
            "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
            &mut super::super::tests::fresh(),
            content,
        );
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
        assert_eq!(
            output.text,
            format!("Unlock Agenda AAA\nSelect:\ndetailAfter {days}:\n\n")
        );
    }
    for instant_decisions in [Recorded::Value(None), Recorded::NotRecorded] {
        set::<tables::SimulationFeatures>(
            &mut fixture,
            vec![SimulationFeatureRow {
                instant_decisions,
                ..Default::default()
            }],
        );
        let output = fixture.render_native(
            "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
            &mut super::super::tests::fresh(),
            content,
        );
        assert_eq!(output.text, "Unlock Agenda AAA\nSelect:\ndetail\n");
        assert!(
            output
                .refusals
                .keys()
                .any(|why| why.contains("instant decisions of simulation features"))
        );
    }
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.days_remove = b"0".to_vec();
    let output = fixture.render_native(
        "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
        &mut super::super::tests::fresh(),
        Content {
            decisions: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

fn event_files_fixture(
    events: &[(&str, &str)],
) -> (Fixture, content_reader_politics::decisions::Database, Folder) {
    let (mut fixture, mut database) = decision_fixture(Some(None));
    let mut files = vec![
        (
            "common/defines/test.lua",
            "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}",
        ),
        (
            "localisation/events_l_english.yml",
            concat!(
                "\u{feff}l_english:\n AAA:0 \"Alpha\"\n BBB:0 \"Beta\"\n AAA_DEF:0 \"alpha\"\n BBB_DEF:0 \"beta\"\n",
                " SYN_NAME:0 \"Agenda [FROM.GetTag]\"\n SYN_CATEGORY:0 \"Panel [ROOT.GetTag]\"\n",
                " EFFECT_ACTIVATE_DECISION:0 \"Activate $NAME$\"\n EFFECT_REMOVE_DECISION:0 \"Remove $NAME$\"\n",
                " EFFECT_REMOVE_MISSION:0 \"Cancel $NAME$\"\n EFFECT_ACTIVATES_MISSION:0 \"Mission $NAME$\"\n",
                " ADD_DAYS_TO_MISSION:0 \"Add $DAYS|0$ to $MISSION$\"\n REMOVE_DAYS_TO_MISSION:0 \"Subtract $DAYS|0$ from $MISSION$\"\n",
                " EFFECT_UNLOCK_DECISION_CATEGORY:0 \"Unlock $NAME$\"\n EFFECT_UNLOCK_DECISION:0 \"Unlock $NAME$$EFFECT$\"\n EFFECT_UNLOCK_DECISION_BOP:0 \"Balance $NAME$$EFFECT$\"\n",
                " EFFECT_UNLOCK_DECISION_SELECT_HEADER:0 \"Select:\"\n EFFECT_UNLOCK_DECISION_REMOVE_HEADER:0 \"Remove:\"\n EFFECT_UNLOCK_DECISION_TIMED_HEADER:0 \"After $DAYS|0$:\"\n",
                " DECISION_WHILE_ACTIVE:0 \"Active [ROOT.GetTag]\"\n POWER:0 \"Power\"\n COST:0 \"Cost\"\n MOD_BODY:0 \"Body [ROOT.GetTag]\"\n",
                " EFFECT_ACTIVATE_TARGETED_DECISION:0 \"Targeted $NAME$\"\n TIP:0 \"detail\"\n EFFECT_COUNTRY_EVENT:0 \"Event $EVENT$\"\n SYN_EVENT:0 \"News [FROM.GetName]\"\n",
                " EFFECT_SHOW_MIO_TOOLTIP_MIO_NAME:0 \"Organisation $NAME$\"\n EFFECT_SHOW_MIO_TOOLTIP_INITIAL_TRAIT_NAME:0 \"Trait $NAME$\"\n",
                " SYN_MIO_NAME:0 \"Workshop\"\n BBB_SYN_MIO_NAME:0 \"Workshop [FROM.GetTag]\"\n SYN_MIO_initial_trait:0 \"Craft\"\n EFFECT_MIO_UNLOCK_MIO:0 \"Unlock $NAME$ [ROOT.GetTag]\"\n EFFECT_MIO_UNLOCK_POLICY:0 \"Policy $NAME$\"\n SYN_POLICY:0 \"Tools\"\n"
            ),
        ),
    ];
    files.extend_from_slice(events);
    let (catalogue, folder) = folder_catalogue(&files);
    catalogue
        .scripted_content()
        .unwrap()
        .set_synthetic_environment(Default::default());
    fixture.catalogue = catalogue;
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.name = b"SYN_NAME".to_vec();
    definition.has_state_targets = false;
    definition.target_states.clear();
    definition.fixed_random_seed = true;
    database
        .categories
        .insert(b"syn_category".to_vec(), b"SYN_CATEGORY".to_vec());
    (fixture, database, folder)
}

#[test]
fn decision_and_mission_description_cases() {
    game::enter();
    let (fixture, database, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let content = Content {
        decisions: Some(&database),
        key_names: Some(&names),
        ..Default::default()
    };
    for (case, body, expected) in [
        (
            "activate_mission_scalar",
            "activate_mission=SYN_DECISION",
            "Mission Agenda AAA\n",
        ),
        (
            "remove_mission_scalar",
            "remove_mission=SYN_DECISION",
            "Cancel Agenda AAA\n",
        ),
        (
            "activate_decision_scalar",
            "activate_decision=SYN_DECISION",
            "Activate Agenda AAA\n",
        ),
        (
            "remove_decision_scalar",
            "remove_decision=SYN_DECISION",
            "Remove Agenda AAA\n",
        ),
        (
            "activate_mission_tooltip_scalar",
            "activate_mission_tooltip=SYN_DECISION",
            "Mission Agenda AAA\n",
        ),
        (
            "add_days_mission_timeout_block",
            "fixture_mission_clock={days=47 mission=SYN_DECISION}",
            "Add 47 to Agenda AAA\n",
        ),
        (
            "unlock_decision_category_scalar",
            "fixture_decision_group=SYN_CATEGORY",
            "Unlock Panel AAA\n",
        ),
        (
            "unlock_decision_flags",
            "fixture_decision_preview={show_modifiers=yes decision=SYN_DECISION show_effect_tooltip=yes}",
            "Unlock Agenda AAA\n",
        ),
        (
            "unlock_decision_missing_definition_keeps_wrapper",
            "fixture_decision_preview=missing",
            "Unlock \n",
        ),
        (
            "remove_decision_on_cooldown_scalar",
            "fixture_decision_cooldown=SYN_DECISION",
            "Remove Agenda AAA\n",
        ),
        (
            "mission_timeout_negative_rounds_half_away",
            "fixture_mission_clock={mission=SYN_DECISION days=-0.5}",
            "Subtract 1 from Agenda AAA\n",
        ),
        (
            "mission_timeout_zero_is_empty",
            "fixture_mission_clock={mission=SYN_DECISION days=0.49999}",
            "",
        ),
        (
            "mission_tooltip_appends_desc_to_name",
            "activate_mission_tooltip={mission=SYN_DECISION desc=TIP}",
            "Mission Agenda AAAdetail\n\n",
        ),
    ] {
        let output = fixture.render_native(body, &mut super::super::tests::fresh(), content);
        assert_eq!(output.text, expected, "{case}");
        assert!(output.refusals.is_empty(), "{case}: {:?}", output.refusals);
    }
}

#[test]
fn unlock_decision_deterministic_body_needs_no_active_seed_row() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_fixture();
    set::<tables::Decisions>(&mut fixture, Vec::new());
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.fixed_random_seed = false;
    definition.complete_effect = content_reader_parser::parser::parse(b"fixture_written_tooltip=TIP")
        .unwrap()
        .items;
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
        &mut super::super::tests::fresh(),
        Content {
            decisions: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Unlock Agenda AAA\nSelect:\ndetail\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn unlock_decision_random_body_refuses_only_at_selection_without_seed_row() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_fixture();
    set::<tables::Decisions>(&mut fixture, Vec::new());
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.fixed_random_seed = false;
    definition.complete_effect = content_reader_parser::parser::parse(
        b"fixture_pick_owned_state={limit={state=1} fixture_written_tooltip=TIP}",
    )
    .unwrap()
    .items;
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
        &mut super::super::tests::fresh(),
        Content {
            decisions: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Unlock Agenda AAA\n");
    assert_eq!(output.refusals.values().sum::<usize>(), 1);
    assert!(
        output
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains("fixture_pick_owned_state")
    );
    assert!(
        output
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains(DECISION_RANDOM_COUNT_GAP)
    );
}

#[test]
fn unlock_decision_modifiers_use_full_mask_and_ordered_target_headings() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_fixture();
    set::<tables::Politics>(
        &mut fixture,
        vec![
            PoliticsRow {
                country: 100,
                ruling_party: Some(1),
                ..Default::default()
            },
            PoliticsRow {
                country: 200,
                ..Default::default()
            },
        ],
    );
    set::<tables::LexicalTokens>(
        &mut fixture,
        vec![
            replay_data::rows::LexicalTokenRow {
                token: 31,
                literal: b"political_power_gain".to_vec(),
            },
            replay_data::rows::LexicalTokenRow {
                token: 32,
                literal: b"political_power_cost".to_vec(),
            },
        ],
    );
    set::<tables::ModifierDefinitions>(
        &mut fixture,
        vec![
            replay_data::rows::ModifierDefinitionRow {
                index: 0,
                token: 31,
                name_key: b"POWER".to_vec(),
                category_mask: 0x0100_0000,
                precision: 2,
                flags: crate::modifier_text::POSITIVE_IS_GOOD,
                ..Default::default()
            },
            replay_data::rows::ModifierDefinitionRow {
                index: 1,
                token: 32,
                name_key: b"COST".to_vec(),
                category_mask: 1,
                precision: 2,
                flags: crate::modifier_text::POSITIVE_IS_GOOD,
                ..Default::default()
            },
        ],
    );
    let parse = |body: &[u8]| content_reader_parser::parser::parse(body).unwrap().items;
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.modifier = parse(b"political_power_gain=0.1 custom_modifier_tooltip=MOD_BODY");
    definition.targeted_modifier = vec![
        parse(b"tag=BBB political_power_gain=0.2 custom_modifier_tooltip=MOD_BODY"),
        parse(b"tag=ROOT political_power_cost=-0.3"),
        parse(b"tag=capital custom_modifier_tooltip=MOD_BODY"),
    ];
    let masked = content_reader_script::modifiers::values(&parse(b"political_power_gain=0.1"));
    fixture.with_renderer(
        &mut super::super::tests::fresh(),
        Content::default(),
        |renderer| {
            assert_eq!(
                renderer
                    .modifier_description_with(&masked, "", Default::default())
                    .unwrap(),
                ""
            );
        },
    );
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_decision_preview={decision=SYN_DECISION show_modifiers=yes}",
        &mut super::super::tests::fresh(),
        Content {
            decisions: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(
        output.text,
        concat!(
            "Unlock Agenda AAA\nActive AAA\nPower: \u{11}G+0.10\u{11}!\nBody AAA\n",
            "\u{11}H\u{12}BBB Beta\u{11}!\n  Power: \u{11}G+0.20\u{11}!\nBody AAA\n",
            "\u{11}H\u{12}AAA Alpha\u{11}!\n  Cost: \u{11}R-0.30\u{11}!\n",
        )
    );
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn activate_targeted_decision_saved_target_without_active_instance() {
    game::enter();

    let (mut fixture, mut database, _folder) = event_fixture();
    set::<tables::Decisions>(&mut fixture, Vec::new());
    database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap()
        .fixed_random_seed = false;
    let names = content_reader_script::key_names::KeyNames::default();
    let mut scopes = super::super::tests::fresh();
    scopes
        .saved_targets
        .insert(0, vec![(b"synthetic".to_vec(), EventObject::Country(2))]);
    let output = fixture.render_native(
        "fixture_select_decision_target={decision=SYN_DECISION target=event_target:synthetic}",
        &mut scopes,
        Content {
            decisions: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Targeted Agenda BBB\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn activate_targeted_decision_random_title_needs_seed_only_on_default_draw() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_files_fixture(&[
        (
            "common/scripted_localisation/random.txt",
            concat!(
                "a={name=Pick text={random_list={1={localization_key=TIP} 1={localization_key=SYN_EVENT}}}} ",
                "a={name=Single text={random_list={seed=unknown 0={localization_key=TIP}}}} ",
                "a={name=Explicit text={random_list={seed=7 1={localization_key=TIP} 1={localization_key=TIP}}}}"
            ),
        ),
        (
            "localisation/random_l_english.yml",
            "\u{feff}l_english:\n RANDOM_NAME:0 \"[Pick]\"\n SINGLE_NAME:0 \"[Single]\"\n EXPLICIT_NAME:0 \"[Explicit]\"\n",
        ),
    ]);
    set::<tables::Decisions>(&mut fixture, Vec::new());
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.fixed_random_seed = false;
    let names = content_reader_script::key_names::KeyNames::default();
    for (name, expected, refused) in [
        (b"RANDOM_NAME".as_slice(), "", true),
        (b"SINGLE_NAME".as_slice(), "Targeted detail\n", false),
        (b"EXPLICIT_NAME".as_slice(), "Targeted detail\n", false),
    ] {
        database
            .entries
            .get_mut(b"syn_decision".as_slice())
            .unwrap()
            .name = name.to_vec();
        let output = fixture.render_native(
            "fixture_select_decision_target={target=BBB decision=SYN_DECISION}",
            &mut super::super::tests::fresh(),
            Content {
                decisions: Some(&database),
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(output.text, expected);
        assert_eq!(
            output.refusals.values().sum::<usize>(),
            usize::from(refused)
        );
        if refused {
            let reason = output.refusals.keys().next().unwrap();
            assert!(reason.contains("fixture_select_decision_target"), "{reason}");
            assert!(reason.contains(DECISION_RANDOM_COUNT_GAP), "{reason}");
        }
    }
}

#[test]
fn missing_decision_seed_propagates_through_event_title_lanes_without_refusing_plain_title() {
    game::enter();
    let (mut fixture, mut database, _folder) = event_files_fixture(&[
        (
            "common/scripted_localisation/random.txt",
            "a={name=Pick text={random_list={1={localization_key=TIP} 1={localization_key=SYN_EVENT}}}}",
        ),
        (
            "localisation/random_l_english.yml",
            "\u{feff}l_english:\n RANDOM_NAME:0 \"[Pick]\"\n",
        ),
    ]);
    set::<tables::Decisions>(&mut fixture, Vec::new());
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.fixed_random_seed = false;
    definition.complete_effect = content_reader_parser::parser::parse(b"country_event={id=synthetic.1}")
        .unwrap()
        .items;
    let names = content_reader_script::key_names::KeyNames::default();
    for (title, expected, refused) in [
        (
            b"title=SYN_EVENT".as_slice(),
            "Unlock Agenda AAA\nSelect:\nEvent News Alpha\n",
            false,
        ),
        (b"title=RANDOM_NAME".as_slice(), "Unlock Agenda AAA\n", true),
    ] {
        let event = std::sync::Arc::new(content_reader_events::events::Event {
            titles: content_reader_parser::parser::parse(title).unwrap().items,
            ..Default::default()
        });
        let lookup = |_: &[u8]| Ok(event.clone());
        let output = fixture.render_native(
            "fixture_decision_preview={decision=SYN_DECISION show_effect_tooltip=yes}",
            &mut super::super::tests::fresh(),
            Content {
                decisions: Some(&database),
                events: Some(&lookup),
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(output.text, expected);
        assert_eq!(
            output.refusals.values().sum::<usize>(),
            usize::from(refused)
        );
        if refused {
            assert!(
                output
                    .refusals
                    .keys()
                    .next()
                    .unwrap()
                    .contains(DECISION_RANDOM_COUNT_GAP)
            );
        }
    }
}

#[test]
fn news_event_id_and_timing_fields() {
    game::enter();
    let (fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let event = std::sync::Arc::new(content_reader_events::events::Event {
        titles: content_reader_parser::parser::parse(b"title=SYN_EVENT")
            .unwrap()
            .items,
        ..Default::default()
    });
    let lookup = |_: &[u8]| Ok(event.clone());
    let output = fixture.render_native(
        "fixture_announce_event={hours=19 id=synthetic.73}",
        &mut super::super::tests::fresh(),
        Content {
            events: Some(&lookup),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Event News Alpha");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn state_event_controller_keeps_state_and_from_root() {
    game::enter();

    let (fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let event = std::sync::Arc::new(content_reader_events::events::Event {
        titles: content_reader_parser::parser::parse(b"title=SYN_EVENT")
            .unwrap()
            .items,
        ..Default::default()
    });
    let lookup = |_: &[u8]| Ok(event.clone());
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::State(2))]);
    let output = fixture.render_native(
        "fixture_notify_state={trigger_for=controller id=synthetic.62}",
        &mut scopes,
        Content {
            events: Some(&lookup),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Event News Two");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    assert!(scopes.scopes.iter().any(|scope| scope.this
        == EventObject::CountryState {
            country: 1,
            state: 2
        }));
}

#[test]
fn show_mio_template_name_initial_trait_and_no_line_feed() {
    game::enter();

    let (fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let mut database = content_reader_military::industrial_organisations::Database::default();
    database
        .read_file(b"SYN_MIO={name=SYN_MIO_NAME initial_trait={}}", &names)
        .unwrap();
    let output = fixture.render_native(
        "fixture_organisation_details=SYN_MIO",
        &mut super::super::tests::fresh(),
        Content {
            industrial_organisations: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Organisation Workshop Trait Craft");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

const UNLOCK_MIO_REFERENCE: u64 = 6 << 32 | 79;

fn unlock_mio_fixture() -> (Fixture, Folder) {
    use replay_data::{
        rows::IndustrialOrganisationRow, rows::LexicalTokenRow, rows::ScriptVariableEntry,
        rows::ScriptVariablesRow,
    };
    let (mut fixture, _, folder) = event_fixture();
    set::<tables::LexicalTokens>(
        &mut fixture,
        vec![LexicalTokenRow {
            token: 11,
            literal: b"SYN_MIO".to_vec(),
        }],
    );
    set::<tables::IndustrialOrganisations>(
        &mut fixture,
        vec![
            IndustrialOrganisationRow {
                country: 100,
                owner: 100,
                token: 11,
                ordinal: 1,
                organisation: 7 << 32 | 79,
                name: b"wrong name".to_vec(),
                ..Default::default()
            },
            IndustrialOrganisationRow {
                country: 100,
                owner: 200,
                token: 11,
                ordinal: 0,
                organisation: UNLOCK_MIO_REFERENCE,
                name: b"SYN_MIO_NAME".to_vec(),
                ..Default::default()
            },
        ],
    );
    set::<tables::ScriptVariables>(
        &mut fixture,
        vec![ScriptVariablesRow {
            scope_kind: 1,
            scope_id: 100,
            entries: vec![ScriptVariableEntry {
                name: b"syn_carrier".to_vec(),
                value_raw: 0x6000_0006,
            }],
            ..Default::default()
        }],
    );
    (fixture, folder)
}

#[test]
fn unlock_mio_operand_uses_recorded_instance_and_owner_name() {
    game::enter();

    let (fixture, _folder) = unlock_mio_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_offer_organisation=mio:SYN_MIO",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Unlock Workshop BBB AAA\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn show_mio_instance_reuses_the_shared_owner_name_path() {
    game::enter();
    let (fixture, _folder) = unlock_mio_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let mut database = content_reader_military::industrial_organisations::Database::default();
    database
        .read_file(b"SYN_MIO={name=SYN_MIO_NAME initial_trait={}}", &names)
        .unwrap();
    let output = fixture.render_native(
        "fixture_organisation_details=mio:SYN_MIO",
        &mut super::super::tests::fresh(),
        Content {
            industrial_organisations: Some(&database),
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Organisation Workshop BBB Trait Craft");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn unlock_mio_carried_saved_and_variable_operands_share_the_name_path() {
    game::enter();
    let (fixture, _folder) = unlock_mio_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    for operand in ["FROM", "event_target:chosen", "var:syn_carrier"] {
        let mut scopes = super::super::tests::fresh();
        scopes.scopes[0].from = Link::Scope(1);
        scopes
            .scopes
            .push(EventScope::of(EventObject::IndustrialOrganisation(
                UNLOCK_MIO_REFERENCE,
            )));
        scopes.saved_targets.insert(
            1,
            vec![(
                b"chosen".to_vec(),
                EventObject::IndustrialOrganisation(UNLOCK_MIO_REFERENCE),
            )],
        );
        let output = fixture.render_native(
            &format!("fixture_offer_organisation={operand}"),
            &mut scopes,
            Content {
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(
            output.text, "Unlock Workshop BBB AAA\n",
            "{operand}: {:?}",
            output.refusals
        );
        assert!(
            output.refusals.is_empty(),
            "{operand}: {:?}",
            output.refusals
        );
    }
}

#[test]
fn unlock_mio_absent_or_non_mio_targets_keep_literal_fallback() {
    game::enter();
    let (fixture, _folder) = unlock_mio_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    for operand in ["mio:ABSENT", "SYN_MIO_NAME", "THIS"] {
        let output = fixture.render_native(
            &format!("fixture_offer_organisation={operand}"),
            &mut super::super::tests::fresh(),
            Content {
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(output.text, format!("Unlock {operand} AAA\n"));
        assert!(
            output.refusals.is_empty(),
            "{operand}: {:?}",
            output.refusals
        );
    }
}

#[test]
fn unlock_mio_missing_recorded_roster_is_named_and_counted() {
    game::enter();
    let (mut fixture, _folder) = unlock_mio_fixture();
    *fixture.frame.tables[tables::IndustrialOrganisations::ID.index()] = TableView::Unavailable {
        reason: "synthetic MIO roster gap".into(),
    };
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_offer_organisation=mio:SYN_MIO",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "");
    assert_eq!(output.refusals.values().sum::<usize>(), 1);
    assert!(
        output
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains("fixture_offer_organisation")
    );
    assert!(
        output
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains("synthetic MIO roster gap")
    );
}

#[test]
fn unlock_mio_policy_block_refuses_only_unrecorded_roster() {
    game::enter();

    let (fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_offer_policy={policy=SYN_POLICY show_modifiers=yes}",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "");
    assert_eq!(output.refusals.values().sum::<usize>(), 1);
    assert!(
        output
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains("mio policies of country counter inputs")
    );
}

#[test]
fn format99_mio_policy_uses_cached_membership_content_and_modifiers() {
    game::enter();
    let (mut fixture, _, _folder) = event_fixture();
    let names = content_reader_script::key_names::KeyNames::default();
    let mut database = content_reader_military::industrial_organisations::Database::default();
    database.policies.read_file(
        b"SYN_POLICY={name=POLICY_NAME equipment_bonus={synthetic_group={soft_attack=1}} organization_modifier={custom_modifier_tooltip=TIP}}",
        &names,
    ).unwrap();
    fixture.catalogue.localisation = std::sync::Arc::new(
        fixture
            .catalogue
            .localisation
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .chain([
                ("AAA_POLICY_NAME".into(), "Owned [ROOT.GetTag]".into()),
                ("synthetic_group".into(), "Group".into()),
                ("STAT_SYNTHETIC_SOFT_ATTACK".into(), "Soft".into()),
                ("STAT_MODIFIER_DIFF".into(), "+$VALUE$".into()),
            ])
            .collect(),
    );
    set::<tables::LexicalTokens>(
        &mut fixture,
        vec![replay_data::rows::LexicalTokenRow {
            token: 7,
            literal: b"SYN_POLICY".to_vec(),
        }],
    );
    let content = Content {
        industrial_organisations: Some(&database),
        key_names: Some(&names),
        ..Default::default()
    };
    for value in [Recorded::Value(None), Recorded::NotRecorded] {
        set::<tables::CountryCounterInputs>(
            &mut fixture,
            vec![replay_data::rows::CountryCounterInputRow {
                country: 100,
                mio_policies: value,
                ..Default::default()
            }],
        );
        let output = fixture.render_native(
            "fixture_offer_policy={policy=SYN_POLICY show_modifiers=yes}",
            &mut super::super::tests::fresh(),
            content,
        );
        assert!(output.text.is_empty());
        assert_eq!(output.refusals.values().sum::<usize>(), 1);
        assert!(
            output
                .refusals
                .keys()
                .any(|why| why.contains("mio policies of country counter inputs"))
        );
    }
    for (policies, expected) in [
        (Vec::new(), "Policy Tools\n"),
        (
            vec![7],
            "Policy Owned AAA\n (\u{11}YGroup\u{11}! Soft+1.00, detail)",
        ),
    ] {
        set::<tables::CountryCounterInputs>(
            &mut fixture,
            vec![replay_data::rows::CountryCounterInputRow {
                country: 100,
                mio_policies: Recorded::Value(Some(policies)),
                ..Default::default()
            }],
        );
        let output = fixture.render_native(
            "fixture_offer_policy={policy=SYN_POLICY show_modifiers=yes}",
            &mut super::super::tests::fresh(),
            content,
        );
        assert_eq!(output.text, expected);
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    }
    let output = fixture.render_native(
        "fixture_offer_policy={policy=SYN_POLICY show_modifiers=no}",
        &mut super::super::tests::fresh(),
        content,
    );
    assert_eq!(output.text, "Policy Owned AAA\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn event_option_tooltip_matches_raw_name_in_registration_order() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[
        (
            "events/a.txt",
            concat!(
                "add_namespace=synthetic state_event={id=synthetic.1 option={name=Chosen fixture_written_tooltip=SYN_EVENT}} ",
                "country_event={id=synthetic.90 option={name=chosen fixture_written_tooltip=SYN_EVENT} ",
                "option={name=Chosen fixture_written_tooltip=TIP} option={name=Chosen fixture_written_tooltip=SYN_EVENT}} ",
                "country_event={id=synthetic.2 option={name=Chosen fixture_written_tooltip=SYN_EVENT}}"
            ),
        ),
        (
            "events/z.txt",
            "news_event={id=synthetic.0 option={name=Chosen fixture_written_tooltip=SYN_EVENT}}",
        ),
    ]);
    let names = content_reader_script::key_names::KeyNames::default();
    for (name, expected) in [
        ("Chosen", "detail"),
        ("chosen", "News Alpha"),
        ("CHOSEN", ""),
        ("missing", ""),
    ] {
        let output = fixture.render_native(
            &format!("fixture_option_details={name}"),
            &mut super::super::tests::fresh(),
            Content {
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(output.text, expected, "{name}");
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    }
}

#[test]
fn event_option_tooltip_skips_recursive_effects_in_inactive_children() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[(
        "events/synthetic.txt",
        concat!(
            "add_namespace=synthetic country_event={id=synthetic.1 ",
            "option={name=chosen if={limit={always=no} fixture_option_details=other}} ",
            "option={name=chosen if={limit={always=yes} fixture_written_tooltip=SYN_EVENT} else={fixture_option_details=other}} ",
            "option={name=chosen fixture_weighted_choices={0={fixture_option_details=other}}} ",
            "option={name=chosen ROOT={fixture_option_details=other}} ",
            "option={name=chosen fixture_written_tooltip=TIP} ",
            "option={name=recursive hidden_effect={fixture_option_details=other}}}"
        ),
    )]);
    let names = content_reader_script::key_names::KeyNames::default();
    for (name, expected) in [("chosen", "detail"), ("recursive", "")] {
        let output = fixture.render_native(
            &format!("fixture_option_details={name}"),
            &mut super::super::tests::fresh(),
            Content {
                key_names: Some(&names),
                ..Default::default()
            },
        );
        assert_eq!(output.text, expected);
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    }
}

#[test]
fn event_option_tooltip_copies_scope_and_swaps_root_from_without_a_new_session() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[
        (
            "events/synthetic.txt",
            concat!(
                "add_namespace=synthetic country_event={id=synthetic.1 option={name=chosen ",
                "fixture_written_tooltip=SCOPE fixture_local_target=option_target}}"
            ),
        ),
        (
            "localisation/options_l_english.yml",
            "\u{feff}l_english:\n SCOPE:0 \"[THIS.GetTag]/[ROOT.GetTag]/[FROM.GetTag]/[PREV.GetTag]\"\n",
        ),
    ]);
    let names = content_reader_script::key_names::KeyNames::default();
    let mut scopes = EventScopes::new(vec![
        EventScope {
            root: Link::Scope(1),
            from: Link::Itself,
            prev: Link::Itself,
            random_seed: 37,
            random_counter: 91,
            ..EventScope::of(EventObject::Country(1))
        },
        EventScope::of(EventObject::Country(2)),
    ]);
    scopes
        .saved_targets
        .insert(0, vec![(b"prior".to_vec(), EventObject::Country(2))]);
    scopes
        .tooltip_targets
        .insert(0, vec![(b"prior_tip".to_vec(), EventObject::Country(2))]);
    let caller = scopes.scopes[0];
    let items =
        content_reader_script::effect_keys::read_body(b"fixture_option_details=chosen", &names).unwrap();
    fixture.with_renderer(
        &mut scopes,
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
        |renderer| {
            let selected = renderer.evaluator().unwrap().event_scopes(0).unwrap();
            assert_eq!(renderer.group(&items, selected, 0), "AAA/AAA/BBB/AAA");
            assert!(
                renderer.out.refusals.is_empty(),
                "{:?}",
                renderer.out.refusals
            );
        },
    );
    assert_eq!(scopes.scopes[0], caller);
    let copied = &scopes.scopes[2];
    assert_eq!(
        (copied.root, copied.from, copied.prev),
        (Link::Scope(0), Link::Scope(1), Link::Scope(0))
    );
    assert_eq!((copied.random_seed, copied.random_counter), (37, 91));
    assert_eq!(
        scopes.saved_targets[&2],
        vec![(b"prior".to_vec(), EventObject::Country(2))]
    );
    assert_eq!(
        scopes.tooltip_targets[&2],
        vec![(b"prior_tip".to_vec(), EventObject::Country(2))]
    );
    assert!(
        scopes.tooltip_targets[&1]
            .iter()
            .any(|(name, object)| name == b"option_target" && *object == EventObject::Country(1))
    );
}

#[test]
fn event_option_tooltip_trims_only_ascii_whitespace() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[
        (
            "events/synthetic.txt",
            "add_namespace=synthetic country_event={id=synthetic.1 option={name=chosen fixture_written_tooltip=PAD}}",
        ),
        (
            "localisation/options_l_english.yml",
            "\u{feff}l_english:\n PAD:0 \" \\t\\n\u{a0}detail\u{a0}\\n\\t \"\n",
        ),
    ]);
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_option_details=chosen",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "\u{a0}detail\u{a0}");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn deep_scripted_templates_render_through_tooltips_event_options_and_iterator_probes() {
    use std::fmt::Write as _;
    game::enter();
    let mut templates =
        String::from("synth_root={fixture_pick_state={prioritize={3} synth_step_0=yes}}\n");
    let mut events = String::from("add_namespace=synth country_event={id=synth.1 ");
    for i in 0..4096 {
        let next = format!("synth_step_{}=yes", i + 1);
        let step = match i % 4 {
            _ if i < 2048 => next,
            0 => next,
            1 => format!("fixture_override_text={{{next}}}"),
            2 => {
                write!(events, "option={{name=synth_choice_{i} {next}}} ").unwrap();
                format!("fixture_option_details=synth_choice_{i}")
            }
            _ => format!("if={{limit={{always=yes}} {next}}}"),
        };
        writeln!(templates, "synth_step_{i}={{{step}}}").unwrap();
    }
    templates.push_str("synth_step_4096={fixture_written_tooltip=SYN_FIRST fixture_written_tooltip=SYN_MIDDLE fixture_written_tooltip=SYN_LAST}");
    events.push('}');
    let (fixture, _, _folder) = event_files_fixture(&[
        ("events/synth.txt", &events),
        ("common/scripted_effects/synth.txt", &templates),
        (
            "localisation/synth_l_english.yml",
            "\u{feff}l_english:\n SYN_FIRST:0 \"first [THIS.GetID]\"\n SYN_MIDDLE:0 \"middle\"\n SYN_LAST:0 \"last\"\n",
        ),
    ]);
    let scripts = fixture.catalogue.scripted_content().unwrap();
    let names = scripts.layers.layers.key_names().unwrap();
    let effects =
        content_reader_key_names::scripted_effects::read(&scripts.layers.layers, &names).unwrap();
    let result = fixture.render_native(
        "synth_root=yes",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            scripted_effects: Some(&effects),
            ..Default::default()
        },
    );
    assert!(result.refusals.is_empty(), "{:?}", result.refusals);
    let mut lines = result.text.lines().map(str::trim);
    assert!(lines.next().unwrap().contains(':'));
    assert_eq!(lines.collect::<Vec<_>>(), ["first 3", "middle", "last"]);
}

#[test]
fn event_option_tooltip_does_not_visit_scripted_templates_or_operand_text() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[
        (
            "events/synthetic.txt",
            concat!(
                "add_namespace=synthetic country_event={id=synthetic.1 ",
                "option={name=chosen synthetic_call=yes} ",
                "option={name=operand fixture_written_tooltip=fixture_option_details}}"
            ),
        ),
        (
            "common/scripted_effects/synthetic.txt",
            "synthetic_call={fixture_option_details=missing fixture_written_tooltip=TIP}",
        ),
        (
            "localisation/options_l_english.yml",
            "\u{feff}l_english:\n fixture_option_details:0 \"operand text\"\n",
        ),
    ]);
    let scripted = fixture.catalogue.scripted_content().unwrap();
    let names = scripted.layers.layers.key_names().unwrap();
    let effects = content_reader_key_names::scripted_effects::read(&scripted.layers.layers, &names).unwrap();
    for (name, expected) in [("chosen", "detail"), ("operand", "operand text")] {
        let output = fixture.render_native(
            &format!("fixture_option_details={name}"),
            &mut super::super::tests::fresh(),
            Content {
                key_names: Some(&names),
                scripted_effects: Some(&effects),
                ..Default::default()
            },
        );
        assert_eq!(output.text, expected);
        assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    }
}

#[test]
fn event_option_tooltip_empty_name_matches_the_default_option_name() {
    game::enter();
    let (fixture, _, _folder) = event_files_fixture(&[(
        "events/synthetic.txt",
        "add_namespace=synthetic country_event={id=synthetic.1 option={fixture_written_tooltip=TIP}}",
    )]);
    let names = content_reader_script::key_names::KeyNames::default();
    let output = fixture.render_native(
        "fixture_option_details=\"\"",
        &mut super::super::tests::fresh(),
        Content {
            key_names: Some(&names),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "detail");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn removal_finder_preserves_container_target_alias_and_first_ordinal() {
    let row = |ordinal, container, target, state| DecisionRow {
        country: 100,
        ordinal,
        container,
        token: b"Sample_Decision".to_vec(),
        country_target: target,
        numeric_target: state,
        lifecycle: -8,
        ignore: 1,
        ..Default::default()
    };
    let rows = [
        row(9, 3, 202, 7),
        row(1, 4, 202, 7),
        row(3, 3, 200, 7),
        row(2, 3, 200, 8),
    ];
    let slot = |raw| match raw {
        100 => Some(1),
        200 | 202 => Some(2),
        _ => None,
    };
    assert_eq!(
        matching(replay_frame::frame::Rowset::Decoded(&rows), b"sample_decision", 1, false, Some(2), 7, slot)
            .unwrap()
            .unwrap()
            .ordinal,
        3
    );
    assert_eq!(
        matching(replay_frame::frame::Rowset::Decoded(&rows), b"sample_decision", 1, true, Some(2), 7, slot)
            .unwrap()
            .unwrap()
            .ordinal,
        1
    );
    assert!(
        matching(replay_frame::frame::Rowset::Decoded(&rows), b"sample_decision", 1, false, None, 7, slot)
            .unwrap()
            .is_none()
    );
    assert!(
        matching(replay_frame::frame::Rowset::Decoded(&rows), b"different", 1, false, Some(2), 7, slot)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unknown_target_link_is_not_an_absent_decision() {
    let row = DecisionRow {
        country: 100,
        container: 3,
        token: b"sample_decision".to_vec(),
        country_target: 202,
        ..Default::default()
    };
    let error = matching(replay_frame::frame::Rowset::Decoded(&[row]), b"sample_decision", 1, false, Some(2), 0, |raw| {
        (raw == 100).then_some(1)
    })
    .unwrap_err();
    assert!(error.contains("Decisions target 202"));
}

#[test]
fn decision_seed_distinguishes_unread_absence_and_fixed_random_seed() {
    let mut definition = decision();
    assert!(
        decision_seed(9, &definition, None, 2)
            .unwrap_err()
            .contains("description_seed")
    );
    let absent = decision_seed(9, &definition, Some(None), 2).unwrap();
    assert_eq!(
        absent,
        decision_seed(9, &definition, Some(Some(0)), 2).unwrap()
    );
    assert_eq!(
        decision_seed(9, &definition, Some(Some(13)), 2).unwrap(),
        absent.wrapping_add(13)
    );
    definition.fixed_random_seed = true;
    assert_eq!(decision_seed(9, &definition, None, 2).unwrap(), absent);
    definition.seed_component = i32::MAX;
    assert_eq!(decision_seed(1, &definition, None, 0).unwrap(), 0x8000_0000);
}

#[test]
fn decision_name_fallback_ladder() {
    let keys = [
        "COS_Name_group",
        "COS_Name",
        "ABC_Name_group",
        "ABC_Name",
        "Name_group",
        "Name",
    ];
    for first in 0..keys.len() {
        assert_eq!(
            name_key("Name", "COS", "ABC", "group", |key| keys[first..]
                .contains(&key)),
            keys[first]
        );
    }
    assert_eq!(name_key("Name", "", "ABC", "group", |_| false), "Name");
    assert_eq!(
        name_key("Name", "", "ABC", "group", |key| key == "Name_group"),
        "Name_group"
    );
}

use super::super::tests::Fixture;
use replay_data::{
    rows::ClockRow, rows::DecisionRandomItemRow, rows::PoliticalPartyRow, rows::PoliticsRow,
    rows::Recorded, rows::SimulationFeatureRow, rows::TableSchema,
};
use replay_frame::frame::TableView;

fn set<T: TableSchema>(fixture: &mut Fixture, rows: Vec<T::Row>) {
    *fixture.frame.tables[T::ID.index()] = TableView::Complete(T::wrap(rows));
}

fn decision_fixture(seed: Option<Option<i32>>) -> (Fixture, content_reader_politics::decisions::Database) {
    let mut fixture = Fixture::new();
    *fixture.frame.tables[TableId::DecisionRandomItems.index()] = TableView::Unavailable {
        reason: "random items not recorded".into(),
    };
    set::<tables::Clock>(&mut fixture, vec![ClockRow::default()]);
    set::<tables::Politics>(
        &mut fixture,
        vec![PoliticsRow {
            country: 100,
            ruling_party: Some(1),
            ..Default::default()
        }],
    );
    set::<tables::PoliticalParties>(
        &mut fixture,
        vec![PoliticalPartyRow {
            country: 100,
            id: 1,
            token: b"group".to_vec(),
            ..Default::default()
        }],
    );
    set::<tables::Decisions>(
        &mut fixture,
        vec![DecisionRow {
            country: 100,
            container: 3,
            token: b"SYN_DECISION".to_vec(),
            country_target: 200,
            description_seed: replay_data::rows::Recorded::Value(seed),
            lifecycle: -1,
            ignore: 1,
            ..Default::default()
        }],
    );
    let mut database = content_reader_politics::decisions::Database::default();
    database.entries.insert(
        b"syn_decision".to_vec(),
        Decision {
            id: b"SYN_DECISION".to_vec(),
            name: b"SYN_DECISION".to_vec(),
            seed_component: content_reader_politics::decisions::seed_component(b"SYN_DECISION"),
            has_state_targets: true,
            target_states: [b"4".to_vec(), b"2".to_vec(), b"4".to_vec()].into(),
            ..Default::default()
        },
    );
    (fixture, database)
}

#[test]
fn removal_description_binds_highlighted_target_from_and_stored_execution_count() {
    game::enter();
    let (fixture, database) = decision_fixture(Some(None));
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::Country(1))]);
    let caller = scopes.scopes[0];
    let description = fixture.render(
        "fixture_release_decision_target = { decision = SYN_DECISION target = BBB }",
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(description.text, "Remove Conflict in Two\n");
    assert!(
        description.refusals.is_empty(),
        "{:?}",
        description.refusals
    );
    assert_eq!(scopes.scopes[0], caller);
    let (fixture, database) = decision_fixture(Some(Some(1)));
    let description = fixture.render(
        "fixture_release_decision_target = { decision = SYN_DECISION target = BBB }",
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(description.text, "Remove Conflict in Four\n");
    assert!(
        description.refusals.is_empty(),
        "{:?}",
        description.refusals
    );
    assert_eq!(scopes.scopes[0], caller);
}

#[test]
fn removal_refuses_unread_seed_but_fixed_seed_bypasses_it_and_missing_instance_is_empty() {
    game::enter();
    let (mut fixture, mut database) = decision_fixture(None);
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::Country(1))]);
    let body = "fixture_release_decision_target = { decision = SYN_DECISION target = BBB }";
    let description = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert!(description.text.is_empty());
    assert_eq!(description.refusals.values().sum::<usize>(), 1);
    assert!(
        description
            .refusals
            .keys()
            .next()
            .unwrap()
            .contains("description_seed")
    );
    if let TableView::Complete(replay_data::rows::TableRows::Decisions(rows)) =
        &mut *fixture.frame.tables[replay_data::rows::TableId::Decisions.index()]
    {
        rows[0].description_seed = replay_data::rows::Recorded::NotRecorded;
    }
    let historical = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert!(historical.text.is_empty());
    assert_eq!(historical.refusals.values().sum::<usize>(), 1);
    assert!(
        historical
            .refusals
            .keys()
            .any(|why| why.contains("description seed of decisions not recorded"))
    );
    database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap()
        .fixed_random_seed = true;
    assert_eq!(
        fixture
            .render(
                body,
                &mut scopes,
                Content {
                    decisions: Some(&database),
                    ..Default::default()
                }
            )
            .text,
        "Remove Conflict in Two\n"
    );
    set::<tables::Decisions>(&mut fixture, vec![]);
    let absent = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert!(absent.text.is_empty() && absent.refusals.is_empty());
    *fixture.frame.tables[replay_data::rows::TableId::Decisions.index()] = TableView::Unavailable {
        reason: "synthetic fault".into(),
    };
    let unavailable = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(unavailable.refusals.values().sum::<usize>(), 1);
}

#[test]
fn highlight_filters_use_fresh_country_from_state_and_all_state_fallback() {
    game::enter();
    let (fixture, mut database) = decision_fixture(Some(None));
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.target_trigger = content_reader_parser::parser::parse(b"FROM = { state = 2 }")
        .unwrap()
        .items;
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::Country(1))]);
    let body = "fixture_release_decision_target = { decision = SYN_DECISION target = BBB }";
    let output = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Remove Conflict in Two\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.target_root_trigger = content_reader_parser::parser::parse(b"tag = CCC").unwrap().items;
    definition.highlight_conditions = content_reader_parser::parser::parse(b"state = 5").unwrap().items;
    let output = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Remove Conflict in Five\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn state_target_wins_before_authored_highlight_candidates() {
    game::enter();
    let (mut fixture, database) = decision_fixture(Some(None));
    set::<tables::Decisions>(
        &mut fixture,
        vec![DecisionRow {
            country: 100,
            container: 3,
            token: b"SYN_DECISION".to_vec(),
            numeric_target: 3,
            description_seed: replay_data::rows::Recorded::Value(Some(None)),
            ..Default::default()
        }],
    );
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::Country(1))]);
    let output = fixture.render(
        "fixture_release_decision_target = { decision = SYN_DECISION target = 3 }",
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Remove Conflict in Three\n");
    assert!(output.refusals.is_empty(), "{:?}", output.refusals);
}

#[test]
fn inactive_decision_skips_unsupported_title_and_empty_trigger_does_not_enable_highlight() {
    game::enter();
    let (mut fixture, mut database) = decision_fixture(Some(None));
    database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap()
        .refused = Some("unsupported synthetic highlight".into());
    set::<tables::Decisions>(&mut fixture, vec![]);
    let mut scopes = EventScopes::new(vec![EventScope::of(EventObject::Country(1))]);
    let body = "fixture_release_decision_target = { decision = SYN_DECISION target = BBB }";
    let output = fixture.render(
        body,
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert!(output.text.is_empty() && output.refusals.is_empty());
    let (mut fixture, mut database) = decision_fixture(Some(None));
    let definition = database
        .entries
        .get_mut(b"syn_decision".as_slice())
        .unwrap();
    definition.has_state_targets = false;
    definition.target_states.clear();
    set::<tables::Decisions>(
        &mut fixture,
        vec![DecisionRow {
            country: 100,
            container: 3,
            token: b"SYN_DECISION".to_vec(),
            numeric_target: 3,
            description_seed: replay_data::rows::Recorded::Value(Some(None)),
            ..Default::default()
        }],
    );
    let output = fixture.render(
        "fixture_release_decision_target = { decision = SYN_DECISION target = 3 }",
        &mut scopes,
        Content {
            decisions: Some(&database),
            ..Default::default()
        },
    );
    assert_eq!(output.text, "Remove Conflict in Three\n");
    assert!(output.refusals.is_empty());
    assert!(
        scopes
            .scopes
            .last()
            .is_some_and(|scope| scope.this == EventObject::State(3) && scope.from == Link::Itself)
    );
}
