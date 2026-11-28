use super::{Content, Counters};
use viewer_layer_core::{layers::{Layer, View}, playhead_host::{MapSide, open, units::Units}};
use playhead::{
    Settings, Timeline,
    pages::{Pages, Run},
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::Store,
    unit::{BLOCK, BlockPage, Encoder, PartSink, Settings as Writing, reader::Extents},
};
use replay_data::rows::TableId;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};
use viewer_counters_land::members::Member;
use viewer_foundation::perspective::{Choice, Country, Sight, Vision};
use viewer_map_modes::map_mode::{MapMode, Stock};

#[derive(Default)]
struct Memory {
    bytes: Vec<u8>,
    pages: Vec<BlockPage>,
}

impl PartSink for Memory {
    fn frame(&mut self, stored: &[u8]) -> io::Result<u64> {
        let offset = self.bytes.len() as u64;
        self.bytes.extend_from_slice(stored);
        Ok(offset)
    }

    fn block(&mut self, page: BlockPage) -> io::Result<()> {
        self.pages.push(page);
        Ok(())
    }

    fn read_back(&mut self, offset: u64, stored: u32, out: &mut Vec<u8>) -> io::Result<()> {
        out.clear();
        out.extend_from_slice(&self.bytes[offset as usize..offset as usize + stored as usize]);
        Ok(())
    }
}

struct Recording {
    bytes: Vec<u8>,
    runs: Vec<Run>,
    kinds: Vec<Vec<Kind>>,
}

impl Pages for Recording {
    fn extents(&self, _run: usize) -> &dyn Extents {
        &self.bytes
    }

    fn runs(&self) -> &[Run] {
        &self.runs
    }

    fn kinds(&self, table: usize) -> Option<&[Kind]> {
        self.kinds.get(table).map(Vec::as_slice)
    }
}

fn read() -> Vec<TableId> {
    super::declared().collect()
}

fn column(table: TableId, field: &str) -> usize {
    (0..64).find(|&column| table.field_name(column) == Some(field)).unwrap_or_else(|| panic!("{table:?}.{field}"))
}

fn record(hours: u32, status: impl Fn(u32, TableId) -> Status, hour: impl FnMut(u32, &mut Store)) -> Units {
    record_writing(hours, Writing::at(3), status, hour)
}

fn record_writing(hours: u32, writing: Writing, status: impl Fn(u32, TableId) -> Status, mut hour: impl FnMut(u32, &mut Store)) -> Units {
    let kinds = replay_recording::schema::schema().unwrap();
    let mut store = Store::new(kinds.clone(), vec![false; kinds.len()]);
    store.attach(Encoder::new(&kinds, writing).unwrap());
    let mut memory = Memory::default();
    store.begin_run(0);
    for at in 0..hours {
        let text = format!("text {at}");
        store.begin_hour(at as i32, &[text.as_bytes()]).unwrap();
        for table in read() {
            store.status(table.index(), status(at, table), true, Some(at as i32), true).unwrap();
        }
        hour(at, &mut store);
        store.end_hour(&mut memory).unwrap();
        if at % BLOCK == BLOCK - 1 {
            store.end_block();
        }
    }
    if !hours.is_multiple_of(BLOCK) {
        store.end_run(&mut memory).unwrap();
    }
    let recording = Arc::new(Recording { bytes: memory.bytes, runs: vec![Run::new(0, memory.pages).unwrap()], kinds: kinds.clone() });
    let pages: Arc<dyn Pages> = recording;
    let timeline = Timeline::whole(&*pages).unwrap();
    static MADE: AtomicU32 = AtomicU32::new(0);
    Units { name: format!("counters {}", MADE.fetch_add(1, Ordering::Relaxed)), pages, timeline, kinds: Arc::new(kinds) }
}

fn write(store: &mut Store, table: TableId, slot: u32, field: &str, cell: Cell<'_>) {
    store.table(table.index()).unwrap().write(slot, column(table, field), cell).unwrap();
}

fn insert(store: &mut Store, table: TableId, slot: u32, key: u128) {
    store.table(table.index()).unwrap().insert_at(slot, key).unwrap();
}

fn division(store: &mut Store, slot: u32, id: u64, owner: u32, province: u32, hit_points: i64) {
    insert(store, TableId::Divisions, slot, u128::from(id));
    for (field, value) in [("roster_owner", owner), ("logical_owner", owner), ("expeditionary_owner", 0), ("province", province)] {
        write(store, TableId::Divisions, slot, field, Cell::U32(value));
    }
    write(store, TableId::Divisions, slot, "template", Cell::U64(7));
    write(store, TableId::Divisions, slot, "hit_points", Cell::I64(hit_points));
    write(store, TableId::Divisions, slot, "organisation", Cell::I64(4));
    write(store, TableId::Divisions, slot, "orders_group", Cell::U64(0));
}

fn world(store: &mut Store) {
    for (slot, key) in [(0, 10u128), (1, 11)] {
        insert(store, TableId::CountryLinks, slot, key);
        write(store, TableId::CountryLinks, slot, "canonical_slot", Cell::U32(slot));
        insert(store, TableId::Countries, slot, key);
    }
    division(store, 0, 100, 10, 5, 5);
    division(store, 1, 101, 11, 5, 6);
    division(store, 2, 102, 10, 6, 7);
    order(store, 0, 5, &[101, 100]);
    order(store, 1, 6, &[102]);
    insert(store, TableId::DivisionStatus, 0, 100);
    for (field, value) in [("entrenchment", 3), ("entrenchment_cap", 9), ("attrition_value", 5), ("attrition_threshold", 2), ("strength_factor", 100_000)] {
        write(store, TableId::DivisionStatus, 0, field, Cell::I64(value));
    }
    write(store, TableId::DivisionStatus, 0, "supply_ratio", Cell::OptionalI64(Some(80)));
    write(store, TableId::DivisionStatus, 0, "exercising", Cell::U8(1));
    insert(store, TableId::DivisionPanelStats, 0, 100);
    write(store, TableId::DivisionPanelStats, 0, "panel_stats[0]", Cell::OptionalI64(Some(100_000)));
    write(store, TableId::DivisionPanelStats, 0, "panel_stats[1]", Cell::OptionalI64(Some(80_000)));
}

