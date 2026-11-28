use super::{Ground, RetirementFlag, Settling, Units, Withheld};
use viewer_layer_core::{layers::{Layer, Upstream, View}, test_support::{rail_recording::recorded as recorded_pages, recorded::{self, column, frames_until, through}}, vision::{self as sight, Vision as Sight}, playhead_host::{MapSide, open, units}};
use playhead::{Settings, Timeline};
use replay_columnar::{manifest::Status, state::Cell, store::Store};
use replay_data::rows::TableId;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};
use viewer_gpu::camera::{MapDefines, MapSize, Placement};
use viewer_unit_reads::counter_move::RouteWorld;
use viewer_foundation::perspective::{Choice, VISION_TABLES, Vision};
use viewer_models::entity_animation::DEFEND;
use viewer_unit_models::unit_models::Unplaced;

struct Flat {
    bounds: BTreeMap<u32, [f32; 4]>,
}

fn flat() -> Arc<Flat> {
    let bounds = [10, 20, 30, 2000].into_iter().map(|p| (p, [p as f32 * 100.0, 1000.0, p as f32 * 100.0 + 10.0, 1010.0])).collect();
    Arc::new(Flat { bounds })
}

impl Ground for Flat {
    fn anchor(&self, province: u32, slot: u32) -> Option<[f32; 3]> {
        (province < 1000).then_some([province as f32, slot as f32, 0.0])
    }

    fn world(&self) -> Option<RouteWorld<'_>> {
        None
    }

    fn transport_scale(&self) -> f32 {
        0.5
    }

    fn bounds(&self) -> &BTreeMap<u32, [f32; 4]> {
        &self.bounds
    }

    fn units_cutoff(&self) -> f32 {
        500.0
    }

    fn ships_cutoff(&self) -> f32 {
        800.0
    }

    fn map(&self) -> (MapSize, MapDefines) {
        (MapSize::TEST, MapDefines::test())
    }

    fn unit_models(&self) -> Option<&viewer_unit_models::unit_models::UnitModelAssets> {
        None
    }
}

const TABLES: [TableId; 6] = [TableId::Divisions, TableId::DivisionMovement, TableId::ArmyIconInputs, TableId::DivisionStatus, TableId::LandCombats, TableId::CountryLinks];
const HOURS: u32 = 320;

fn division(store: &mut Store, slot: u32, id: u64, province: u32, owner: u32) {
    let mut divisions = store.table(TableId::Divisions.index()).unwrap();
    divisions.insert_at(slot, u128::from(id)).unwrap();
    divisions.write(slot, column(TableId::Divisions, "province"), Cell::U32(province)).unwrap();
    divisions.write(slot, column(TableId::Divisions, "logical_owner"), Cell::U32(owner)).unwrap();
    divisions.write(slot, column(TableId::Divisions, "roster_owner"), Cell::U32(owner)).unwrap();
    divisions.write(slot, column(TableId::Divisions, "expeditionary_owner"), Cell::U32(0)).unwrap();
    divisions.write(slot, column(TableId::Divisions, "template"), Cell::U64(900 + id)).unwrap();
    let mut statuses = store.table(TableId::DivisionStatus.index()).unwrap();
    statuses.insert_at(slot, u128::from(id)).unwrap();
    statuses.write(slot, column(TableId::DivisionStatus, "exercising"), Cell::U8(0)).unwrap();
}

fn campaign(at: u32, store: &mut Store) {
    let unrecorded = |table: TableId| match table {
        TableId::Divisions => (180..220).contains(&at),
        TableId::CountryLinks => (240..280).contains(&at),
        _ => false,
    };
    for table in TABLES {
        let status = if unrecorded(table) { Status::NotRecorded } else { Status::Complete };
        store.status(table.index(), status, true, Some(at as i32), true).unwrap();
    }
    let template = column(TableId::Divisions, "template");
    match at {
        0 => {
            let mut links = store.table(TableId::CountryLinks.index()).unwrap();
            for (slot, (key, canonical)) in [(7u32, 3u32), (8, 4)].into_iter().enumerate() {
                links.insert_at(slot as u32, u128::from(key)).unwrap();
                links.write(slot as u32, column(TableId::CountryLinks, "canonical_slot"), Cell::U32(canonical)).unwrap();
            }
            for (slot, (id, province, owner)) in [(101, 10, 7), (102, 10, 7), (103, 10, 7), (104, 10, 8), (105, 20, 7), (106, 2000, 7)].into_iter().enumerate() {
                division(store, slot as u32, id, province, owner);
            }
            let mut movement = store.table(TableId::DivisionMovement.index()).unwrap();
            movement.insert_at(0, 104).unwrap();
            movement.write(0, column(TableId::DivisionMovement, "naval_transfer"), Cell::U8(1)).unwrap();
            movement.write(0, column(TableId::DivisionMovement, "path_total"), Cell::U32(1)).unwrap();
            movement.write(0, column(TableId::DivisionMovement, "path"), Cell::U32List(&[11])).unwrap();
            movement.write(0, column(TableId::DivisionMovement, "target"), Cell::U32(0)).unwrap();
        }
        50 => {
            let mut divisions = store.table(TableId::Divisions.index()).unwrap();
            divisions.write(0, column(TableId::Divisions, "province"), Cell::U32(20)).unwrap();
        }
        100 | 101 => {
            let mut divisions = store.table(TableId::Divisions.index()).unwrap();
            divisions.write(1, template, Cell::U64(if at == 100 { 950 } else { 960 })).unwrap();
        }
        130 => {
            store.table(TableId::Divisions.index()).unwrap().delete(2).unwrap();
            store.table(TableId::DivisionStatus.index()).unwrap().delete(2).unwrap();
            division(store, 6, 107, 30, 7);
        }
        290 => {
            let mut divisions = store.table(TableId::Divisions.index()).unwrap();
            divisions.write(4, column(TableId::Divisions, "province"), Cell::U32(30)).unwrap();
        }
        150 => {
            let mut combats = store.table(TableId::LandCombats.index()).unwrap();
            combats.insert_at(0, 1).unwrap();
            combats.write(0, column(TableId::LandCombats, "attackers"), Cell::U64List(&[105])).unwrap();
            combats.write(0, column(TableId::LandCombats, "defenders"), Cell::U64List(&[101])).unwrap();
        }
        _ => {}
    }
}

fn record(name: &str) -> units::Units {
    recorded::record(name, HOURS, campaign)
}

fn side_seeing(name: &str, seeing: bool) -> MapSide {
    side_at(name, seeing, View::default())
}

fn side_at(name: &str, seeing: bool, view: View) -> MapSide {
    let entry = open(record(name), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Units::new(flat()))], view).unwrap();
    if seeing {
        layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
    }
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn side(name: &str) -> MapSide {
    side_seeing(name, true)
}

fn layer(side: &MapSide) -> &Units {
    side.parts().unwrap().1[0].any().downcast_ref::<Units>().unwrap()
}

fn composed_equal(side: &MapSide) {
    let (mirrors, _) = side.parts().unwrap();
    let mut fresh = layer(side).fresh();
    fresh.compose(mirrors).unwrap();
    assert_eq!(layer(side).differ(&fresh), (0, None));
}

fn oldest(side: &MapSide) -> Vec<((u32, u32, Option<[u32; 2]>), u64)> {
    layer(side).figures().into_iter().map(|(key, figure)| (key, figure.selection.oldest)).collect()
}

