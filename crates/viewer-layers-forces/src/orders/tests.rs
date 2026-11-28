use super::{Orders, Plans};
use viewer_layer_core::{layers::Layer, test_support::recorded::{self, column}, playhead_host::{open, views::apart::Apart}};
use playhead::Settings;
use replay_columnar::{manifest::Status, state::Cell, store::Store};
use replay_data::{
    rows::{TableId, tables},
    scope::CaptureMode,
};
use replay_frame::frame::DecodedFrame;
use std::{io, sync::Arc, time::Duration};
use viewer_counter_products::map::orders;
use viewer_foundation::perspective::{Choice, Vision};
use viewer_gpu::ribbon_content::OrderField;

const LIMIT: Duration = Duration::from_secs(60);

struct Links;

impl Plans for Links {
    fn build(&self, frame: &DecodedFrame, _ours: u32, _show_allied: bool) -> io::Result<OrderField> {
        let mut plan = OrderField::default();
        plan.report.recorded = frame.complete::<tables::CountryLinks>()?.len();
        Ok(plan)
    }
}

fn link(store: &mut Store, slot: u32, key: u128) {
    let mut links = store.table(TableId::CountryLinks.index()).unwrap();
    links.insert_at(slot, key).unwrap();
    links.write(slot, column(TableId::CountryLinks, "canonical_slot"), Cell::U32(slot)).unwrap();
}

fn campaign(at: u32, store: &mut Store) {
    for table in orders::memo_tables(false) {
        store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
    }
    match at {
        0 => link(store, 0, 7),
        45 => link(store, 1, 9),
        _ => {}
    }
}

#[test]
fn retiring_a_map_interrupts_an_orders_join_whose_plan_has_not_arrived() {
    use std::sync::mpsc;
    use viewer_layer_core::layers::Settling;

    let (_entry, mut fed, mut layer, plans) = gated("retiring orders", busy);
    plans.set(false);
    assert!(fed.drive(1));
    let retire = layer.retirement().unwrap();
    let (waiting, at_wait) = mpsc::channel();
    layer.shared.lock().on_wait = Some(Box::new(move || waiting.send(()).unwrap()));

    let outcome = std::thread::scope(|scope| {
        let join = scope.spawn(|| layer.settle_at(1, Settling::Whole));
        let waited = at_wait.recv_timeout(LIMIT);
        retire();
        let outcome = join.join().unwrap();
        plans.set(true);
        waited.expect("the join waits for a plan once the rows arrive");
        outcome
    });

    assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::Interrupted);
}

#[test]
fn retiring_after_an_orders_join_yields_wakes_the_map_and_interrupts_the_retry() {
    use std::sync::mpsc;
    use viewer_layer_core::layers::Settling;

    let (_entry, fed, mut layer, _plans) = gated("retiring orders with rows owed", busy);
    let (progress, woke) = mpsc::channel();
    fed.store().handle().set_on_publish(Arc::new(move || { let _ = progress.send(()); }));
    layer.settle_at(1, Settling::Whole).unwrap();
    assert!(!layer.made_at(1), "the join yields until its rows arrive");
    layer.retirement().unwrap()();
    assert!(woke.try_recv().is_ok(), "retirement wakes a map waiting for publication");
    assert_eq!(layer.settle_at(1, Settling::Whole).unwrap_err().kind(), io::ErrorKind::Interrupted);
}

#[test]
fn live_playback_keeps_the_last_plan_while_new_orders_rows_are_missing() {
    use viewer_layer_core::layers::Settling;

    let entry = open(recorded::record("short orders", 2, campaign), Settings::default()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Links));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(3, [])));
    entry.playhead().seek(0);
    assert!(fed.drive(0));
    layer.settle_at(0, Settling::Whole).unwrap();
    assert_eq!(layer.made_for(), Some(0));
    layer.shared.lock().on_wait = Some(Box::new(|| panic!("a live frame waits only after the orders rows arrive")));

    layer.settle_at(1, Settling::still()).unwrap();
    assert_eq!(layer.made_for(), Some(0));
}