fn order(store: &mut Store, slot: u32, province: u32, units: &[u64]) {
    if store.table(TableId::ProvinceUnitOrder.index()).unwrap().slot(u128::from(province)).is_none() {
        insert(store, TableId::ProvinceUnitOrder, slot, u128::from(province));
    }
    write(store, TableId::ProvinceUnitOrder, slot, "mixed", Cell::U64List(units));
}

fn quick() -> Settings {
    Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }
}

fn fog_off() -> View {
    View { fog: false, ..View::default() }
}

fn layer_seeing_all() -> Counters {
    let layer = Counters::new(Arc::new(Content::bare()));
    layer.set_vision(Choice { country: Country::Recorded, fog: false }, Arc::new(Vision::sharing(0, []).without_fog()));
    layer.with_relations(Arc::new(viewer_counter_kit::type_tint::Index::default()))
}

fn side_for(units: Units) -> MapSide {
    let entry = open(units, quick()).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer_seeing_all())], fog_off()).unwrap();
    assert_eq!(side.refusals().count(), 0, "{:?}", side.refusals().collect::<Vec<_>>());
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn frames_until(side: &mut MapSide, done: impl FnMut(&MapSide, &viewer_layer_core::playhead_host::Frame) -> bool) {
    viewer_layer_core::test_support::recorded::frames_until(side, done);
}

fn reach(side: &mut MapSide, hour: i32) {
    let cap = (hour + 3).min(side.entry().playhead().timeline().last());
    loop {
        assert!(side.entry().playhead().error().is_none(), "the playhead stopped before hour {hour}: {:?}", side.entry().playhead().error());
        viewer_layer_core::test_support::recorded::reads_landed(side);
        let before = side.entry().playhead().step_state();
        let head = before.0.unwrap_or(0).min(cap);
        let frame = if side.composing() { side.frame_waiting(head) } else { side.frame(head) }.unwrap();
        if !side.composing() && side.applied().is_some_and(|applied| applied >= hour) {
            return;
        }
        if side.composing() || frame.steps > 0 || frame.swapped.is_some() || frame.superseded {
            continue;
        }
        let more = side
            .wait_step_after(before, cap)
            .unwrap_or_else(|error| panic!("the playhead stopped before hour {hour}: {error}"));
        assert!(
            more,
            "the map side reached hour {:?} (head cap {cap}) without reaching hour {hour}: {}",
            side.applied(),
            viewer_layer_core::test_support::recorded::seen(side),
        );
    }
}

fn hold(side: &mut MapSide, hour: i32) {
    while side.entry().playhead().newest().is_none_or(|newest| newest < hour) {
        let head = side.entry().playhead().newest().unwrap_or(0).min(hour);
        side.frame(head).unwrap();
        std::thread::yield_now();
    }
    side.frame(hour).unwrap();
}

fn layer(side: &MapSide) -> &Counters {
    side.parts().unwrap().1[0].any().downcast_ref::<Counters>().unwrap()
}

fn settled_equals_composed(side: &mut MapSide) {
    side.settle_all().unwrap();
    let (mirrors, _) = side.parts().unwrap();
    let mut fresh = layer(side).fresh();
    fresh.compose(mirrors).unwrap();
    assert_eq!(layer(side).differ(&fresh), (0, None));
}

fn members(side: &MapSide) -> Vec<((u32, u32), Vec<Member>)> {
    layer(side).sources().map(|(source, members)| (source, members.cloned().collect())).collect()
}

fn member(side: &MapSide, id: u64) -> Option<((u32, u32), Member)> {
    members(side).into_iter().find_map(|(source, members)| members.into_iter().find(|member| member.id == id).map(|member| (source, member)))
}

fn ids(side: &MapSide) -> Vec<((u32, u32), Vec<u64>)> {
    members(side).into_iter().map(|(source, members)| (source, members.into_iter().map(|member| member.id).collect())).collect()
}

#[test]
fn a_division_changed_in_a_step_is_marked_once_and_settles_to_the_composed_value() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => {
            write(store, TableId::Divisions, 0, "hit_points", Cell::I64(2));
            write(store, TableId::Divisions, 0, "organisation", Cell::I64(2));
            write(store, TableId::DivisionStatus, 0, "entrenchment", Cell::I64(4));
        }
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 150);
    side.settle_all().unwrap();
    assert_eq!(ids(&side), vec![((5, 0), vec![101, 100]), ((6, 0), vec![102])], "occupant order, not id order");
    let (_, first) = member(&side, 100).unwrap();
    assert_eq!((first.ordinal, first.owner, first.sight, first.bars, first.status.entrenchment, first.status.attrition), (1, 0, Sight::Full, true, 3, true));
    assert_eq!((first.shared_vision, first.status.own), (Ok(true), true));
    assert_eq!(member(&side, 101).unwrap().1.shared_vision, Ok(false));
    assert_eq!(
        first.status.supply_ratio,
        viewer_unit_reads::counter_status::SupplyRatio::Interval([80_000, 80_999]),
        "stored in units of its display step, a thousandth"
    );
    assert!(!member(&side, 101).unwrap().1.bars, "no status row: no bars");
    let marked = layer(&side).marked();
    reach(&mut side, 204);
    assert_eq!(layer(&side).marked() - marked, 1, "three cells of one division in two tables mark it once");
    settled_equals_composed(&mut side);
    let (_, changed) = member(&side, 100).unwrap();
    assert_eq!(changed.status.entrenchment, 4);
    assert_ne!((changed.hp, changed.org), (first.hp, first.org));
}