#[test]
fn a_division_changed_in_a_step_is_marked_once_and_settled_to_the_composed_value() {
    let mut side = side("units-once");
    assert_eq!(oldest(&side), vec![((10, 3, None), 101), ((10, 4, None), 104), ((20, 3, None), 105), ((2000, 3, None), 106)]);
    assert_eq!(layer(&side).figures()[&(10, 4, None)].selection.transport, true, "a naval transfer selects the transport");
    assert_eq!(layer(&side).figures()[&(2000, 3, None)].placed.as_ref().and_then(|placed| placed.as_ref().err()), Some(&Withheld::Unplaced(Unplaced::Unanchored)));
    composed_equal(&side);
    through(&mut side, 30);
    let before = layer(&side).marked();
    let settles = side.hold_settles();
    through(&mut side, 50);
    assert_eq!(layer(&side).marked()[0] - before[0], 1, "the moved division is marked once");
    assert_eq!(layer(&side).carried().entities, 1, "nothing settles without settling");
    drop(settles);
    through(&mut side, 64);
    assert_eq!(layer(&side).carried().entities, 0);
    assert_eq!(oldest(&side), vec![((10, 3, None), 102), ((10, 4, None), 104), ((20, 3, None), 101), ((2000, 3, None), 106)]);
    composed_equal(&side);
}

#[test]
fn a_division_changed_twice_before_it_settles_shows_its_newest_values() {
    let mut side = side("units-twice");
    through(&mut side, 76);
    let settles = side.hold_settles();
    through(&mut side, 102);
    viewer_layer_core::test_support::recorded::released_unit_by_unit(&mut side, settles);
    through(&mut side, 120);
    assert_eq!(layer(&side).carried().entities, 0);
    assert_eq!(layer(&side).figures()[&(10, 3, None)].selection.template, 960, "the newest template");
    composed_equal(&side);
}

#[test]
fn a_division_deleted_or_inserted_updates_every_stack_that_names_it() {
    let mut side = side("units-churn");
    through(&mut side, 134);
    side.settle_all().unwrap();
    assert_eq!(oldest(&side), vec![((10, 3, None), 102), ((10, 4, None), 104), ((20, 3, None), 101), ((30, 3, None), 107), ((2000, 3, None), 106)]);
    composed_equal(&side);
    through(&mut side, 154);
    side.settle_all().unwrap();
    let figure = layer(&side).figures()[&(20, 3, None)].placed.as_ref().unwrap().as_ref().map(|(target, _)| target.instance.animation).unwrap();
    assert_eq!(figure, DEFEND, "the stack's oldest defends in the combat that started");
    composed_equal(&side);
}

#[test]
fn a_table_not_recorded_is_drawn_as_today_draws_it() {
    let mut side = side("units-gaps");
    through(&mut side, 184);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).refused(), None);
    assert_eq!(layer(&side).stack_count(), 0, "no Divisions rows, no stacks; never the last rows");
    composed_equal(&side);
    through(&mut side, 244);
    side.settle_all().unwrap();
    assert!(layer(&side).refused().is_some_and(|why| why.contains("CountryLinks")), "{:?}", layer(&side).refused());
    assert_eq!(layer(&side).stack_count(), 0);
    through(&mut side, 284);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).refused(), None);
    assert_eq!(layer(&side).stack_count(), 5);
    composed_equal(&side);
}

#[test]
fn a_layer_given_no_vision_is_refused_by_name_and_composes_whole_once_given_one() {
    let mut side = side_seeing("units-vision", false);
    side.settle_all().unwrap();
    assert!(layer(&side).refused().is_some_and(|why| why.contains("units") && why.contains("vision")), "{:?}", layer(&side).refused());
    assert_eq!(layer(&side).stack_count(), 0);
    through(&mut side, 60);
    layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
    side.settle_all().unwrap();
    assert_eq!(layer(&side).refused(), None);
    assert_eq!(oldest(&side), vec![((10, 3, None), 102), ((10, 4, None), 104), ((20, 3, None), 101), ((2000, 3, None), 106)]);
    composed_equal(&side);
}

fn over(province: u32, height: f32) -> View {
    let camera = Placement::map([province as f32 * 100.0 + 5.0, 1005.0], height);
    View { camera: Some(camera), camera_height: height, viewport: [2048, 1152], ..View::default() }
}

#[test]
fn a_stack_that_moved_outside_the_view_gets_its_figure_when_the_camera_admits_it() {
    let mut side = side_at("units-admitted", true, over(10, 100.0));
    let figures = layer(&side).figures();
    assert!(figures[&(10, 3, None)].admitted && figures[&(10, 3, None)].placed.is_some(), "{:?}", figures[&(10, 3, None)]);
    assert!(!figures[&(20, 3, None)].admitted && figures[&(20, 3, None)].placed.is_none(), "an unadmitted stack's figure waits");
    through(&mut side, 64);
    side.settle_all().unwrap();
    assert!(layer(&side).figures()[&(20, 3, None)].placed.is_none(), "it moved while outside: still waiting");
    composed_equal(&side);
    side.set_view(over(20, 100.0)).unwrap();
    through(&mut side, 72);
    side.settle_all().unwrap();
    let figure = layer(&side).figures()[&(20, 3, None)];
    assert!(figure.admitted && matches!(figure.placed, Some(Ok(_))), "{figure:?}");
    assert_eq!(figure.selection.oldest, 101);
    composed_equal(&side);
    side.set_view(View::default()).unwrap();
    through(&mut side, 80);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).admitted(), layer(&side).stack_count(), "no camera admits every stack");
    assert!(layer(&side).figures().values().all(|figure| figure.placed.is_some()));
    composed_equal(&side);
}

#[test]
fn crossing_the_units_cutoff_declares_nothing_new_and_composes_no_reset() {
    let kinds = replay_recording::schema::schema().unwrap();
    let mut far = Units::new(flat());
    let mut near = Units::new(flat());
    let declared = |layer: &mut Units, view: &View| {
        let mut inputs = viewer_layer_core::layers::Inputs::new(&kinds, "units");
        layer.inputs(view, &mut inputs).unwrap();
        inputs.into_tables()
    };
    let above = declared(&mut far, &over(10, 600.0));
    let below = declared(&mut near, &over(10, 100.0));
    assert_eq!(above, below, "the same columns at every height");
    assert!(!far.drawn() && near.drawn(), "no land figure is drawn above the cutoff");
    let mut side = side("units-far");
    through(&mut side, 60);
    side.set_view(over(10, 600.0)).unwrap();
    through(&mut side, 64);
    assert!(side.refusals().next().is_none());
    assert_eq!(layer(&side).stack_count(), 0, "it holds nothing");
    assert_eq!(layer(&side).carried().entities, 0, "and carries nothing");
    side.set_view(View::default()).unwrap();
    let frame = side.frame(side.entry().playhead().newest().unwrap_or(0)).unwrap();
    assert_eq!((frame.resets, side.composing()), (0, false), "crossing the cutoff resets no table");
    side.settle_all().unwrap();
    assert_eq!(layer(&side).stack_count(), 4, "its stacks are keyed again from the mirrors at once (at {:?})", side.applied());
    composed_equal(&side);
}

#[test]
fn a_division_moved_outside_the_view_is_rekeyed_once_its_province_is_admitted() {
    let mut side = side_at("units-deferred", true, over(10, 100.0));
    through(&mut side, 296);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).deferred(), 1, "division 105 moved from 20 to 30, neither admitted");
    assert_eq!(layer(&side).figures()[&(20, 3, None)].selection.oldest, 101, "the stack outside keeps what it had");
    composed_equal(&side);
    side.set_view(over(30, 100.0)).unwrap();
    through(&mut side, 300);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).deferred(), 0);
    assert_eq!(layer(&side).figures()[&(30, 3, None)].selection.oldest, 105, "re-keyed once its province is admitted");
    composed_equal(&side);
    side.set_view(View::default()).unwrap();
    through(&mut side, 304);
    side.settle_all().unwrap();
    composed_equal(&side);
}