#[test]
fn a_plan_is_kept_while_its_rows_are_unchanged_and_made_again_from_the_hour_one_changed() {
    let entry = open(recorded::record("orders", 60, campaign), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Links));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(3, [])));
    entry.playhead().seek(0);
    let through = |fed: &mut Apart, layer: &mut Orders, to: i32| loop {
        let head = entry.playhead().newest().unwrap_or(0);
        assert!(fed.drive(head), "the store never reached hour {head}");
        layer.settle_at(head, viewer_layer_core::layers::Settling::Whole).unwrap();
        if head >= to {
            break;
        }
    };

    through(&mut fed, &mut layer, 36);
    let (hour, plan) = layer.plan().expect("a plan from the first rows");
    assert_eq!((hour, plan.report.recorded), (0, 1));
    assert_eq!(layer.marked()[..2], [1, 0], "one plan while the rows are unchanged, no table learned");

    through(&mut fed, &mut layer, 55);
    let (hour, plan) = layer.plan().unwrap();
    assert_eq!(plan.report.recorded, 2);
    assert!((45..50).contains(&hour), "made again from the rows of the step the table changed in, at {hour}");
    assert_eq!(layer.marked()[0], 2);
}

struct Reaching;

impl Plans for Reaching {
    fn build(&self, frame: &DecodedFrame, ours: u32, show_allied: bool) -> io::Result<OrderField> {
        let mut plan = Links.build(frame, ours, show_allied)?;
        if plan.report.recorded == 2 {
            plan.report.drawn = frame.complete::<tables::Countries>()?.len();
        }
        Ok(plan)
    }
}

fn campaign_with_countries(at: u32, store: &mut Store) {
    campaign(at, store);
    store.status(TableId::Countries.index(), Status::Complete, true, Some(at as i32), true).unwrap();
}

#[test]
fn a_whole_settle_that_learns_a_table_shows_the_plan_of_its_hour_made_once_the_table_joined() {
    let entry = open(recorded::record("orders learning", 60, campaign_with_countries), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Reaching));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(3, [])));
    entry.playhead().seek(0);
    let through = |fed: &mut Apart, layer: &mut Orders, to: i32| loop {
        let head = entry.playhead().newest().unwrap_or(0);
        let limit = std::time::Instant::now() + LIMIT;
        loop {
            assert!(fed.drive(head), "the store never reached hour {head}");
            layer.settle_at(head, viewer_layer_core::layers::Settling::step()).unwrap();
            if layer.made_for() == Some(head) {
                break;
            }
            assert!(std::time::Instant::now() < limit, "the plan never stood at hour {head}: it stands at {:?}", layer.made_for());
        }
        if head >= to {
            break head;
        }
    };

    through(&mut fed, &mut layer, 36);
    assert_eq!(layer.marked()[1], 0, "no table learned while one link is recorded");

    let head = through(&mut fed, &mut layer, 45);
    let (hour, plan) = layer.plan().expect("a plan");
    assert_eq!(layer.marked()[1], 1, "the countries learned once, at the hour the second link came");
    assert_eq!(layer.made_for(), Some(head), "the plan stands for the rows at the hour settled, not for rows before the table was learned");
    assert!((45..=head).contains(&hour), "made from rows after the second link, at {hour}, settled at {head}");
    assert_eq!((plan.report.recorded, plan.report.drawn), (2, 0), "both links, and the countries read");
}