#[test]
fn exact_supply_ratio_is_preserved_without_display_step_rounding_and_tracks_sub_percent_changes() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => {
            world(store);
            write(store, TableId::DivisionStatus, 0, "supply_ratio", Cell::OptionalI64(None));
            write(store, TableId::DivisionStatus, 0, "supply_ratio_exact", Cell::OptionalI64(Some(35_499)));
        }
        200 => {
            write(store, TableId::DivisionStatus, 0, "supply_ratio_exact", Cell::OptionalI64(Some(35_500)));
        }
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 150);
    side.settle_all().unwrap();
    assert_eq!(
        member(&side, 100).unwrap().1.status.supply_ratio,
        viewer_unit_reads::counter_status::SupplyRatio::Exact(35_499)
    );
    reach(&mut side, 204);
    settled_equals_composed(&mut side);
    assert_eq!(
        member(&side, 100).unwrap().1.status.supply_ratio,
        viewer_unit_reads::counter_status::SupplyRatio::Exact(35_500)
    );
}

#[test]
fn a_division_changed_twice_before_it_settles_shows_its_newest_values() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => write(store, TableId::Divisions, 1, "hit_points", Cell::I64(1)),
        201 => {
            write(store, TableId::Divisions, 1, "province", Cell::U32(6));
            order(store, 0, 5, &[100]);
            order(store, 1, 6, &[102, 101]);
        }
        202 => write(store, TableId::Divisions, 1, "hit_points", Cell::I64(3)),
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 150);
    side.settle_all().unwrap();
    let settles = side.hold_settles();
    reach(&mut side, 204);
    viewer_layer_core::test_support::recorded::released_unit_by_unit(&mut side, settles);
    settled_equals_composed(&mut side);
    assert_eq!(ids(&side), vec![((5, 0), vec![100]), ((6, 0), vec![102, 101])]);
    let (_, moved) = member(&side, 101).unwrap();
    assert_eq!(moved.ordinal, 1);
    let mirrors = side.parts().unwrap().0;
    let divisions = mirrors.get(TableId::Divisions).unwrap();
    assert_eq!(divisions.raw_cell(column(TableId::Divisions, "hit_points"), divisions.slot(101).unwrap()), Some(3));
}

#[test]
fn a_division_deleted_and_another_inserted_in_its_slot_updates_every_source_naming_them() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => {
            store.table(TableId::Divisions.index()).unwrap().delete(2).unwrap();
            order(store, 1, 6, &[]);
        }
        201 => {
            division(store, 2, 103, 11, 5, 9);
            order(store, 0, 5, &[101, 103, 100]);
        }
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 200);
    settled_equals_composed(&mut side);
    assert_eq!(ids(&side), vec![((5, 0), vec![101, 100])], "the deleted division's source is gone with it");
    reach(&mut side, 201);
    settled_equals_composed(&mut side);
    assert_eq!(ids(&side), vec![((5, 0), vec![101, 103, 100])], "its slot taken by another");
    assert_eq!(member(&side, 103).unwrap().1.owner, 1);
    assert!(member(&side, 102).is_none());
}

#[test]
fn tables_not_wholly_recorded_draw_as_today_draws_them() {
    let units = record(BLOCK, |at, table| match (at, table) {
        (240..=247, TableId::ProvinceUnitOrder) => Status::Truncated { missing: 1 },
        (260..=267, TableId::DivisionStatus) => Status::Unavailable { reason: 1 },
        (280..=287, TableId::CountryLinks) => Status::NotRecorded,
        _ => Status::Complete,
    }, |at, store| match at {
        0 => world(store),
        236 => order(store, 0, 5, &[101]),
        250 => order(store, 0, 5, &[101, 100]),
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 236);
    settled_equals_composed(&mut side);
    assert!(member(&side, 100).is_none(), "absent from its province's array: no counter");
    reach(&mut side, 244);
    settled_equals_composed(&mut side);
    let (source, unordered) = member(&side, 100).expect("without a whole occupant order every division stands, at ordinal zero");
    assert_eq!((source, unordered.ordinal), ((5, 0), 0));
    assert_eq!(ids(&side)[0], ((5, 0), vec![100, 101]), "ties in id order");
    reach(&mut side, 264);
    settled_equals_composed(&mut side);
    let (_, unavailable) = member(&side, 100).unwrap();
    assert!(!unavailable.bars && unavailable.status.exercising.is_none(), "an unavailable status reads as a missing row, never the last one");
    reach(&mut side, 284);
    side.settle_all().unwrap();
    assert!(layer(&side).refused().is_some_and(|why| why.contains("CountryLinks")), "{:?}", layer(&side).refused());
    assert!(members(&side).is_empty(), "nothing is drawn from the last rows");
    reach(&mut side, 292);
    settled_equals_composed(&mut side);
    assert_eq!(layer(&side).refused(), None);
    assert_eq!(member(&side, 100).unwrap().1.ordinal, 1, "after the gap, the arrays again");
}

#[test]
fn a_mode_without_land_counters_declares_nothing_and_a_mode_with_them_composes() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => {
            write(store, TableId::Divisions, 0, "province", Cell::U32(6));
            order(store, 0, 5, &[101]);
            order(store, 1, 6, &[102, 100]);
        }
        _ => {}
    });
    let resources = View { map_mode: MapMode::Stock(Stock::Resources), ..fog_off() };
    let kinds = replay_recording::schema::schema().unwrap();
    let mut inputs = viewer_layer_core::layers::Inputs::new(&kinds, "counters");
    layer_seeing_all().inputs(&resources, &mut inputs).unwrap();
    assert!(inputs.into_tables().is_empty(), "the resources mode draws no unit counters");
    let mut side = side_for(units);
    reach(&mut side, 150);
    side.set_view(resources).unwrap();
    assert_eq!(side.refusals().count(), 0, "hidden, not refused");
    hold(&mut side, 206);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).carried().entities, 0, "a view that draws nothing carries nothing, though steps changed its tables");
    assert_eq!(layer(&side).held(), layer_seeing_all().held(), "and holds what an empty layer holds");
    assert!(members(&side).is_empty(), "nothing is kept while hidden");
    settled_equals_composed(&mut side);
    side.set_view(fog_off()).unwrap();
    frames_until(&mut side, |side, _| !side.composing() && side.draws("counters") && layer(side).refused().is_none());
    settled_equals_composed(&mut side);
    assert_eq!(layer(&side).refused(), None);
    assert!(!members(&side).is_empty(), "composed again from the mirrors it is given");
}