const TAKEN: i32 = 13;

const RENAMED: i32 = 25;

fn sight_side(name: &str, enemy: bool, view: View) -> MapSide {
    let (entry, content) = sight_world(name, enemy);
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sight::new(content)), Box::new(Units::reading_vision(flat()))];
    playing(&entry, layers, view)
}

fn sight_world(name: &str, enemy: bool) -> (Arc<viewer_layer_core::playhead_host::Entry>, Arc<sight::Content>) {
    let strings = |at: u32| if at == 0 { vec!["AAA".to_owned(), "BBB".to_owned()] } else { Vec::new() };
    let mut tables: Vec<TableId> = VISION_TABLES.to_vec();
    tables.extend(TABLES.into_iter().filter(|table| !VISION_TABLES.contains(table)));
    let pages = recorded_pages(120, strings, |at, store| {
        for &table in &tables {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        let write = |store: &mut Store, table: TableId, slot: u32, field: &str, cell: Cell<'_>| store.table(table.index()).unwrap().write(slot, column(table, field), cell).unwrap();
        let insert = |store: &mut Store, table: TableId, slot: u32, key: u128| store.table(table.index()).unwrap().insert_at(slot, key).unwrap();
        if at == 0 {
            for (slot, raw) in [101u32, 102].into_iter().enumerate() {
                let slot = slot as u32;
                insert(store, TableId::CountryLinks, slot, u128::from(raw));
                write(store, TableId::CountryLinks, slot, "canonical_slot", Cell::U32(slot + 1));
                insert(store, TableId::Countries, slot, u128::from(raw));
                write(store, TableId::Countries, slot, "canonical_slot", Cell::U32(slot + 1));
                write(store, TableId::Countries, slot, "tag", Cell::Bytes(slot));
            }
            insert(store, TableId::LocalPlayer, 0, 0);
            write(store, TableId::LocalPlayer, 0, "country", Cell::U32(101));
            for (slot, (province, state, controller)) in [(1u32, 100u32, 101u32), (2, 100, 101), (3, 200, 102), (4, 200, 102), (9, 0, 0)].into_iter().enumerate() {
                insert(store, TableId::Provinces, slot as u32, u128::from(province));
                write(store, TableId::Provinces, slot as u32, "state", Cell::U32(state));
                write(store, TableId::Provinces, slot as u32, "controller", Cell::U32(controller));
            }
            insert(store, TableId::WarRelations, 0, 101 << 32 | 102);
            let mut placed = vec![(901, 1, 101), (902, 2, 101)];
            if enemy {
                placed.push((905, 4, 102));
            }
            for (slot, (id, province, owner)) in placed.into_iter().enumerate() {
                division(store, slot as u32, id, province, owner);
                write(store, TableId::Divisions, slot as u32, "reveal_skip", Cell::U8(0));
            }
        }
        if at as i32 == TAKEN {
            write(store, TableId::Provinces, 3, "controller", Cell::U32(101));
        }
        if at as i32 == RENAMED {
            write(store, TableId::Countries, 1, "tag", Cell::Bytes(0));
        }
    });
    let content = {
        use viewer_foundation::visibility::{FogContent, Topology, test_defines};
        let neighbors = [(1, vec![2]), (2, vec![1, 3]), (3, vec![2, 4]), (4, vec![3, 9]), (9, vec![4])].into_iter().collect();
        Arc::new(sight::Content::new(Ok(FogContent { topology: Topology { land: (1..=4).collect(), sea: BTreeSet::from([9]), neighbors }, defines: test_defines() })))
    };
    let timeline = Timeline::whole(&*pages).unwrap();
    let recording = units::Units { name: name.to_owned(), pages, timeline, kinds: Arc::new(replay_recording::schema::schema().unwrap()) };
    let entry = open(recording, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    (entry, content)
}

fn playing(entry: &Arc<viewer_layer_core::playhead_host::Entry>, layers: Vec<Box<dyn Layer>>, view: View) -> MapSide {
    let mut side = MapSide::new(Arc::clone(entry), layers, view).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn reading(side: &MapSide) -> &Units {
    side.parts().unwrap().1[1].any().downcast_ref::<Units>().unwrap()
}

fn sight_of(side: &MapSide) -> &Sight {
    side.parts().unwrap().1[0].any().downcast_ref::<Sight>().unwrap()
}

fn settled(side: &mut MapSide) {
    for _ in 0..8 {
        side.settle_all().unwrap();
        if side.parts().unwrap().1.iter().all(|layer| layer.carried().entities == 0) {
            return;
        }
    }
    panic!("eight whole settles left work carried");
}

fn reading_composed_equal(side: &MapSide) {
    let (mirrors, layers) = side.parts().unwrap();
    let mut fresh = reading(side).fresh();
    fresh.compose_with(mirrors, &Upstream::new(&layers[..1])).unwrap();
    assert_eq!(reading(side).differ(&fresh), (0, None));
}

fn held_through(side: &mut MapSide, hour: i32) {
    for _ in 0..50_000_000u64 {
        viewer_layer_core::test_support::recorded::reads_landed(side);
        let head = side.entry().playhead().newest().unwrap_or(0).min(hour + 3);
        let frame = side.frame(head).unwrap();
        if frame.applied.is_some_and(|applied| applied >= hour) {
            return;
        }
        if frame.steps == 0 && frame.swapped.is_none() {
            std::thread::yield_now();
        }
    }
    panic!("the map side never reached hour {hour}");
}

fn unseen(side: &MapSide, key: (u32, u32, Option<[u32; 2]>)) -> bool {
    matches!(reading(side).figures()[&key].placed, Some(Err(Withheld::Unseen)))
}

#[test]
fn a_change_of_one_provinces_sight_marks_the_divisions_that_read_it_and_no_others() {
    let mut side = sight_side("units-sight-one", true, View::default());
    held_through(&mut side, TAKEN - 4);
    settled(&mut side);
    assert_eq!(reading(&side).refused(), None);
    assert_eq!(reading(&side).stack_count(), 3);
    assert!(unseen(&side, (4, 2, None)), "the enemy division in province 4 is unseen before it is taken");
    assert!(!unseen(&side, (1, 1, None)) && !unseen(&side, (2, 1, None)));
    reading_composed_equal(&side);
    let (stacks, sights) = (reading(&side).marked()[1], reading(&side).sighted());
    through(&mut side, TAKEN + 4);
    settled(&mut side);
    assert_eq!(sight_of(&side).province_byte(4), Some(255), "province 4 is the viewer's");
    assert!(!unseen(&side, (4, 2, None)), "seen once its province is taken");
    assert_eq!(reading(&side).marked()[1] - stacks, 1, "only the stack whose sight changed is placed again");
    assert!(reading(&side).sighted() > sights, "division 905's sight is read again");
    reading_composed_equal(&side);
}

#[test]
fn a_vision_change_that_moves_no_divisions_sight_leaves_the_layer_carrying_nothing() {
    let mut side = sight_side("units-sight-none", false, View::default());
    held_through(&mut side, TAKEN - 4);
    settled(&mut side);
    let marked = reading(&side).marked();
    frames_until(&mut side, |side, frame| sight_of(side).province_byte(4) == Some(255) && frame.applied.is_some_and(|hour| hour >= TAKEN + 4));
    settled(&mut side);
    assert_eq!(reading(&side).marked(), marked, "nothing marked");
    reading_composed_equal(&side);
}

#[test]
fn a_whole_vision_publish_with_the_sight_key_unchanged_reads_every_divisions_sight_again() {
    let mut side = sight_side("units-sight-whole", true, View::default());
    held_through(&mut side, RENAMED - 4);
    settled(&mut side);
    assert_eq!(reading(&side).refused(), None);
    assert_eq!(reading(&side).stack_count(), 3);
    let (epoch, sights) = (sight_of(&side).sight_epoch(), reading(&side).sighted());
    through(&mut side, RENAMED + 4);
    settled(&mut side);
    assert_eq!(sight_of(&side).sight_epoch(), epoch, "no part of the sight key moved");
    assert!(reading(&side).sighted() - sights >= 3, "every division a stack holds reads its sight again, not {}", reading(&side).sighted() - sights);
    reading_composed_equal(&side);
}

#[test]
fn a_reading_layer_at_a_view_that_draws_no_unit_model_holds_and_carries_nothing() {
    let mut side = sight_side("units-sight-far", true, View::default());
    through(&mut side, 4);
    settled(&mut side);
    assert_eq!(reading(&side).stack_count(), 3);
    side.set_view(over(10, 600.0)).unwrap();
    through(&mut side, TAKEN + 4);
    assert!(side.draws("vision") && !side.composing(), "its columns are declared at every height: nothing composes");
    settled(&mut side);
    assert_eq!(sight_of(&side).province_byte(4), Some(255));
    assert_eq!(reading(&side).stack_count(), 0);
    assert_eq!(reading(&side).carried().entities, 0);
    assert_eq!(reading(&side).held(), 0, "it holds nothing");
    assert_eq!(reading(&side).perspective(), None);
    side.set_view(View::default()).unwrap();
    let frame = side.frame(side.entry().playhead().newest().unwrap_or(0)).unwrap();
    assert_eq!((frame.resets, side.composing()), (0, false), "crossing the cutoff resets no table");
    settled(&mut side);
    assert_eq!(reading(&side).stack_count(), 3, "shown again, its stacks are keyed from the mirrors at once");
    assert!(!unseen(&side, (4, 2, None)), "with the sight that changed while it was hidden");
    reading_composed_equal(&side);
}

#[test]
fn steps_mark_their_divisions_reading_no_row_and_a_division_changed_in_several_is_placed_once() {
    let entry = open(
        recorded::record("units-apply-marks", HOURS, |at, store| {
            campaign(at, store);
            let province = column(TableId::Divisions, "province");
            match at {
                54 => store.table(TableId::Divisions.index()).unwrap().write(0, province, Cell::U32(30)).unwrap(),
                58 => {
                    store.table(TableId::Divisions.index()).unwrap().write(0, province, Cell::U32(20)).unwrap();
                    store.table(TableId::DivisionStatus.index()).unwrap().write(0, column(TableId::DivisionStatus, "exercising"), Cell::U8(1)).unwrap();
                }
                _ => {}
            }
        }),
        Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() },
    )
    .unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Units::new(flat()))], View::default()).unwrap();
    layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    through(&mut side, 30);
    side.settle_all().unwrap();
    let placed = layer(&side).placed();
    let settles = side.hold_settles();
    through(&mut side, 58);
    assert_eq!(layer(&side).applied_reads(), 0, "apply marks; it reads no row");
    assert_eq!(layer(&side).placed(), placed, "nothing is placed while the settles are held");
    viewer_layer_core::test_support::recorded::released_unit_by_unit(&mut side, settles);
    assert_eq!(layer(&side).placed() - placed, 1, "division 101, changed in three steps, is placed once");
    assert_eq!(oldest(&side), vec![((10, 3, None), 102), ((10, 4, None), 104), ((20, 3, None), 101), ((2000, 3, None), 106)]);
    let figure = layer(&side).figures()[&(20, 3, None)].placed.clone();
    assert!(matches!(figure, Some(Ok(_))), "{figure:?}");
    composed_equal(&side);
}