fn ordering(at: u32, store: &mut Store) {
    for table in orders::memo_tables(false) {
        store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
    }
    let write = |store: &mut Store, table: TableId, field: &str, cell: Cell<'_>| store.table(table.index()).unwrap().write(0, column(table, field), cell).unwrap();
    match at {
        0 => {
            link(store, 0, 7);
            store.table(TableId::LandTheatres.index()).unwrap().insert_at(0, 5).unwrap();
            for (field, cell) in [("owner", Cell::U32(0)), ("ordinal", Cell::U32(0)), ("volunteers_theatre", Cell::I32(0))] {
                write(store, TableId::LandTheatres, field, cell);
            }
            store.table(TableId::OrdersGroups.index()).unwrap().insert_at(0, 77).unwrap();
            let group = [
                ("country", Cell::U32(0)),
                ("is_army_group", Cell::U8(0)),
                ("motorization_level", Cell::U8(0)),
                ("theatre", Cell::U64(5)),
                ("theatre_ordinal", Cell::OptionalU32(Some(0))),
                ("theater_group", Cell::U64(0)),
                ("parent", Cell::U64(0)),
                ("leader", Cell::U64(0)),
                ("hq_provider", Cell::U64(0)),
            ];
            for (field, cell) in group {
                write(store, TableId::OrdersGroups, field, cell);
            }
            let name = store.intern(b"Army 1").unwrap();
            store.table(TableId::OrdersGroupPlans.index()).unwrap().insert_at(0, 77).unwrap();
            let plan = [
                ("red", Cell::U32(255)),
                ("green", Cell::U32(0)),
                ("blue", Cell::U32(0)),
                ("alpha", Cell::U32(255)),
                ("primary_orders", Cell::U32List(&[])),
                ("fallback_orders", Cell::U32List(&[])),
                ("name", Cell::Bytes(name)),
            ];
            for (field, cell) in plan {
                write(store, TableId::OrdersGroupPlans, field, cell);
            }
        }
        20 => {
            store.table(TableId::ArmyOrders.index()).unwrap().insert_at(0, 77 << 32 | 1).unwrap();
            let order = [
                ("country", Cell::U32(0)),
                ("order_type", Cell::I32(0)),
                ("active", Cell::U8(0)),
                ("paradrop_target", Cell::U32(0)),
                ("path", Cell::U32List(&[])),
                ("target_states", Cell::U32List(&[])),
            ];
            for (field, cell) in order {
                write(store, TableId::ArmyOrders, field, cell);
            }
        }
        _ => {}
    }
}

#[test]
fn the_battle_plans_notes_are_stated_from_the_rows_the_viewing_country_holds_an_order_in() {
    let entry = open(recorded::record("orders notes", 40, ordering), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Links));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(0, [])));
    entry.playhead().seek(0);
    let through = |fed: &mut Apart, layer: &mut Orders, to: i32| loop {
        let head = entry.playhead().newest().unwrap_or(0);
        assert!(fed.drive(head), "the store never reached hour {head}");
        layer.settle_at(head, viewer_layer_core::layers::Settling::Whole).unwrap();
        if head >= to {
            break;
        }
    };

    through(&mut fed, &mut layer, 12);
    assert!(layer.plan().is_some_and(|(hour, _)| hour < 20), "a plan from rows before the order");
    assert!(layer.notes().is_empty(), "no order of the viewing country, no note: {:?}", layer.notes());

    through(&mut fed, &mut layer, 30);
    let (hour, _) = layer.plan().unwrap();
    assert!(hour >= 20, "a plan from rows holding the order, at {hour}");
    let mut today = viewer_foundation::tables::Limitations::default();
    today.note_battle_plan(1);
    assert_eq!(layer.notes(), today.into_lines(), "today's two lines for the one order drawn");
    assert_eq!(layer.notes().len(), 2);
}