#[test]
fn a_new_vision_takes_every_division_s_sight_again() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| {
        if at == 0 {
            world(store);
        }
    });
    let mut side = side_for(units);
    assert!(member(&side, 101).is_some());
    let fogged = Arc::new(Vision::sharing(0, []));
    layer(&side).set_vision(Choice { country: Country::Recorded, fog: true }, fogged);
    frames_until(&mut side, |side, frame| frame.steps > 0 && layer(side).carried().entities == 0);
    settled_equals_composed(&mut side);
    assert!(member(&side, 101).is_none(), "another country's division, undetected under fog, has no counter");
    assert_eq!(member(&side, 100).unwrap().1.sight, Sight::Full, "our own stays in full");
}

#[test]
fn the_layer_declares_against_this_schema() {
    let kinds = replay_recording::schema::schema().unwrap();
    let mut inputs = viewer_layer_core::layers::Inputs::new(&kinds, "counters");
    layer_seeing_all().inputs(&fog_off(), &mut inputs).unwrap();
    let declared: Vec<usize> = inputs.into_tables().into_keys().collect();
    let mut read: Vec<usize> = read().into_iter().map(|table| table.index()).collect();
    read.sort_unstable();
    read.dedup();
    assert_eq!(declared, read, "the tables the layer declares are its list, each once");
}

#[test]
fn outside_the_view_a_change_stays_marked_and_inside_it_is_made() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => {
            write(store, TableId::Divisions, 0, "hit_points", Cell::I64(2));
            write(store, TableId::Divisions, 2, "hit_points", Cell::I64(1));
        }
        _ => {}
    });
    let entry = open(units, quick()).unwrap();
    let mut counters = layer_seeing_all();
    counters.admit_only(|province| province == 5);
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(counters)], fog_off()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    reach(&mut side, 150);
    side.settle_all().unwrap();
    assert_eq!(ids(&side), vec![((5, 0), vec![101, 100])], "only the admitted province's members are made");
    let before = member(&side, 100).unwrap().1.hp;
    reach(&mut side, 204);
    side.settle_all().unwrap();
    assert_ne!(member(&side, 100).unwrap().1.hp, before, "inside: made");
    assert!(layer(&side).outside() > 0, "outside: kept marked");
    assert_eq!(layer(&side).carried().entities, 0, "what waits outside is not late");
    settled_equals_composed(&mut side);
}

#[test]
fn a_destination_changing_hands_gives_only_the_division_going_there_its_action_again() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => {
            world(store);
            insert(store, TableId::DivisionMovement, 0, 102);
            write(store, TableId::DivisionMovement, 0, "path_total", Cell::U32(1));
            write(store, TableId::DivisionMovement, 0, "path", Cell::U32List(&[7]));
            insert(store, TableId::Provinces, 0, 7);
            write(store, TableId::Provinces, 0, "controller", Cell::U32(10));
        }
        200 => write(store, TableId::Provinces, 0, "controller", Cell::U32(11)),
        _ => {}
    });
    let mut side = side_for(units);
    reach(&mut side, 150);
    side.settle_all().unwrap();
    let action = |side: &MapSide, id: u64| layer(side).sources_acting().flat_map(|(_, members)| members.map(|(member, action)| (member.id, action)).collect::<Vec<_>>()).find(|&(held, _)| held == id).map(|(_, action)| action);
    assert_eq!(action(&side, 102), Some(Ok(4)), "its own controller's province: an ordinary move");
    assert_eq!(action(&side, 101), Some(Err("DivisionMovement division")), "not the viewer's: no action, as today");
    assert_eq!(action(&side, 100), Some(Err("DivisionMovement division")), "the viewer's, without a movement row");
    let marked = layer(&side).marked();
    reach(&mut side, 204);
    side.settle_all().unwrap();
    assert_eq!(layer(&side).marked() - marked, 1, "only the division going there");
    assert_eq!(action(&side, 102), Some(Err("action destination directed-war receiver unavailable")), "another controller asks the relations layer's wars, which no layer gives here");
    settled_equals_composed(&mut side);
}

fn reading_side() -> MapSide {
    reading_side_with(false, |_, _| {})
}