#[derive(Default)]
struct HeldModels {
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl HeldModels {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

impl super::models::Select for HeldModels {
    fn choose_meshes(&self, _frame: &replay_frame::frame::DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), _stacks: &std::collections::HashMap<u64, super::meshes::Asked>, _asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> std::io::Result<super::meshes::Chosen> {
        Ok(super::meshes::Chosen { meshes: Vec::new(), made: viewer_unit_models::unit_models::Made::default() })
    }

    fn prepare(&self, _frame: &replay_frame::frame::DecodedFrame, _perspective: &Choice, _convoys: Option<&[viewer_unit_models::trade_routes::ConvoyLine]>, _naval: bool, _kept: &mut super::models::Kept) -> std::io::Result<viewer_unit_models::unit_models::PreparedModels> {
        let until = std::time::Instant::now() + Duration::from_secs(60);
        let mut open = self.open.lock().unwrap();
        while !*open {
            let Some(left) = until.checked_duration_since(std::time::Instant::now()) else { break };
            open = self.changed.wait_timeout(open, left).unwrap().0;
        }
        Err(std::io::Error::other("no unit models in this test"))
    }
}

fn window_state(side: &MapSide, at: usize) -> String {
    let units = side.parts().map(|(_, layers)| layers[at].any().downcast_ref::<Units>().unwrap()).map(|units| {
        let models = units.models.as_ref().map(|models| (models.in_flight(), models.refused.clone()));
        let meshes = units.meshes.as_ref().map(|meshes| (meshes.hour, meshes.refused.clone()));
        format!("carried {:?}, published {:?}, models out and refused {models:?}, meshes at and refused {meshes:?}", units.carried(), units.published())
    });
    format!("applied {:?}, newest {:?}, composing {}, on screen {:?}; the units: {units:?}; the playhead's {:?}", side.applied(), side.entry().playhead().newest(), side.composing(), side.on_screen().map(|on| on.drawn), side.entry().playhead().held().2)
}

fn frames_at(side: &mut MapSide, head: Option<i32>, at: usize, what: &str, mut done: impl FnMut(&MapSide) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let hour = head.or(side.applied()).unwrap_or(0);
        let frame = side.frame(hour).unwrap();
        if done(side) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what} not within 120 s with the head at {hour}: {}", window_state(side, at));
        if frame.steps == 0 && frame.swapped.is_none() {
            std::thread::yield_now();
        }
    }
}

#[test]
fn a_models_job_out_is_not_carried_and_the_layer_publishes_nothing_before_its_first_models() {
    use viewer_layer_core::layers::Published;
    let (entry, content) = sight_world("units-models-out", false);
    let held = Arc::new(HeldModels::default());
    let mut units = Units::new(flat()).in_window(Arc::clone(&held) as Arc<dyn super::models::Select>);
    units.window.as_mut().expect("in the window").tables = vec![TableId::Divisions];
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sight::new(content)), Box::new(units)];
    let mut side = playing(&entry, layers, View::default());
    let out = |side: &MapSide| side.parts().is_some() && reading(side).models.as_ref().is_some_and(|models| models.in_flight() > 0);
    frames_at(&mut side, Some(4), 1, "the models job going out with nothing carried", |side| out(side) && reading(side).carried().entities == 0);
    assert_eq!(reading(&side).published(), Published::Nothing, "no models made yet");
    let sent_at = side.applied().expect("composed");
    let next = sent_at + 4;
    assert!(next < 120, "the recording holds hour {next}");
    frames_at(&mut side, Some(next), 1, &format!("hour {next} applied and settled with the job held"), |side| !side.composing() && side.applied() >= Some(next) && reading(side).carried().entities == 0);
    assert!(out(&side), "the job is still held while steps are applied and settled: {}", window_state(&side, 1));
    let held_at = side.applied().expect("steps applied while the job was held");
    assert!(side.on_screen().and_then(|on| on.drawn).is_none_or(|drawn| drawn < held_at), "the map claims no hour whose first models are not made");
    held.open();
    frames_at(&mut side, None, 1, "the held job answered and refused", |side| side.parts().is_some() && !out(side) && reading(side).models.as_ref().is_some_and(|models| models.refused.is_some()));
    let refused = reading(&side).models.as_ref().and_then(|models| models.refused.clone());
    assert!(refused.as_deref().is_some_and(|why| why.contains("no unit models in this test")), "{refused:?}");
    frames_at(&mut side, None, 1, "the layer current, answered and refused, once its meshes are made", |side| side.parts().is_some() && reading(side).published() == Published::Current);
    frames_at(&mut side, None, 1, &format!("the map's drawn hour at {held_at}"), |side| side.on_screen().and_then(|on| on.drawn).is_some_and(|drawn| drawn >= held_at));
}