#[test]
fn after_a_seek_a_driven_settle_shows_the_plan_of_the_sought_hours_rows() {
    let entry = open(recorded::record("orders driven past the seek", 40, ordering), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Links));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(0, [])));
    entry.playhead().seek(0);
    let through = |fed: &mut Apart, layer: &mut Orders, to: i32| loop {
        let head = entry.playhead().newest().unwrap_or(0);
        assert!(fed.drive(head), "the store never reached hour {head}");
        layer.settle_at(head, viewer_layer_core::layers::Settling::Whole).unwrap();
        if head >= to {
            break;
        }
    };
    through(&mut fed, &mut layer, 30);
    let old = layer.plan().unwrap().0;
    assert!(old >= 20, "a plan from the rows holding the order, at {old}");

    entry.playhead().seek(4);
    layer.compose(&viewer_layer_core::playhead_host::mirror::Mirrors::default()).unwrap();
    assert!(fed.drive(4), "the store never reached the hour sought");
    layer.settle_at(4, viewer_layer_core::layers::Settling::Whole).unwrap();
    assert_eq!(layer.plan().unwrap().0, 4, "the plan of the rows at the sought hour");
}

#[test]
fn after_a_seek_back_the_plan_shown_at_the_sought_hour_is_made_from_its_rows() {
    let entry = open(recorded::record("orders after a seek back", 40, ordering), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let mut layer = Orders::new(Arc::new(Links));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(0, [])));
    entry.playhead().seek(0);
    assert!(fed.drive(30), "the store never reached hour 30");
    layer.settle_at(30, viewer_layer_core::layers::Settling::Whole).unwrap();
    assert!(layer.plan().is_some(), "a plan before the seek");

    let until = std::time::Instant::now() + LIMIT;
    entry.playhead().seek(4);
    layer.compose(&viewer_layer_core::playhead_host::mirror::Mirrors::default()).unwrap();
    while entry.playhead().newest().is_none_or(|newest| newest <= 4) {
        assert!(std::time::Instant::now() < until, "the steps past the sought hour were never built");
        std::thread::yield_now();
    }
    assert!(fed.drive(4), "the store never reached the hour sought");
    layer.settle_at(4, viewer_layer_core::layers::Settling::Whole).unwrap();
    assert_eq!(layer.plan().unwrap().0, 4, "the plan shown at the sought hour is made from the rows at it");
}

fn label(key: &str) -> viewer_runtime::name_draw::WorldName {
    viewer_runtime::name_draw::WorldName {
        key: key.to_owned(),
        strategic_provinces: None,
        text: "118 Divs - Army Group 2".to_owned(),
        pages: vec![2, 2],
        corners: vec![[3399.9587, 10.140796, 1761.204], [3400.3167, 9.99884, 1760.4884]],
        uvs: vec![[0.0, 0.0, 0.5, 0.5], [0.5, 0.5, 1.0, 1.0]],
        colors: vec![[1.0; 4], [1.0; 4]],
        aabb: [3398.0, 1759.0, 3401.0, 1762.0],
        wrapped: false,
        font_size: 1.0,
        spaces: 0,
        alpha: 1.0,
    }
}