fn reading_side_with(fed: bool, more: impl Fn(u32, &mut Store)) -> MapSide {
    use viewer_foundation::{
        perspective::VISION_TABLES,
        visibility::{FogContent, Topology, test_defines},
    };
    let mut tables: Vec<TableId> = read();
    tables.extend(VISION_TABLES.iter().copied().filter(|table| !tables.contains(table)).collect::<Vec<_>>());
    let kinds = replay_recording::schema::schema().unwrap();
    let mut store = Store::new(kinds.clone(), vec![false; kinds.len()]);
    store.attach(Encoder::new(&kinds, Writing::at(3)).unwrap());
    let mut memory = Memory::default();
    store.begin_run(0);
    let tags: [&[u8]; 2] = [b"AAA", b"BBB"];
    for at in 0..BLOCK {
        store.begin_hour(at as i32, if at == 0 { &tags[..] } else { &[] }).unwrap();
        for &table in &tables {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        if at == 0 {
            for (slot, raw) in [101u32, 102].into_iter().enumerate() {
                let slot = slot as u32;
                insert(&mut store, TableId::CountryLinks, slot, u128::from(raw));
                write(&mut store, TableId::CountryLinks, slot, "canonical_slot", Cell::U32(slot + 1));
                insert(&mut store, TableId::Countries, slot, u128::from(raw));
                write(&mut store, TableId::Countries, slot, "canonical_slot", Cell::U32(slot + 1));
                write(&mut store, TableId::Countries, slot, "tag", Cell::Bytes(slot));
            }
            insert(&mut store, TableId::LocalPlayer, 0, 0);
            write(&mut store, TableId::LocalPlayer, 0, "country", Cell::U32(101));
            for (slot, (province, state, controller)) in [(1u32, 100u32, 101u32), (2, 100, 101), (3, 200, 102), (4, 200, 102), (9, 0, 0)].into_iter().enumerate() {
                insert(&mut store, TableId::Provinces, slot as u32, u128::from(province));
                write(&mut store, TableId::Provinces, slot as u32, "state", Cell::U32(state));
                write(&mut store, TableId::Provinces, slot as u32, "controller", Cell::U32(controller));
            }
            insert(&mut store, TableId::WarRelations, 0, 101 << 32 | 102);
            for (slot, (id, owner, province)) in [(901u64, 101u32, 1u32), (905, 102, 4)].into_iter().enumerate() {
                division(&mut store, slot as u32, id, owner, province, 5);
                write(&mut store, TableId::Divisions, slot as u32, "reveal_skip", Cell::U8(0));
                order(&mut store, slot as u32, province, &[id]);
            }
        }
        more(at, &mut store);
        store.end_hour(&mut memory).unwrap();
    }
    store.end_block();
    let recording = Arc::new(Recording { bytes: memory.bytes, runs: vec![Run::new(0, memory.pages).unwrap()], kinds: kinds.clone() });
    let pages: Arc<dyn Pages> = recording;
    let timeline = Timeline::whole(&*pages).unwrap();
    static MADE: AtomicU32 = AtomicU32::new(0);
    let units = Units { name: format!("counters reading the vision layer {}", MADE.fetch_add(1, Ordering::Relaxed)), pages, timeline, kinds: Arc::new(kinds) };
    let content = {
        let neighbors = [(1, vec![2]), (2, vec![1, 3]), (3, vec![2, 4]), (4, vec![3, 9]), (9, vec![4])].into_iter().collect();
        Arc::new(viewer_layer_core::vision::Content::new(Ok(FogContent { topology: Topology { land: (1..=4).collect(), sea: std::collections::BTreeSet::from([9]), neighbors }, defines: test_defines() })))
    };
    let counters = Counters::new(Arc::new(Content::bare())).with_relations(Arc::new(viewer_counter_kit::type_tint::Index::default())).reading_vision();
    let entry = open(units, quick()).unwrap();
    let counters: Box<dyn Layer> = if fed { Box::new(super::fed::Fed::new(counters)) } else { Box::new(counters) };
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(viewer_layer_core::vision::Vision::new(content)), counters];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View { fog: true, perspective: Choice { country: Country::Recorded, fog: true }, ..View::default() }).unwrap();
    assert_eq!(side.refusals().count(), 0, "{:?}", side.refusals().collect::<Vec<_>>());
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    reach(&mut side, 2);
    side
}

fn reading(side: &MapSide) -> &Counters {
    side.parts().unwrap().1[1].any().downcast_ref::<Counters>().unwrap()
}

fn reading_sight(side: &MapSide) -> &viewer_layer_core::vision::Vision {
    side.parts().unwrap().1[0].any().downcast_ref::<viewer_layer_core::vision::Vision>().unwrap()
}

fn reading_settled(side: &mut MapSide) {
    for _ in 0..8 {
        side.settle_all().unwrap();
        if side.parts().unwrap().1.iter().all(|layer| layer.carried().entities == 0) {
            return;
        }
    }
    panic!("eight whole settles left work carried");
}

fn reading_member(side: &MapSide, id: u64) -> Option<Member> {
    reading(side).sources().flat_map(|(_, members)| members.cloned().collect::<Vec<_>>()).find(|member| member.id == id)
}

fn reading_equals_composed(side: &MapSide) {
    let (mirrors, layers) = side.parts().unwrap();
    let mut fresh = reading(side).fresh();
    fresh.compose_with(mirrors, &viewer_layer_core::layers::Upstream::new(&layers[..1])).unwrap();
    assert_eq!(reading(side).differ(&fresh), (0, None));
}

#[test]
fn the_counters_reading_the_vision_layer_follow_a_change_of_the_viewing_country_with_nothing_given() {
    let mut side = reading_side();
    reading_settled(&mut side);
    assert_eq!(reading(&side).refused(), None);
    assert!(reading(&side).perspective().is_none(), "no vision is given to the layer");
    assert!(reading_member(&side, 901).is_some(), "the viewer's own division");
    assert!(reading_member(&side, 905).is_none(), "the enemy's, two provinces away under fog, has no counter");
    reading_equals_composed(&side);
    side.set_view(View { fog: true, perspective: Choice { country: Country::Tag("BBB".into()), fog: true }, ..View::default() }).unwrap();
    reach(&mut side, 6);
    reading_settled(&mut side);
    assert!(reading(&side).perspective().is_none(), "still nothing given");
    assert!(reading_member(&side, 905).is_some(), "viewed as 102, its own division has its counter");
    assert!(reading_member(&side, 901).is_none(), "and 101's, two provinces away, none");
    reading_equals_composed(&side);
}