#[test]
fn the_window_moves_a_moved_figure_from_its_own_placing_with_no_models_answered() {
    let entry = open(record("units-movement"), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let held = Arc::new(HeldModels::default());
    let mut units = Units::new(flat()).in_window(Arc::clone(&held) as Arc<dyn super::models::Select>);
    units.reading = false;
    units.set_vision(Choice::default(), Arc::new(Vision::default()));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(units)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_at(&mut side, None, 0, "the seek's compose swapped in", |side| side.applied().is_some() && !side.composing());
    let out = |side: &MapSide| side.parts().is_some() && layer(side).models.as_ref().is_some_and(|models| models.in_flight() > 0);
    let moved = 60;
    assert!(side.applied().is_some_and(|applied| applied < moved), "composed past hour {moved}: {}", window_state(&side, 0));
    frames_at(&mut side, Some(moved), 0, &format!("hour {moved} applied and settled with the models job out"), |side| !side.composing() && side.applied() == Some(moved) && out(side) && layer(side).carried().entities == 0);
    let units = layer(&side);
    assert!(units.prepared().is_none(), "the models job is held: no models answered");
    let figure = units.figures()[&(20, 3, None)];
    assert_eq!(figure.selection.oldest, 101);
    let Some(Ok((target, position))) = &figure.placed else { panic!("the moved stack is placed: {:?}", figure.placed) };
    assert_eq!(target.province, 20);
    let moved: Vec<_> = units.movement_positions().filter(|(unit, ..)| *unit == viewer_runtime::unit_handle::Unit::Division(101)).collect();
    assert_eq!(moved.len(), 1, "the moved figure is handed once");
    assert!(std::ptr::eq(moved[0].1, position), "its position is its figure's at the applied hour");
    assert_eq!(moved[0].2, target.instance.rotation, "a figure standing faces as placed");
    held.open();
}

#[derive(Default)]
struct CountedChoices {
    asked: std::sync::Mutex<Vec<u64>>,
}

impl super::models::Select for CountedChoices {
    fn choose_meshes(&self, _frame: &replay_frame::frame::DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), stacks: &std::collections::HashMap<u64, super::meshes::Asked>, asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> std::io::Result<super::meshes::Chosen> {
        let mut meshes: Vec<(u64, super::meshes::Mesh)> = stacks.keys().filter(|oldest| asked.contains(oldest)).map(|&oldest| (oldest, Ok(Some(oldest as usize)))).collect();
        meshes.sort_unstable_by_key(|&(oldest, _)| oldest);
        self.asked.lock().unwrap().extend(meshes.iter().map(|&(oldest, _)| oldest));
        let made = viewer_unit_models::unit_models::Made { selections: meshes.len(), ..Default::default() };
        Ok(super::meshes::Chosen { meshes, made })
    }

    fn prepare(&self, _frame: &replay_frame::frame::DecodedFrame, _perspective: &Choice, _convoys: Option<&[viewer_unit_models::trade_routes::ConvoyLine]>, _naval: bool, _kept: &mut super::models::Kept) -> std::io::Result<viewer_unit_models::unit_models::PreparedModels> {
        Err(std::io::Error::other("no unit models in this test"))
    }
}

#[test]
fn a_step_that_changes_one_division_chooses_that_stacks_mesh_and_no_other() {
    let entry = open(record("units-meshes"), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let choices = Arc::new(CountedChoices::default());
    let mut units = Units::new(flat()).in_window(Arc::clone(&choices) as Arc<dyn super::models::Select>);
    units.reading = false;
    units.set_vision(Choice::default(), Arc::new(Vision::default()));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(units)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    through(&mut side, 64);
    side.settle_all().unwrap();
    let meshes = |side: &MapSide| -> BTreeMap<u64, usize> { layer(side).meshes().expect("in the window").iter().map(|(&oldest, &mesh)| (oldest, mesh)).collect() };
    assert_eq!(meshes(&side), BTreeMap::from([(101, 101), (102, 102), (104, 104)]), "{}", layer(&side).mesh_state());
    let before = choices.asked.lock().unwrap().len();
    through(&mut side, 104);
    side.settle_all().unwrap();
    let since: BTreeSet<u64> = choices.asked.lock().unwrap()[before..].iter().copied().collect();
    assert_eq!(since, BTreeSet::from([102]), "only the stack whose division's template changed is chosen again");
    assert_eq!(meshes(&side), BTreeMap::from([(101, 101), (102, 102), (104, 104)]));
    assert_eq!(layer(&side).figures()[&(10, 3, None)].selection.template, 960);
}

#[test]
fn a_figure_waiting_to_be_placed_again_is_drawn_where_it_was_until_it_is_placed() {
    let choices = Arc::new(CountedChoices::default());
    let mut side = meshes_side(record("units-meshes-waiting-drawn"), &choices);
    let units = side.layers_mut()[0].any_mut().and_then(|layer| layer.downcast_mut::<Units>()).expect("the units layer");
    let stack = units.asking.get(&101).map(|&(stack, _)| stack).expect("101's stack asks a mesh");
    let drawn = |units: &Units| -> Vec<(u32, [f32; 3])> {
        let meshes = &units.meshes.as_ref().expect("in the window").meshes;
        units.land(meshes).iter().filter(|target| target.identity == 101).map(|target| (target.province, target.instance.position)).collect()
    };
    let moving = |units: &Units| units.movement_positions().filter(|(unit, ..)| *unit == viewer_runtime::unit_handle::Unit::Division(101)).count();
    let before = drawn(&units);
    assert_eq!(before.len(), 1, "101 drawn once");
    let held = units.stacks[stack as usize].figure.take().expect("101's figure");
    let mut placing = held.placing.clone();
    placing.province += 1;
    let looks = (held.selection, held.admitted, false);
    let pending = units.refigured(stack, Some(held), looks, Some(placing));
    assert!(pending.placed.is_none(), "it waits to be placed again");
    units.stacks[stack as usize].figure = Some(pending);
    for frame in 0..3 {
        assert_eq!(drawn(&units), before, "frame {frame} while it waits: drawn where it was");
        assert_eq!(moving(&units), 1, "frame {frame} while it waits: its animator still reads it");
    }
    units.place_figure(stack);
    let after = drawn(&units);
    assert_eq!(after.len(), 1, "drawn at its new placing the frame it is made");
    assert_ne!(after, before, "and that placing is the new one");
    assert_eq!(moving(&units), 1);
}

fn meshes_side(recording: units::Units, choices: &Arc<CountedChoices>) -> MapSide {
    let entry = open(recording, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut units = Units::new(flat()).in_window(Arc::clone(choices) as Arc<dyn super::models::Select>);
    units.reading = false;
    units.set_vision(Choice::default(), Arc::new(Vision::default()));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(units)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    through(&mut side, 64);
    side.settle_all().unwrap();
    side
}

#[test]
fn a_mesh_build_waits_for_a_table_the_meshes_read_to_be_made_anew() {
    let recording = recorded::record("units-meshes-idle", HOURS, |at, store| {
        campaign(at, store);
        if (60..=98).contains(&at) {
            let mut combats = store.table(TableId::LandCombats.index()).unwrap();
            if at == 60 {
                combats.insert_at(7, 2).unwrap();
            }
            let attackers: &[u64] = if at % 2 == 0 { &[105] } else { &[] };
            combats.write(7, column(TableId::LandCombats, "attackers"), Cell::U64List(attackers)).unwrap();
        }
    });
    let choices = Arc::new(CountedChoices::default());
    let mut side = meshes_side(recording, &choices);
    assert_eq!(layer(&side).meshes().map(HashMap::len), Some(3), "the three placed figures have meshes: {}", layer(&side).mesh_state());
    let before = layer(&side).take_mesh_counts().expect("in the window");
    assert!(before.builds > 0 && before.products > 0, "{before:?}");
    let asking = |side: &MapSide| layer(side).asking.iter().map(|(&oldest, &(stack, asked))| (oldest, (stack, asked))).collect::<std::collections::BTreeMap<_, _>>();
    let asked_before = asking(&side);
    held_through(&mut side, 96);
    side.settle_all().unwrap();
    let counts = layer(&side).take_mesh_counts().expect("in the window");
    assert_eq!((counts.builds, counts.products), (0, 0), "no table the meshes read was made anew and no stack's place or selection moved: {counts:?}; asked by oldest member (stack, (province, key)) before {asked_before:?}, after {:?}", asking(&side));
    assert!(layer(&side).mesh_hour().is_some_and(|hour| hour >= 96), "the meshes stand for the newer rows: {:?}", layer(&side).mesh_hour());
    assert_eq!(layer(&side).meshes().map(HashMap::len), Some(3));
}

#[test]
fn a_mesh_build_decides_the_limits_again_when_only_the_hour_moves() {
    let recording = recorded::record("units-meshes-dated", HOURS, |at, store| {
        campaign(at, store);
        store.status(TableId::Clock.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        let mut clock = store.table(TableId::Clock.index()).unwrap();
        if at == 0 {
            clock.insert_at(0, 0).unwrap();
        }
        clock.write(0, column(TableId::Clock, "hour"), Cell::I32(at as i32)).unwrap();
    });
    let choices = Arc::new(CountedChoices::default());
    let mut side = meshes_side(recording, &choices);
    let _ = layer(&side).take_mesh_counts();
    held_through(&mut side, 96);
    side.settle_all().unwrap();
    let counts = layer(&side).take_mesh_counts().expect("in the window");
    assert!(counts.builds > 0 && counts.products > 0, "the hour moved: a session decided the limits again: {counts:?}");
    assert!(layer(&side).mesh_hour().is_some_and(|hour| hour >= 96), "the meshes stand for the newer rows: {:?}", layer(&side).mesh_hour());
}

#[test]
fn a_step_applied_every_frame_starts_a_mesh_build_once_the_one_before_answers_and_waits_for_no_clock() {
    let recording = recorded::record("units-meshes-cadence", HOURS, |at, store| {
        campaign(at, store);
        if (66..=160).contains(&at) {
            store.table(TableId::Divisions.index()).unwrap().write(1, column(TableId::Divisions, "template"), Cell::U64(900 + u64::from(at))).unwrap();
        }
    });
    let choices = Arc::new(CountedChoices::default());
    let mut side = meshes_side(recording, &choices);
    let _ = layer(&side).take_mesh_counts();
    for hour in [80, 100, 120, 140] {
        held_through(&mut side, hour);
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while !layer(&side).mesh_hour().is_some_and(|meshes| meshes >= hour) {
            side.frame(side.applied().expect("applied")).unwrap();
            assert!(std::time::Instant::now() < until, "the meshes never reached hour {hour}: {:?}", layer(&side).mesh_hour());
            std::thread::yield_now();
        }
    }
    let counts = layer(&side).take_mesh_counts().expect("in the window");
    assert!(counts.builds >= 4 && counts.products >= 4, "builds followed the steps: {counts:?}");
    held_through(&mut side, 160);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).figures()[&(10, 3, None)].selection.template, 1060);
    assert_eq!(layer(&side).meshes().and_then(|meshes| meshes.get(&102).copied()), Some(102), "the newest stack's mesh, settled whole");
}

struct NoModels;

impl super::models::Select for NoModels {
    fn choose_meshes(&self, _frame: &replay_frame::frame::DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), _stacks: &std::collections::HashMap<u64, super::meshes::Asked>, _asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> std::io::Result<super::meshes::Chosen> {
        Ok(super::meshes::Chosen { meshes: Vec::new(), made: viewer_unit_models::unit_models::Made::default() })
    }

    fn prepare(&self, _frame: &replay_frame::frame::DecodedFrame, _perspective: &Choice, _convoys: Option<&[viewer_unit_models::trade_routes::ConvoyLine]>, _naval: bool, _kept: &mut super::models::Kept) -> std::io::Result<viewer_unit_models::unit_models::PreparedModels> {
        Ok(viewer_unit_models::unit_models::PreparedModels::drawn_by_layers())
    }
}

#[test]
fn a_view_above_the_land_cutoff_and_below_the_ships_draws_the_ships_and_no_land_figure() {
    let kinds = replay_recording::schema::schema().unwrap();
    let (entry, content) = sight_world("units-ships-only", false);
    let mut alone = Units::new(flat()).in_window(Arc::new(NoModels) as Arc<dyn super::models::Select>);
    let mut inputs = viewer_layer_core::layers::Inputs::new(&kinds, "units");
    alone.inputs(&over(10, 600.0), &mut inputs).unwrap();
    let mut land = Units::new(flat()).in_window(Arc::new(NoModels) as Arc<dyn super::models::Select>);
    let mut drawn = viewer_layer_core::layers::Inputs::new(&kinds, "units");
    land.inputs(&over(10, 100.0), &mut drawn).unwrap();
    assert_eq!(inputs.selected(), drawn.selected(), "the land tables are declared at every height: crossing the cutoff resets none");
    drop((land, alone));

    let mut units = Units::new(flat()).in_window(Arc::new(NoModels) as Arc<dyn super::models::Select>);
    units.window.as_mut().expect("in the window").tables = vec![TableId::Divisions];
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sight::new(content)), Box::new(units)];
    let mut side = playing(&entry, layers, over(10, 600.0));
    frames_at(&mut side, Some(8), 1, "the models made at a view that draws ships and no land figure", |side| {
        side.parts().is_some() && !side.composing() && reading(side).models.as_ref().is_some_and(|models| models.made_at(8))
    });
    assert_eq!(reading(&side).stack_count(), 0, "it holds no land figure");
    assert!(reading(&side).prepared().is_some(), "the models for the ships-only view were made");
}