#[test]
fn two_plans_labels_differ_by_content_never_by_their_upload_generation() {
    use super::differ_labels;
    use viewer_runtime::name_draw::{Placements, next_generation};
    let placements = |country| Placements { country, warnings: Vec::new(), generation: next_generation(), near: None };
    let today = placements(vec![label("1:1"), label("1:2")]);
    let layer = placements(vec![label("1:1"), label("1:2")]);
    assert_ne!(format!("{today:?}"), format!("{layer:?}"));
    assert_eq!(differ_labels(&today, &layer), None);

    let mut moved = label("1:2");
    moved.corners[1][2] = 1760.5;
    let layer = placements(vec![label("1:1"), moved]);
    assert_eq!(differ_labels(&today, &layer).as_deref(), Some(r#"label "1:2" (at 1), corners[1]: today [3400.3167, 9.99884, 1760.4884], the layer [3400.3167, 9.99884, 1760.5]"#));

    let layer = placements(vec![label("1:2"), label("1:1")]);
    assert_eq!(differ_labels(&today, &layer).as_deref(), Some(r#"label 0: today "1:1", the layer "1:2""#));

    let layer = placements(vec![label("1:1")]);
    assert_eq!(differ_labels(&today, &layer).as_deref(), Some(r#"2 labels today, 1 the layer; the first unmatched today Some("1:2"), the layer None"#));
}

#[derive(Default)]
struct Gate {
    open: bool,
    begun: usize,
    builds: usize,
}

#[derive(Default)]
struct Gated {
    gate: std::sync::Mutex<Gate>,
    changed: std::sync::Condvar,
}

impl Gated {
    fn gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set(&self, open: bool) {
        self.gate().open = open;
        self.changed.notify_all();
    }

    fn begun(&self) -> usize {
        self.gate().begun
    }

    fn builds(&self) -> usize {
        self.gate().builds
    }

    fn begun_past(&self, begun: usize, what: &str) {
        let mut gate = self.gate();
        while gate.begun <= begun {
            let (held, waited) = self.changed.wait_timeout(gate, LIMIT).unwrap_or_else(std::sync::PoisonError::into_inner);
            gate = held;
            assert!(!waited.timed_out() || gate.begun > begun, "{what}");
        }
    }
}

impl Plans for Gated {
    fn build(&self, frame: &DecodedFrame, ours: u32, show_allied: bool) -> io::Result<OrderField> {
        let mut gate = self.gate();
        gate.begun += 1;
        self.changed.notify_all();
        while !gate.open {
            let (held, waited) = self.changed.wait_timeout(gate, LIMIT).unwrap_or_else(std::sync::PoisonError::into_inner);
            gate = held;
            if waited.timed_out() {
                break;
            }
        }
        gate.builds += 1;
        drop(gate);
        self.changed.notify_all();
        Links.build(frame, ours, show_allied)
    }
}

fn busy(at: u32, store: &mut Store) {
    for table in orders::memo_tables(false) {
        store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
    }
    link(store, at, u128::from(at) + 7);
}

fn gated(name: &str, written: fn(u32, &mut Store)) -> (Arc<viewer_layer_core::playhead_host::Entry>, Apart, Orders, Arc<Gated>) {
    let entry = open(recorded::record(name, 60, written), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let fed = Apart::new(&entry, 0).unwrap();
    let plans = Arc::new(Gated::default());
    plans.set(true);
    let layer = Orders::new(Arc::clone(&plans) as Arc<dyn Plans>);
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(3, [])));
    let mut layer = layer;
    layer.take_vision();
    entry.playhead().seek(0);
    (entry, fed, layer, plans)
}

fn live(fed: &mut Apart, layer: &mut Orders, hour: i32) {
    assert!(fed.drive(hour), "the store never reached hour {hour}");
    layer.head = Some(hour);
    layer.take();
}

fn stands_at(fed: &mut Apart, layer: &mut Orders, hour: i32) {
    let limit = std::time::Instant::now() + LIMIT;
    while layer.made_for() != Some(hour) {
        assert!(std::time::Instant::now() < limit, "the plan never stood at hour {hour}: it stands at {:?}", layer.made_for());
        live(fed, layer, hour);
        std::thread::yield_now();
    }
}

#[test]
fn playing_the_plan_is_current_while_its_tables_are_unchanged_and_a_build_on_the_worker_shows_the_hour_before() {
    use viewer_layer_core::layers::Published;
    let (_entry, mut fed, mut layer, plans) = gated("orders playing", campaign);
    stands_at(&mut fed, &mut layer, 0);
    assert_eq!(layer.published(), Published::Current, "the first plan made");
    for hour in 1..45 {
        stands_at(&mut fed, &mut layer, hour);
        assert_eq!((layer.published(), layer.carried().entities), (Published::Current, 0), "hour {hour}: unchanged tables leave the plan current, nothing carried");
    }
    let begun = plans.begun();
    plans.set(false);
    live(&mut fed, &mut layer, 45);
    plans.begun_past(begun, "the change at hour 45 began no build");
    live(&mut fed, &mut layer, 45);
    assert_eq!(layer.published(), Published::Cadence(44), "a build of hour 45 is on the worker: the plan shown stands through 44");
    assert_eq!(layer.plan().map(|(hour, plan)| (hour, plan.report.recorded)), Some((0, 1)), "the plan drawn is the one its rows were last made for");
    assert_eq!(layer.carried().entities, 0, "the worker's build is not the frame thread's work");
    plans.set(true);
    stands_at(&mut fed, &mut layer, 45);
    assert_eq!(layer.published(), Published::Current, "current again");
    for hour in 46..50 {
        stands_at(&mut fed, &mut layer, hour);
        assert_eq!((layer.published(), layer.carried().entities), (Published::Current, 0), "hour {hour}: current again");
    }
    assert_eq!(layer.plan().map(|(hour, plan)| (hour, plan.report.recorded)), Some((45, 2)));
}

#[test]
fn steps_faster_than_a_build_keep_the_plan_finished_shown_at_its_own_hour() {
    let (_entry, mut fed, mut layer, plans) = gated("orders behind a build", busy);
    stands_at(&mut fed, &mut layer, 0);
    let first = layer.made_for().expect("a first plan");
    plans.set(false);
    let mut stood = first;
    for hour in 1..40 {
        live(&mut fed, &mut layer, hour);
        let now = layer.made_for().unwrap_or_else(|| panic!("hour {hour}: the plan finished for hour {first} was not shown while a build was in flight"));
        assert!(now >= stood, "hour {hour}: the hour the plan stands at went back from {stood} to {now}");
        assert!(layer.plan().is_some(), "hour {hour}: the plan finished is still drawn");
        stood = now;
    }
    plans.set(true);
    stands_at(&mut fed, &mut layer, 39);
    assert!(plans.builds() < 39, "it built every step it was behind by instead of the newest rows: {} builds", plans.builds());
    let (hour, plan) = layer.plan().expect("a plan");
    assert_eq!((hour, plan.report.recorded), (39, 40), "the plan of the rows at the hour it caught up to, holding a link an hour");
}

#[test]
fn a_plan_of_rows_behind_the_hour_applied_says_their_hour_and_never_current() {
    use viewer_layer_core::layers::Published;
    let (_entry, mut fed, mut layer, _plans) = gated("orders cadence", campaign);
    stands_at(&mut fed, &mut layer, 0);
    layer.head = Some(3);
    layer.settle_at(3, viewer_layer_core::layers::Settling::step()).unwrap();
    assert_eq!(layer.published(), Published::Cadence(0), "the map applied hour 3, the plan's rows stand at 0");
    assert!(layer.plan().is_some());
    assert!(!layer.made_at(3));
    stands_at(&mut fed, &mut layer, 3);
    assert_eq!(layer.published(), Published::Current, "its rows stand at the hour applied");
}

#[test]
fn a_late_row_declaration_does_not_leave_the_old_plan_made_at_its_hour() {
    let (_entry, mut fed, mut layer, _plans) = gated("orders late declaration", campaign);
    stands_at(&mut fed, &mut layer, 0);
    assert!(layer.made_at(0));
    let reader = layer.rows.lock().unwrap().as_ref().cloned().expect("orders reader");
    reader.declare_only(&[TableId::Clock]);
    assert!(!layer.made_at(0), "clearing the reader frame invalidates the old plan");
}

#[test]
fn a_compose_shows_the_plan_before_it_and_counts_none_until_one_is_made_after_the_reset() {
    use viewer_layer_core::layers::Published;
    let (_entry, mut fed, mut layer, _plans) = gated("orders composed", campaign);
    stands_at(&mut fed, &mut layer, 0);
    layer.compose(&viewer_layer_core::playhead_host::mirror::Mirrors::default()).unwrap();
    layer.take();
    assert_eq!(layer.published(), Published::Nothing, "no plan of this picture yet");
    assert!(layer.plan().is_some(), "the plan of the picture before is still drawn");
    stands_at(&mut fed, &mut layer, 0);
    assert_eq!(layer.published(), Published::Current);
}

#[derive(Default)]
struct HeldForFour {
    gate: std::sync::Mutex<(bool, usize)>,
    changed: std::sync::Condvar,
}

impl HeldForFour {
    fn open(&self) {
        self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0 = true;
        self.changed.notify_all();
    }

    fn begun(&self) -> usize {
        self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1
    }

    fn wait_begun(&self) {
        let gate = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (gate, waited) = self.changed.wait_timeout_while(gate, LIMIT, |gate| gate.1 == 0).unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!waited.timed_out() || gate.1 > 0, "slot 4's build did not start within {LIMIT:?}");
    }
}

impl Plans for HeldForFour {
    fn build(&self, frame: &DecodedFrame, ours: u32, _show_allied: bool) -> io::Result<OrderField> {
        if ours == 4 {
            let mut gate = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            gate.1 += 1;
            self.changed.notify_all();
            while !gate.0 {
                gate = self.changed.wait(gate).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        let mut plan = OrderField::default();
        plan.report.recorded = ours as usize;
        plan.report.drawn = frame.complete::<tables::CountryLinks>()?.len();
        Ok(plan)
    }
}

struct OpenHeldForFour(Arc<HeldForFour>);

impl Drop for OpenHeldForFour {
    fn drop(&mut self) {
        self.0.open();
    }
}

#[test]
fn a_plan_made_for_another_viewing_country_is_withheld_until_its_own_is_made() {
    use viewer_foundation::perspective::Country;
    const BEFORE: i32 = 0;
    const AFTER: i32 = 50;
    let entry = open(recorded::record("orders viewer", 60, campaign), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut fed = Apart::new(&entry, 0).unwrap();
    let held = Arc::new(HeldForFour::default());
    let mut layer = Orders::new(Arc::clone(&held) as Arc<dyn Plans>);
    let _release = OpenHeldForFour(Arc::clone(&held));
    layer.set_views(fed.store()).unwrap();
    layer.set_vision(Choice::default(), Arc::new(Vision::sharing(3, [])));
    entry.playhead().seek(0);
    let settle = |fed: &mut Apart, layer: &mut Orders, head: i32| {
        assert!(fed.drive(head), "the store never reached hour {head}");
        layer.settle_at(head, viewer_layer_core::layers::Settling::step()).unwrap();
    };
    let made_for = |layer: &Orders| layer.plan().map(|(_, plan)| (plan.report.recorded, plan.report.drawn));
    let deadline = std::time::Instant::now() + LIMIT;
    while made_for(&layer).is_none() {
        settle(&mut fed, &mut layer, BEFORE);
        assert!(std::time::Instant::now() < deadline, "slot 3's plan within {LIMIT:?}");
    }
    assert_eq!(made_for(&layer), Some((3, 1)));
    layer.set_vision(Choice { country: Country::Tag("other".into()), ..Choice::default() }, Arc::new(Vision::sharing(4, [])));
    settle(&mut fed, &mut layer, BEFORE);
    assert_eq!(made_for(&layer), None, "slot 3's plan shown while slot 4's is made");
    held.wait_begun();
    assert!(fed.drive(AFTER), "the store never reached hour {AFTER}");
    for _ in 0..50 {
        settle(&mut fed, &mut layer, AFTER);
        assert_eq!(made_for(&layer), None, "slot 3's plan shown while slot 4's is made");
    }
    held.open();
    while made_for(&layer) != Some((4, 2)) {
        settle(&mut fed, &mut layer, AFTER);
        assert!(std::time::Instant::now() < deadline, "slot 4's post-45 plan within {LIMIT:?}: {:?}", made_for(&layer));
    }
    assert_eq!(made_for(&layer), Some((4, 2)));
    assert!(held.begun() >= 2, "slot 4 rebuilt for the hour-45 change: {} builds", held.begun());
}