#[test]
fn a_whole_vision_publish_with_the_sight_key_unchanged_marks_every_division_again() {
    const RENAMED: u32 = 25;
    let mut side = reading_side_with(false, |at, store| {
        if at == RENAMED {
            write(store, TableId::Countries, 1, "tag", Cell::Bytes(0));
        }
    });
    reach(&mut side, RENAMED as i32 - 5);
    reading_settled(&mut side);
    assert_eq!(reading(&side).refused(), None);
    let (epoch, marked) = (reading_sight(&side).sight_epoch(), reading(&side).marked());
    reach(&mut side, RENAMED as i32 + 4);
    reading_settled(&mut side);
    assert_eq!(reading_sight(&side).sight_epoch(), epoch, "no part of the sight key moved");
    assert!(reading(&side).marked() - marked >= 2, "both divisions are marked again, not {}", reading(&side).marked() - marked);
    reading_equals_composed(&side);
}

fn phase(side: &MapSide, name: &str) -> u64 {
    layer(side).phases().find(|(held, _, _)| *held == name).map_or(0, |(_, entities, _)| entities)
}

#[test]
fn steps_mark_their_divisions_reading_no_row_and_a_division_changed_in_several_is_made_once() {
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| match at {
        0 => world(store),
        200 => {
            write(store, TableId::Divisions, 0, "hit_points", Cell::I64(2));
            write(store, TableId::Divisions, 2, "hit_points", Cell::I64(1));
        }
        204 => write(store, TableId::DivisionStatus, 0, "entrenchment", Cell::I64(5)),
        208 => {
            write(store, TableId::Divisions, 0, "hit_points", Cell::I64(3));
            order(store, 0, 5, &[100, 101]);
        }
        _ => {}
    });
    let entry = open(units, quick()).unwrap();
    let mut counters = layer_seeing_all();
    counters.admit_only(|province| province == 5);
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(counters)], fog_off()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    reach(&mut side, 150);
    side.settle_all().unwrap();
    let made = (phase(&side, "whole members"), phase(&side, "strength"));
    let settles = side.hold_settles();
    reach(&mut side, 208);
    assert_eq!(layer(&side).applied_reads(), 0, "apply marks; it reads no row");
    assert_eq!((phase(&side, "whole members"), phase(&side, "strength")), made, "nothing is made while the settles are held");
    viewer_layer_core::test_support::recorded::released_unit_by_unit(&mut side, settles);
    assert_eq!(phase(&side, "strength") - made.1, 1, "division 100, changed in three steps, is made once");
    assert_eq!(phase(&side, "whole members"), made.0, "and nothing whole: 102 is outside the view");
    assert_eq!(ids(&side), vec![((5, 0), vec![100, 101])], "its newest place in the array");
    let mirrors = side.parts().unwrap().0;
    let divisions = mirrors.get(TableId::Divisions).unwrap();
    assert_eq!(divisions.raw_cell(column(TableId::Divisions, "hit_points"), divisions.slot(100).unwrap()), Some(3));
    assert_eq!(member(&side, 100).unwrap().1.status.entrenchment, 5);
    settled_equals_composed(&mut side);
}

#[test]
fn a_compose_on_the_hosts_worker_validates_the_strategic_layout_with_the_recordings_tables() {
    let ambient = synthetic_game::enter();
    let tables = content_reader_script::test_game::tables();
    assert!(!std::ptr::eq(ambient, tables));
    let units = record(BLOCK, |_, _| Status::Complete, |at, store| {
        if at == 0 {
            world(store);
        }
    });
    let entry = open(units, quick()).unwrap();
    let counters = Counters::new(Arc::new(Content { tables: Some(tables), ..Content::bare() }));
    counters.set_vision(Choice { country: Country::Recorded, fog: false }, Arc::new(Vision::sharing(0, []).without_fog()));
    let counters = counters.with_relations(Arc::new(viewer_counter_kit::type_tint::Index::default()));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(counters)], fog_off()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    reach(&mut side, 150);
    let [before, _] = layer(&side).validated();
    entry.playhead().seek(100);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side.settle_all().unwrap();
    let [scoped, unscoped] = layer(&side).validated();
    assert!(scoped > before, "the seek's compose validated the strategic layout");
    assert_eq!(unscoped, 0, "and every validation, on whatever thread, had exactly the recording's tables in scope");
    settled_equals_composed(&mut side);
}

fn fed_side(strength: impl Fn(u32, u32) -> Option<i64>) -> MapSide {
    reading_side_with(true, move |at, store| {
        if at == 0 {
            division(store, 2, 902, 101, 2, 5);
            write(store, TableId::Divisions, 2, "reveal_skip", Cell::U8(0));
            order(store, 2, 2, &[902]);
            for (slot, id) in [(0, 901u128), (1, 902)] {
                insert(store, TableId::DivisionStatus, slot, id);
                write(store, TableId::DivisionStatus, slot, "strength_factor", Cell::I64(100_000));
                insert(store, TableId::DivisionPanelStats, slot, id);
                write(store, TableId::DivisionPanelStats, slot, "panel_stats[0]", Cell::OptionalI64(Some(100_000)));
                write(store, TableId::DivisionPanelStats, slot, "panel_stats[1]", Cell::OptionalI64(Some(80_000)));
            }
        }
        for slot in [0, 2] {
            if let Some(hit_points) = strength(at, slot) {
                write(store, TableId::Divisions, slot, "hit_points", Cell::I64(hit_points));
            }
        }
    })
}

fn fed(side: &MapSide) -> &super::fed::Fed {
    side.parts().unwrap().1[1].any().downcast_ref::<super::fed::Fed>().unwrap()
}

fn fed_equals_composed(side: &MapSide) {
    use viewer_layer_core::layers::{Settling, Published, Upstream};
    let (mirrors, layers) = side.parts().unwrap();
    let layer = fed(side);
    assert!(matches!(layer.published(), Published::Current), "settled whole, the product is the hour applied's");
    assert_eq!(layer.refused(), None);
    let upstream = Upstream::new(&layers[..1]);
    let mut fresh = layer.fresh();
    fresh.compose_with(mirrors, &upstream).unwrap();
    fresh.settle_with(mirrors, &upstream, Settling::Whole).unwrap();
    fresh.publish();
    assert_eq!(**layer.stacks().unwrap(), *fresh.published);
}