#[derive(Default)]
struct HeldMeshes {
    armed: std::sync::atomic::AtomicBool,
    entered: std::sync::atomic::AtomicU64,
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl HeldMeshes {
    fn arm(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::Release);
    }

    fn entered(&self) -> u64 {
        self.entered.load(std::sync::atomic::Ordering::Acquire)
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

impl super::models::Select for HeldMeshes {
    fn choose_meshes(&self, _frame: &replay_frame::frame::DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), stacks: &std::collections::HashMap<u64, super::meshes::Asked>, asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> std::io::Result<super::meshes::Chosen> {
        if self.armed.load(std::sync::atomic::Ordering::Acquire) {
            self.entered.fetch_add(1, std::sync::atomic::Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(60);
            let mut open = self.open.lock().unwrap();
            while !*open {
                let Some(left) = until.checked_duration_since(std::time::Instant::now()) else { break };
                open = self.changed.wait_timeout(open, left).unwrap().0;
            }
        }
        let mut meshes: Vec<(u64, super::meshes::Mesh)> = stacks.keys().filter(|oldest| asked.contains(oldest)).map(|&oldest| (oldest, Ok(Some(oldest as usize)))).collect();
        meshes.sort_unstable_by_key(|&(oldest, _)| oldest);
        let made = viewer_unit_models::unit_models::Made { selections: meshes.len(), ..Default::default() };
        Ok(super::meshes::Chosen { meshes, made })
    }

    fn prepare(&self, _frame: &replay_frame::frame::DecodedFrame, _perspective: &Choice, _convoys: Option<&[viewer_unit_models::trade_routes::ConvoyLine]>, _naval: bool, _kept: &mut super::models::Kept) -> std::io::Result<viewer_unit_models::unit_models::PreparedModels> {
        Ok(viewer_unit_models::unit_models::PreparedModels::drawn_by_layers())
    }
}

#[test]
fn figures_do_not_stand_while_a_stack_handed_to_the_meshes_is_not_chosen() {
    let entry = open(record("units-meshes-owed"), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let held = Arc::new(HeldMeshes::default());
    let mut units = Units::new(flat()).in_window(Arc::clone(&held) as Arc<dyn super::models::Select>);
    units.reading = false;
    units.set_vision(Choice::default(), Arc::new(Vision::default()));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(units)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    held_through(&mut side, 64);
    side.settle_all().unwrap();
    let stood = layer(&side).mesh_hour().expect("the meshes answered");
    assert_eq!(layer(&side).meshes().map(HashMap::len), Some(3), "the three placed figures have meshes: {}", layer(&side).mesh_state());
    assert!(layer(&side).figures_stand(stood), "every stack handed is chosen: {}; {}", layer(&side).figures_state(), layer(&side).mesh_state());

    held.arm();
    held_through(&mut side, 104);
    frames_at(&mut side, Some(104), 0, "a build held on the meshes' worker with nothing carried", |side| held.entered() > 0 && layer(side).carried().entities == 0);
    assert!(layer(&side).mesh_hour().is_some_and(|hour| hour >= stood), "the meshes' rows stand past hour {stood}: {}", layer(&side).figures_state());
    assert!(!layer(&side).figures_stand(stood), "a stack handed on is not chosen yet: {}", layer(&side).figures_state());

    held.open();
    frames_at(&mut side, Some(104), 0, "the held build answered and every stack chosen", |side| layer(side).figures_stand(stood));
    assert_eq!(layer(&side).meshes().and_then(|meshes| meshes.get(&102).copied()), Some(102), "the newest stack's mesh");
}

#[test]
fn the_model_picker_reads_every_division_where_a_figure_stands_with_its_route_facts() {
    let recording = recorded::record("units-pick", HOURS, |at, store| {
        campaign(at, store);
        if at == 0 {
            let mut movement = store.table(TableId::DivisionMovement.index()).unwrap();
            movement.insert_at(1, 105).unwrap();
            movement.write(1, column(TableId::DivisionMovement, "naval_transfer"), Cell::U8(1)).unwrap();
            movement.write(1, column(TableId::DivisionMovement, "path_total"), Cell::U32(0)).unwrap();
            movement.write(1, column(TableId::DivisionMovement, "path"), Cell::U32List(&[])).unwrap();
            movement.write(1, column(TableId::DivisionMovement, "target"), Cell::U32(0)).unwrap();
        }
        if at == 10 {
            store.table(TableId::Divisions.index()).unwrap().write(0, column(TableId::Divisions, "roster_owner"), Cell::U32(8)).unwrap();
        }
    });
    let entry = open(recording, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Units::new(flat()))], View::default()).unwrap();
    layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let divisions = layer(&side).pick_divisions();
    let facts = |divisions: &HashMap<u32, Vec<super::PickDivision>>, province: u32| {
        divisions[&province].iter().map(|d| (d.id, d.owner, d.roster, d.hop, d.unrouted)).collect::<Vec<_>>()
    };
    assert_eq!(
        facts(&divisions, 10),
        [(101, 3, Some(3), None, None), (102, 3, Some(3), None, None), (103, 3, Some(3), None, None), (104, 4, Some(4), Some(11), Some(false))],
        "a division without a movement row keeps its roster and settles no route fact; a routed transfer keeps its first hop"
    );
    assert!(divisions[&10].iter().all(|d| d.kind.is_none()), "without route geometry no stack kind is placed");
    assert_eq!(
        facts(&divisions, 20),
        [(105, 3, Some(3), Some(0), Some(true))],
        "a transfer with no ordinary route has hop zero and is passed over"
    );
    through(&mut side, 12);
    side.settle_all().unwrap();
    let stepped = layer(&side).pick_divisions();
    assert_eq!(
        facts(&stepped, 10),
        [(101, 3, Some(4), None, None), (102, 3, Some(3), None, None), (103, 3, Some(3), None, None), (104, 4, Some(4), Some(11), Some(false))],
        "a roster-only step updates the cached roster of an unmoved division"
    );
}

#[test]
fn a_stack_asked_and_withdrawn_before_the_meshes_worker_takes_them_is_not_waited_for() {
    let recording = recorded::record("units-meshes-withdrawn", HOURS, |at, store| {
        campaign(at, store);
        store.status(TableId::DivisionEquipment.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        if at == 0 {
            let mut equipment = store.table(TableId::DivisionEquipment.index()).unwrap();
            for (slot, id) in [101_u64, 102, 103, 104, 105, 106].into_iter().enumerate() {
                equipment.insert_at(slot as u32, u128::from(id) << 32).unwrap();
            }
        }
    });
    let choices = Arc::new(CountedChoices::default());
    let mut side = meshes_side(recording, &choices);
    let units = side.layers_mut()[0].any_mut().and_then(|layer| layer.downcast_mut::<Units>()).expect("the units layer");
    let ours = units.vision.as_ref().map(|(_, vision)| vision.ours()).expect("a vision");
    let (_, asked) = units.asking.get(&102).copied().expect("102's stack asks a mesh");
    assert!(!units.asking.contains_key(&103), "103 leads no stack: its equipment rows are not declared");
    let meshes = units.meshes.as_mut().expect("in the window");
    assert!(meshes.stand(), "every stack handed is chosen before the test: {}", meshes.state());
    let stood = meshes.hour.expect("the meshes answered");
    let before = choices.asked.lock().unwrap().len();
    meshes.ask(ours, vec![super::meshes::Ask::Stack(103, asked), super::meshes::Ask::Gone(103)]);
    meshes.join(stood, Settling::Whole, &RetirementFlag::default());
    assert!(meshes.stand(), "the withdrawn stack is not waited for: {}", meshes.state());
    assert_eq!(meshes.take_counts().waiting, 0, "no stack waits for its rows: {}", meshes.state());
    assert!(!meshes.meshes.contains_key(&103) && !meshes.withheld.contains_key(&103), "no mesh for the withdrawn stack");
    assert!(!choices.asked.lock().unwrap()[before..].contains(&103), "the withdrawn stack is never chosen");
    assert_eq!(meshes.meshes.keys().copied().collect::<BTreeSet<u64>>(), BTreeSet::from([101, 102, 104]), "the stacks held keep their meshes");
}

fn legacy_fast_inverse() -> u64 {
    let factor = 0x517c_c1b7_2722_0a95_u64;
    let mut inv = 1u64;
    for _ in 0..6 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(factor.wrapping_mul(inv)));
    }
    debug_assert_eq!(factor.wrapping_mul(inv), 1);
    inv
}

fn legacy_fast_colliding_u64(count: usize, shift: u32) -> Vec<u64> {
    let inv = legacy_fast_inverse();
    (1..=count as u64).map(|i| (i << shift).wrapping_mul(inv)).collect()
}

fn legacy_fast_hash_stack_key(key: super::StackKey) -> u64 {
    let factor = 0x517c_c1b7_2722_0a95_u64;
    let step = |state: u64, word: u64| (state.rotate_left(5) ^ word).wrapping_mul(factor);
    let state = step(step(0, u64::from(key.0)), u64::from(key.1));
    match key.2 {
        None => step(state, 0),
        Some([a, b]) => step(step(step(state, 1), u64::from(a)), u64::from(b)),
    }
}

fn legacy_fast_colliding_stacks(count: usize, bucket_mask: u64) -> Vec<super::StackKey> {
    let mut keys = Vec::with_capacity(count);
    let mut province = 1u32;
    while keys.len() < count {
        let key = (province, 3, None);
        if legacy_fast_hash_stack_key(key) & bucket_mask == 0 {
            keys.push(key);
        }
        province += 1;
    }
    keys
}

#[test]
fn units_hash_indexes_randomize_instances_and_avoid_fixed_fast_collisions() {
    use std::hash::BuildHasher;
    let units = Units::new(flat());
    let _: &foldhash::fast::RandomState = units.asking.hasher();
    let _: &foldhash::fast::RandomState = units.combat.hasher();
    let _: &foldhash::fast::RandomState = units.readers.hasher();
    let _: &foldhash::fast::RandomState = units.links.hasher();
    let _: &foldhash::fast::RandomState = units.by_key.hasher();

    let u64_keys = legacy_fast_colliding_u64(512, 12);
    let stack_keys = legacy_fast_colliding_stacks(512, 511);

    let buckets = 512usize;
    let seeded = foldhash::fast::FixedState::with_seed(0x9e37_79b9_7f4a_7c15);
    let max_load = |hashes: Vec<u64>| {
        let mut load = vec![0u32; buckets];
        for hash in hashes {
            load[hash as usize & (buckets - 1)] += 1;
        }
        *load.iter().max().unwrap()
    };
    let u64_max = max_load(u64_keys.iter().map(|&k| seeded.hash_one(k)).collect());
    assert!(u64_max <= 12, "{u64_max} legacy-Fast colliding division keys shared a bucket");
    let stack_max = max_load(stack_keys.iter().map(|&k| seeded.hash_one(k)).collect());
    assert!(stack_max <= 12, "{stack_max} legacy-Fast colliding stack keys shared a bucket");

    let ids = legacy_fast_colliding_u64(32, 12);
    let held_ids = ids.clone();
    let recording = recorded::record("units-adversarial-keys", 96, move |at, store| {
        for table in TABLES {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        if at == 0 {
            let mut links = store.table(TableId::CountryLinks.index()).unwrap();
            links.insert_at(0, 7).unwrap();
            links.write(0, column(TableId::CountryLinks, "canonical_slot"), Cell::U32(3)).unwrap();
            for (slot, &id) in held_ids.iter().enumerate() {
                let province = if slot % 2 == 0 { 10 } else { 20 };
                division(store, slot as u32, id, province, 7);
            }
        } else if at == 32 {
            let mut combats = store.table(TableId::LandCombats.index()).unwrap();
            combats.insert_at(0, 1).unwrap();
            combats.write(0, column(TableId::LandCombats, "defenders"), Cell::U64List(&held_ids)).unwrap();
        } else if at == 64 {
            let min_even = held_ids.iter().enumerate().filter(|(slot, _)| slot % 2 == 0).min_by_key(|(_, id)| *id).unwrap().0 as u32;
            store.table(TableId::Divisions.index()).unwrap().delete(min_even).unwrap();
            store.table(TableId::DivisionStatus.index()).unwrap().delete(min_even).unwrap();
        }
    });
    let entry = open(recording, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Units::new(flat()))], View::default()).unwrap();
    layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let expected_min_even = *ids.iter().enumerate().filter(|(slot, _)| slot % 2 == 0).map(|(_, id)| id).min().unwrap();
    let expected_min_odd = *ids.iter().enumerate().filter(|(slot, _)| slot % 2 == 1).map(|(_, id)| id).min().unwrap();
    assert_eq!(oldest(&side), vec![((10, 3, None), expected_min_even), ((20, 3, None), expected_min_odd)]);
    composed_equal(&side);