#[test]
fn steps_moving_two_stacks_are_made_on_the_worker_and_equal_a_compose_at_their_hour() {
    let mut side = fed_side(|at, _| (at >= 200).then_some(3 + i64::from(at % 5)));
    reach(&mut side, 196);
    reading_settled(&mut side);
    fed_equals_composed(&side);
    let mut product = Arc::clone(fed(&side).stacks().unwrap());
    assert_eq!(product.len(), 2, "901's stack and 902's");
    for hour in (200..=240).step_by(4) {
        let made = super::made_here();
        reach(&mut side, hour);
        reading_settled(&mut side);
        assert_eq!(super::made_here(), made, "at hour {hour} the frame thread made a stack");
        let now = Arc::clone(fed(&side).stacks().unwrap());
        assert!(!Arc::ptr_eq(&now, &product), "at hour {hour} the step moved both stacks: a new product");
        assert!(now.values().zip(product.values()).all(|(now, before)| !Arc::ptr_eq(now, before)), "and both its sources anew");
        fed_equals_composed(&side);
        product = now;
    }
}

fn step_a_frame(side: &mut MapSide, steps: u32, mut each: impl FnMut(&MapSide, i32, &viewer_layer_core::playhead_host::Frame)) {
    let mut before = fed(side).applied_hour().unwrap();
    let mut target = before + 4;
    let mut applied_steps = 0;
    let mut stood = 0u64;
    for _ in 0..50_000_000u64 {
        let playhead = side.entry().playhead();
        playhead.wait_for(|| playhead.newest().is_some_and(|newest| newest >= target)).unwrap();
        let head = playhead.newest().unwrap_or(0).min(target);
        let frame = side.frame(head).unwrap();
        let applied = fed(side).applied_hour().unwrap();
        each(side, before, &frame);
        if applied > before {
            assert_eq!(applied - before, 4, "one step a frame");
            applied_steps += 1;
            target = applied + 4;
            if applied_steps == steps {
                return;
            }
        } else if frame.steps == 0 {
            std::thread::yield_now();
        }
        stood = if applied > before { 0 } else { stood + 1 };
        assert!(stood < 2_000_000, "the map side stood at {applied} for its step to {target}: newest {:?}, composing {}, {:?}", side.entry().playhead().newest(), side.composing(), side.entry().playhead().held().2);
        before = applied;
    }
    panic!("the map side applied {applied_steps} steps of {steps}");
}

#[test]
fn with_the_worker_answering_within_a_frame_the_counters_show_the_step_before() {
    let mut side = fed_side(|at, _| (at >= 200).then_some(3 + i64::from(at % 5)));
    reach(&mut side, 196);
    let settles = side.hold_settles();
    step_a_frame(&mut side, viewer_layer_core::layers::BOUND, |_, _, _| {});
    drop(settles);
    reading_settled(&mut side);
    let blocked = fed(&side).blocked();
    let mut waits = Vec::new();
    let mut was = blocked;
    step_a_frame(&mut side, 40, |side, before, frame| {
        let now = fed(side).blocked();
        waits.push((before, frame.steps, frame.layers[1].shown, now - was));
        was = now;
        let applied = fed(side).applied_hour().unwrap();
        let shown = frame.layers[1].shown.expect("the counters show an hour");
        assert!(shown >= before && shown <= applied, "applied {applied}, the frame before {before}: the counters show {shown}");
        while !fed(side).answered() {
            std::thread::yield_now();
        }
    });
    assert_eq!(fed(&side).blocked(), blocked, "the frame thread never waited for the worker: (hour before, steps, counters shown, waits) {waits:?}");
}

#[test]
fn with_the_worker_held_the_frame_never_waits_shows_the_product_it_has_and_catches_up_in_one_take() {
    use viewer_layer_core::layers::Published;
    let mut side = fed_side(|at, _| (at >= 200).then_some(3 + i64::from(at % 5)));
    reach(&mut side, 196);
    reading_settled(&mut side);
    let had = fed(&side).applied_hour().unwrap();
    let product = Arc::clone(fed(&side).stacks().unwrap());
    let blocked = fed(&side).blocked();
    fed(&side).hold_worker(true);
    let held_steps = viewer_layer_core::layers::BOUND - 1;
    step_a_frame(&mut side, held_steps, |side, _, frame| {
        assert_eq!(frame.layers[1].shown, Some(had), "held, the counters show the hour of the product they have");
        let layer = fed(side);
        if layer.applied_hour() > Some(had) {
            assert_eq!(layer.published(), Published::At(had), "never current while its steps are with the worker");
        }
        assert!(Arc::ptr_eq(layer.stacks().unwrap(), &product));
    });
    assert_eq!(fed(&side).blocked(), blocked, "the frame thread returned at once every frame");
    let applied = fed(&side).applied_hour().unwrap();
    assert_eq!(applied, had + 4 * held_steps as i32);
    let taken = fed(&side).taken();
    fed(&side).hold_worker(false);
    while !fed(&side).answered() {
        std::thread::yield_now();
    }
    let frame = side.frame(applied).unwrap();
    assert_eq!(fed(&side).taken(), taken + 1, "the steps queued while it was held are one product, taken at once");
    assert_eq!(frame.layers[1].shown, Some(applied), "and the counters show the hour applied");
    assert_eq!(fed(&side).published(), Published::Current);
    assert_eq!(fed(&side).blocked(), blocked);
    fed_equals_composed(&side);
}