    through(&mut side, 40);
    side.settle_all().unwrap();
    let figure = layer(&side).figures()[&(10, 3, None)].placed.as_ref().unwrap().as_ref().map(|(target, _)| target.instance.animation).unwrap();
    assert_eq!(figure, DEFEND);
    composed_equal(&side);

    through(&mut side, 72);
    side.settle_all().unwrap();
    let second_min_even = *ids.iter().enumerate().filter(|(slot, _)| slot % 2 == 0).map(|(_, id)| id).filter(|&&id| id != expected_min_even).min().unwrap();
    assert_eq!(oldest(&side), vec![((10, 3, None), second_min_even), ((20, 3, None), expected_min_odd)]);
    composed_equal(&side);
}

#[test]
fn combat_changes_with_moving_divisions_keep_deterministic_land_target_order_after_slot_reuse() {
    let bounds: BTreeMap<u32, [f32; 4]> = (10..=300u32).map(|p| (p, [p as f32 * 10.0, 1000.0, p as f32 * 10.0 + 5.0, 1005.0])).collect();
    let ground: Arc<dyn Ground> = Arc::new(Flat { bounds });
    let initial: Vec<u64> = (101..=108).collect();
    let fresh_ids: Vec<u64> = (201..=208).collect();
    let initial_rec = initial.clone();
    let fresh_rec = fresh_ids.clone();
    let recording = recorded::record("units-combat-slot-reuse", 64, move |at, store| {
        for table in TABLES {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        if at == 0 {
            let mut links = store.table(TableId::CountryLinks.index()).unwrap();
            links.insert_at(0, 7).unwrap();
            links.write(0, column(TableId::CountryLinks, "canonical_slot"), Cell::U32(3)).unwrap();
            for (slot, &id) in initial_rec.iter().enumerate() {
                division(store, slot as u32, id, 10 + slot as u32 * 10, 7);
            }
            let mut combats = store.table(TableId::LandCombats.index()).unwrap();
            combats.insert_at(0, 1).unwrap();
            combats.write(0, column(TableId::LandCombats, "defenders"), Cell::U64List(&initial_rec)).unwrap();
        } else if at == 20 {
            store.table(TableId::LandCombats.index()).unwrap().delete(0).unwrap();
            let mut divisions = store.table(TableId::Divisions.index()).unwrap();
            for slot in 0..initial_rec.len() as u32 {
                divisions.write(slot, column(TableId::Divisions, "province"), Cell::U32(110 + slot * 10)).unwrap();
            }
        } else if at == 40 {
            for (offset, &id) in fresh_rec.iter().enumerate() {
                division(store, (initial_rec.len() + offset) as u32, id, 210 + offset as u32 * 10, 7);
            }
        }
    });
    let entry = open(recording, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let meshes: HashMap<u64, usize> = initial.iter().chain(&fresh_ids).map(|&id| (id, id as usize)).collect();
    let expected: Vec<u64> = fresh_ids.iter().rev().copied().chain(initial.iter().copied()).collect();
    for _ in 0..4 {
        let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Units::new(Arc::clone(&ground)))], View::default()).unwrap();
        layer(&side).set_vision(Choice::default(), Arc::new(Vision::default()));
        entry.playhead().play(4.0);
        entry.playhead().seek(0);
        frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
        through(&mut side, 24);
        side.settle_all().unwrap();
        through(&mut side, 48);
        side.settle_all().unwrap();
        let identities: Vec<u64> = layer(&side).land(&meshes).into_iter().map(|target| target.identity).collect();
        assert_eq!(identities, expected, "land() target order after combat-triggered slot reuse must be deterministic");
    }
}