#[test]
fn moved_hours_are_read_against_the_mirrors_before_each_step_across_a_pack_and_a_reused_slot() {
    let writing = Writing { restore_blocks: 1, whole_every: 1, pack_margin: 0, ..Writing::at(3) };
    let province = |store: &mut Store, slot: u32, value: u32| write(store, TableId::Divisions, slot, "province", Cell::U32(value));
    let units = record_writing(BLOCK + 120, writing, |_, _| Status::Complete, |at, store| match at {
        0 => {
            for (slot, key) in [(0, 90), (1, 1), (2, 2), (3, 93), (4, 94)] {
                insert(store, TableId::Divisions, slot, key);
                province(store, slot, 10 + slot);
            }
        }
        1 => {
            for slot in [0, 3, 4] {
                store.table(TableId::Divisions.index()).unwrap().delete(slot).unwrap();
            }
        }
        717 => province(store, 1, 21),
        718 => province(store, 2, 22),
        800 => province(store, 0, 31),
        810 => province(store, 1, 32),
        822 => {
            store.table(TableId::Divisions.index()).unwrap().delete(0).unwrap();
            insert(store, TableId::Divisions, 0, 3);
            province(store, 0, 41);
        }
        828 => {
            store.table(TableId::Divisions.index()).unwrap().delete(1).unwrap();
        }
        829 => {
            insert(store, TableId::Divisions, 1, 4);
            province(store, 1, 51);
        }
        _ => {}
    });
    let mut layer = layer_seeing_all();
    let mut inputs = viewer_layer_core::layers::Inputs::new(&units.kinds, "counters");
    layer.inputs(&fog_off(), &mut inputs).unwrap();
    let declared = inputs.into_tables().into_iter().map(|(table, read)| playhead::Declared { table, columns: read.columns, keyed: read.keyed, passing: false, hourly: read.hourly }).collect();
    let playhead = playhead::Playhead::new(Arc::clone(&units.pages), units.timeline.clone(), quick(), None).unwrap();
    let mut consumer = playhead.consumer(playhead.declare(declared, Some(viewer_layer_core::playhead_host::mirror::rebuild(Arc::clone(&units.pages)))).unwrap());
    consumer.set_blocking(true);
    playhead.seek(0);
    let last = BLOCK as i32 + 119;
    let divisions = TableId::Divisions.index();
    let mut steps: Vec<Arc<playhead::Step>> = Vec::new();
    let mut head = 0;
    loop {
        playhead.wait_for(|| {
            consumer.advance(head);
            consumer.take(head, &mut steps);
            playhead.newest().is_some_and(|newest| newest >= head) && steps.iter().rev().find_map(|step| step.table(divisions)).is_some_and(|changes| changes.short() == 0)
        }).unwrap();
        if head == last {
            break;
        }
        head = (head + 4).min(last);
    }
    playhead.wait_for(|| {
        consumer.take(last, &mut steps);
        steps.last().is_some_and(|step| step.last() >= last)
    }).unwrap();
    assert!(steps.iter().any(|step| step.table(divisions).and_then(|changes| changes.renumber()) == Some(&[1, 2][..])), "the block's end packed A and B to slots 0 and 1");
    assert!(
        steps.iter().filter_map(|step| step.table(divisions)).any(|changes| changes.deleted().keys() == Some(&[1][..]) && changes.inserted().keys() == Some(&[3][..])),
        "A's deletion and 3's birth in its slot, written in one hour, are one step"
    );
    let mut mirrors = viewer_layer_core::playhead_host::mirror::Mirrors::default();
    let mut moved = viewer_unit_reads::counter_move::MovedHours::default();
    for step in &steps {
        layer.moved_at(step, &mirrors, &mut moved);
        for changes in step.tables().iter().filter(|changes| changes.reset()) {
            if let Some(Ok(rebuilt)) = consumer.state(step, changes.table()).map(|state| state.downcast::<viewer_layer_core::playhead_host::mirror::Mirror>()) {
                mirrors.insert(rebuilt);
            }
        }
        mirrors.apply(step);
    }
    let moves: Vec<(u64, i32, u32)> = moved.divisions.iter().flat_map(|(&unit, hours)| hours.iter().map(move |&(hour, to)| (unit, hour, to))).collect();
    assert_eq!(moves, [(1, 717, 21), (1, 800, 31), (2, 718, 22), (2, 810, 32)], "each move is its own division's at the hour it moved");
    assert!(moved.task_forces.is_empty());
}

#[test]
fn status_recorded_and_naval_transfer_stay_distinct_from_bar_maxima() {
    let units = record(4, |_, _| Status::Complete, |at, store| {
        if at != 0 {
            return;
        }
        world(store);
        insert(store, TableId::DivisionStatus, 1, 101);
        write(store, TableId::DivisionStatus, 1, "supply_ratio", Cell::OptionalI64(Some(20)));
        insert(store, TableId::DivisionMovement, 0, 101);
        write(store, TableId::DivisionMovement, 0, "naval_transfer", Cell::U8(1));
        insert(store, TableId::DivisionMovement, 1, 102);
        write(store, TableId::DivisionMovement, 1, "naval_transfer", Cell::U8(0));
    });
    let mut side = side_for(units);
    reach(&mut side, 2);

    let (_, with_maxima_no_movement) = member(&side, 100).unwrap();
    assert!(with_maxima_no_movement.bars);
    assert!(with_maxima_no_movement.status_recorded);
    assert_eq!(with_maxima_no_movement.status.naval_transfer, None);

    let (_, without_maxima_at_sea) = member(&side, 101).unwrap();
    assert!(!without_maxima_at_sea.bars);
    assert!(without_maxima_at_sea.status_recorded);
    assert_eq!(without_maxima_at_sea.status.naval_transfer, Some(true));

    let (_, without_status) = member(&side, 102).unwrap();
    assert!(!without_status.bars);
    assert!(!without_status.status_recorded);
    assert_eq!(without_status.status.naval_transfer, Some(false));
}
