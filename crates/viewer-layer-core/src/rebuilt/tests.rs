
use super::{Build, Seen, Whole};
use engine_executable::executable_tables::Tables;
use crate::{layers::{Layer, Published, View}, test_support::recorded, vision, playhead_host::{MapSide, open, units::Units, views::Recording}};
use playhead::{
    Settings, Timeline,
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::Store,
};
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::{DecodedFrame, TableView};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use viewer_foundation::perspective::{Choice, VISION_TABLES, Vision};
use viewer_runtime::presentation::Continuity;

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    moved: Condvar,
}

impl Gate {
    fn shut() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn opened() -> Arc<Self> {
        let gate = Self::shut();
        gate.open();
        gate
    }

    fn open(&self) {
        self.state.lock().unwrap().0 = true;
        self.moved.notify_all();
    }

    fn reached(&self) -> bool {
        self.state.lock().unwrap().1
    }

    fn wait_reached(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.1 {
            state = self.moved.wait(state).unwrap();
        }
    }

    fn close(&self) {
        *self.state.lock().unwrap() = (false, false);
    }

    fn pass(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.moved.notify_all();
        while !state.0 {
            state = self.moved.wait(state).unwrap();
        }
        Ok(())
    }
}

struct Opens(Arc<Gate>);

impl Drop for Opens {
    fn drop(&mut self) {
        self.0.open();
    }
}

struct Probe {
    reads: Vec<TableId>,
    gate: Arc<Gate>,
}

impl Build for Probe {
    type Product = Hour;

    fn tables(&self) -> Option<Tables> {
        None
    }

    fn build(&self, frame: &DecodedFrame, _seen: &Seen) -> io::Result<Hour> {
        self.gate.pass()?;
        for table in &self.reads {
            let _ = std::hint::black_box(&*frame.tables[table.index()]);
        }
        Ok(frame.hour)
    }
}

fn recording_tables() -> Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    let decoded = || engine_executable::executable_tables::decode(engine_executable::build::ExecutableIdentity::of(b"rebuilt test"), Vec::new(), std::path::Path::new("no game install"));
    *TABLES.get_or_init(|| -> Tables { Box::leak(Box::new(decoded())) })
}

struct NeedsTables {
    given: Option<Tables>,
}

impl Build for NeedsTables {
    type Product = Hour;

    fn tables(&self) -> Option<Tables> {
        self.given
    }

    fn build(&self, frame: &DecodedFrame, _seen: &Seen) -> io::Result<Hour> {
        let current = engine_executable::executable_tables::current();
        assert!(current.is_some_and(|current| std::ptr::eq(current, recording_tables())), "trigger and effect registries withheld: no recording's tables are in scope for this work");
        Ok(frame.hour)
    }
}

struct InOrder {
    reads: Vec<TableId>,
}

impl Build for InOrder {
    type Product = Hour;

    fn tables(&self) -> Option<Tables> {
        None
    }

    fn build(&self, frame: &DecodedFrame, _seen: &Seen) -> io::Result<Hour> {
        for table in &self.reads {
            if let TableView::Unavailable { reason } = &*frame.tables[table.index()] {
                return Err(io::Error::other(reason.clone()));
            }
        }
        Ok(frame.hour)
    }
}

fn vision_content() -> Arc<vision::Content> {
    use viewer_foundation::visibility::{FogContent, Topology, test_defines};
    let neighbors = BTreeMap::from([(1, vec![2]), (2, vec![1])]);
    Arc::new(vision::Content::new(Ok(FogContent { topology: Topology { land: BTreeSet::from([1]), sea: BTreeSet::from([2]), neighbors }, defines: test_defines() })))
}

fn record() -> Units {
    recording(40, |_, _| {})
}

fn ticking(hours: u32) -> Units {
    recording(hours, |at, store| {
        if at > 0 {
            store.table(TableId::Clock.index()).unwrap().write(0, recorded::column(TableId::Clock, "speed"), Cell::I32(at as i32 + 1)).unwrap();
        }
    })
}

fn recording(hours: u32, mut every: impl FnMut(u32, &mut Store)) -> Units {
    let mut tables: Vec<TableId> = VISION_TABLES.to_vec();
    tables.push(TableId::Clock);
    tables.sort_unstable();
    tables.dedup();
    static RECORDED: AtomicUsize = AtomicUsize::new(0);
    let name = format!("rebuilt test {}", RECORDED.fetch_add(1, Ordering::Relaxed));
    let strings = |at: u32| if at == 0 { ["AAA", "BBB", "CCC"].map(str::to_owned).to_vec() } else { Vec::new() };
    let pages = crate::test_support::rail_recording::recorded(hours, strings, |at, store: &mut Store| {
        let column = recorded::column;
        if at == 0 {
            for (slot, raw) in [101u32, 102, 103].into_iter().enumerate() {
                let slot = slot as u32;
                store.table(TableId::CountryLinks.index()).unwrap().insert_at(slot, u128::from(raw)).unwrap();
                store.table(TableId::CountryLinks.index()).unwrap().write(slot, column(TableId::CountryLinks, "canonical_slot"), Cell::U32(raw - 100)).unwrap();
                store.table(TableId::Countries.index()).unwrap().insert_at(slot, u128::from(raw)).unwrap();
                store.table(TableId::Countries.index()).unwrap().write(slot, column(TableId::Countries, "canonical_slot"), Cell::U32(raw - 100)).unwrap();
                store.table(TableId::Countries.index()).unwrap().write(slot, column(TableId::Countries, "tag"), Cell::Bytes(slot)).unwrap();
            }
            store.table(TableId::LocalPlayer.index()).unwrap().insert_at(0, 0).unwrap();
            store.table(TableId::LocalPlayer.index()).unwrap().write(0, column(TableId::LocalPlayer, "country"), Cell::U32(101)).unwrap();
            store.table(TableId::Clock.index()).unwrap().insert_at(0, 0).unwrap();
            store.table(TableId::Clock.index()).unwrap().write(0, column(TableId::Clock, "speed"), Cell::I32(1)).unwrap();
        }
        every(at, store);
        for &table in &tables {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
    });
    let kinds = Arc::new((0..TableId::ALL.len()).map(|table| pages.kinds(table).map(<[Kind]>::to_vec).unwrap_or_default()).collect());
    let timeline = Timeline::whole(&*pages).unwrap();
    Units { name, pages, timeline, kinds }
}

fn side_with<B: Build>(units: Units, probes: Vec<(&'static str, B)>) -> MapSide {
    let entry = open(units, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut layers: Vec<Box<dyn Layer>> = vec![Box::new(vision::Vision::new(vision_content()))];
    for (name, probe) in probes {
        layers.push(Box::new(Whole::new(name, probe, |_, _| true, |_| 0, &[TableId::Clock], false).unwrap()));
    }
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    assert!(side.refusals().next().is_none());
    for at in 1..side.parts().unwrap().1.len() {
        layer_of::<B>(&side, at).rebuilt().set_source(Arc::clone(&entry), Recording { branch: 1 }, (Choice::default(), Some(Arc::new(Vision::default())))).unwrap();
    }
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    recorded::frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn side_of(units: Units, reads: Vec<TableId>, gate: &Arc<Gate>) -> MapSide {
    side_with(units, vec![("probe", Probe { reads, gate: Arc::clone(gate) })])
}

fn layer_of<B: Build>(side: &MapSide, at: usize) -> &Whole<B> {
    side.parts().unwrap().1[at].any().downcast_ref::<Whole<B>>().unwrap()
}

fn probe_of(side: &MapSide) -> &Whole<Probe> {
    layer_of(side, 1)
}

fn vision_of(side: &MapSide) -> &vision::Vision {
    side.parts().unwrap().1[0].any().downcast_ref::<vision::Vision>().unwrap()
}

#[test]
fn a_rebuilt_layer_after_vision_settles_while_its_worker_holds_its_build() {
    let gate = Gate::shut();
    let mut side = side_of(record(), Vec::new(), &gate);
    let _opens = Opens(Arc::clone(&gate));
    side.frame(side.applied().unwrap_or(0)).unwrap();
    gate.wait_reached();
    let mut carrying = Vec::new();
    for _ in 0..1_000 {
        side.settle_all().unwrap();
        carrying = side.parts().unwrap().1.iter().filter(|layer| layer.carried().entities > 0).map(|layer| (layer.name(), layer.carried().entities)).collect();
        if carrying.is_empty() {
            break;
        }
    }
    assert!(carrying.is_empty(), "the layers carry {carrying:?} while the probe's worker holds its build");
    assert_eq!(probe_of(&side).published(), Published::Nothing, "before its first product it has published nothing");
    gate.open();
    let hour = side.applied().expect("the head has reached an hour");
    let built = probe_of(&side).rebuilt().wait_settled(hour, 0).unwrap().expect("a built product");
    assert!(built.of(hour), "built for hour {hour}: {:?}", built.hour);
    assert_eq!(built.product.as_ref().ok(), Some(&hour));
    side.settle_all().unwrap();
    assert_eq!(probe_of(&side).published(), Published::Cadence(hour), "once built it shows the hour of its rows");
    assert_eq!(probe_of(&side).carried().entities, 0);
}

#[test]
fn a_table_the_recording_holds_nothing_of_is_refused_by_name_and_never_declared() {
    let gate = Gate::opened();
    let mut side = side_of(recorded::without(record(), TableId::Factions), vec![TableId::Factions], &gate);
    side.settle_all().unwrap();
    let hour = side.applied().expect("the head has reached an hour");
    let built = probe_of(&side).rebuilt().wait_settled(hour, 0).unwrap().expect("a refused product");
    let why = built.product.as_ref().expect_err("the product is refused");
    assert!(why.contains("holds no Factions"), "the refusal names the table: {why}");
    assert!(!probe_of(&side).rebuilt().tables().contains(&TableId::Factions), "the table is never declared");
    assert!(side.entry().playhead().error().is_none(), "the playhead goes on");
    assert_eq!(probe_of(&side).carried().entities, 0);
}

#[test]
fn a_build_runs_under_the_recordings_executable_tables_and_one_that_panics_is_refused_by_name() {
    let mut side = side_with(record(), vec![("scoped", NeedsTables { given: Some(recording_tables()) }), ("unscoped", NeedsTables { given: None })]);
    side.settle_all().unwrap();
    let hour = side.applied().expect("the head has reached an hour");
    let scoped = layer_of::<NeedsTables>(&side, 1).rebuilt().wait_settled(hour, 0).unwrap().expect("a scoped product");
    assert!(scoped.of(hour), "scoped built for hour {hour}: {:?}", scoped.hour);
    assert_eq!(scoped.product.as_ref().ok(), Some(&hour), "the build ran under the recording's tables: {:?}", scoped.product.as_ref().err());
    let unscoped = layer_of::<NeedsTables>(&side, 2).rebuilt();
    let refused = unscoped.wait_settled(hour, 0).unwrap().expect("a panicked product");
    let why = refused.product.as_ref().expect_err("a build that panics is refused");
    assert!(why.contains("panicked") && why.contains("no recording's tables are in scope"), "the refusal names the panic: {why}");
    let builds = unscoped.builds().count;
    let next = next_hour(&mut side);
    let _ = layer_of::<NeedsTables>(&side, 2).rebuilt().wait_settled(next, 0).unwrap();
    side.frame(next).unwrap();
    let unscoped = layer_of::<NeedsTables>(&side, 2).rebuilt();
    assert!(unscoped.builds().count > builds, "the worker kept building after a panic");
    assert!(unscoped.stopped().is_none(), "the worker has not stopped");
    assert!(unscoped.newer(None).expect("a product").product.is_err());
    assert!(side.entry().playhead().error().is_none(), "the playhead goes on");
}

#[test]
fn a_build_that_misses_three_tables_learns_the_three_in_one_round() {
    let reads = vec![TableId::FactionMembers, TableId::WarRelations, TableId::Subjects];
    let mut side = side_with(record(), vec![("in order", InOrder { reads: reads.clone() })]);
    let at = side.applied().unwrap_or(0);
    side.frame(at).unwrap();
    let _ = layer_of::<InOrder>(&side, 1).rebuilt().wait_settled(at, 0).unwrap();
    assert_eq!(layer_of::<InOrder>(&side, 1).rebuilt().builds().discovered, 1, "one round learned the three");
    side.frame(at).unwrap();
    recorded::reads_landed(&side);
    let target = (at + 4).min(39);
    reach(&mut side, target);
    let reached = side.applied().expect("an hour applied");
    let _ = layer_of::<InOrder>(&side, 1).rebuilt().wait_settled(reached, 1).unwrap();
    let rebuilt = layer_of::<InOrder>(&side, 1).rebuilt();
    let tables = rebuilt.tables();
    assert!(reads.iter().all(|table| tables.contains(table)), "every table the build reaches is declared: {tables:?}");
    assert_eq!(rebuilt.builds().discovered, 1, "one round learned the three");
    let built = rebuilt.newer(None).expect("a product");
    assert_eq!(built.product.as_ref().ok(), Some(&built.hour), "once joined the build reads all three: {:?}", built.product.as_ref().err());
}

fn window_side(units: Units, gate: &Arc<Gate>) -> MapSide {
    window_side_with(units, gate, false)
}

fn window_side_with(units: Units, gate: &Arc<Gate>, exact: bool) -> MapSide {
    let entry = open(units, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let probe = Whole::new("probe", Probe { reads: Vec::new(), gate: Arc::clone(gate) }, |_, view| view.camera_height < 500.0, |_| 0, &[TableId::Clock], false).unwrap().in_window(Arc::clone(&entry), Recording { branch: 1 }).unwrap();
    let probe = if exact { probe.exact() } else { probe };
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(vision::Vision::new(vision_content())), Box::new(probe)];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    recorded::frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn window_state(side: &MapSide, gate: &Gate) -> String {
    let probe = side.parts().map(|_| probe_of(side)).map(|probe| {
        let rebuilt = probe.rebuilt();
        format!("published {:?}, carried {:?}, shows {}, product at {:?}, builds {}, tables {:?}, awaiting {:?}, stopped {:?}", probe.published(), probe.carried(), probe.shows(), probe.product().map(|built| built.hour), rebuilt.builds().count, rebuilt.tables(), rebuilt.awaiting(), rebuilt.stopped())
    });
    format!("applied {:?}, newest {:?}, composing {}, gate reached {}; the probe: {probe:?}", side.applied(), side.entry().playhead().newest(), side.composing(), gate.reached())
}

fn reach(side: &mut MapSide, target: Hour) {
    let cap = target.min(side.entry().playhead().timeline().last());
    loop {
        assert!(side.entry().playhead().error().is_none(), "the playhead stopped before hour {target}: {:?}", side.entry().playhead().error());
        recorded::reads_landed(side);
        let before = side.entry().playhead().step_state();
        let head = before.0.unwrap_or(0).min(cap);
        let frame = if side.composing() { side.frame_waiting(head) } else { side.frame(head) }.unwrap();
        if !side.composing() && side.applied().is_some_and(|applied| applied >= target) {
            return;
        }
        if side.composing() || frame.steps > 0 || frame.swapped.is_some() || frame.superseded {
            continue;
        }
        let more = side
            .wait_step_after(before, cap)
            .unwrap_or_else(|error| panic!("the playhead stopped before hour {target}: {error}"));
        assert!(more, "the map side reached hour {:?} (target {target}) without advancing: {}", side.applied(), recorded::seen(side));
    }
}

fn wait_probe_product(side: &mut MapSide) {
    let at = side.applied().expect("the head has reached an hour");
    let _ = if side.composing() { side.frame_waiting(at) } else { side.frame(at) }.unwrap();
    if side.composing() {
        side.frame_waiting(at).unwrap();
    }
    let hour = side.applied().expect("the head has reached an hour");
    let built = probe_of(side).rebuilt().wait_settled(hour, 0).unwrap().expect("a built product");
    assert!(built.of(hour), "probe built for hour {hour}: {:?}", built.hour);
    side.frame(hour).unwrap();
}

#[test]
fn a_window_layer_shows_todays_plan_until_its_first_product_and_nothing_where_the_view_hides_it() {
    let gate = Gate::shut();
    let mut side = window_side(record(), &gate);
    let _opens = Opens(Arc::clone(&gate));
    side.frame(side.applied().unwrap_or(0)).unwrap();
    gate.wait_reached();
    assert!(!probe_of(&side).shows(), "until its first product the window draws today's plan");
    assert!(probe_of(&side).uploaded(), "and the layer hands the scene nothing");
    gate.open();
    let hour = side.applied().expect("the head has reached an hour");
    let built = probe_of(&side).rebuilt().wait_settled(hour, 0).unwrap().expect("a built product");
    assert!(built.of(hour), "built for hour {hour}: {:?}", built.hour);
    assert_eq!(built.product.as_ref().ok(), Some(&hour), "built for the view's perspective, its vision left to the builder");
    side.settle_all().unwrap();
    assert!(probe_of(&side).shows(), "with a product the window draws the layer's: today's makes none");
    assert!(!probe_of(&side).uploaded(), "the product it took is due to the scene");
    assert_eq!(probe_of(&side).refusal(), None);
    side.set_view(View { camera_height: 600.0, ..View::default() }).unwrap();
    let next = hour + 4;
    assert!(next < 40, "the recording holds hour {next}");
    reach(&mut side, next);
    assert!(probe_of(&side).product().is_none(), "hidden, it holds no product");
    assert!(probe_of(&side).shows(), "hidden, it draws nothing, as today's draws nothing there");
    assert!(!probe_of(&side).uploaded(), "an empty plan is due to the scene");
    side.set_view(View::default()).unwrap();
    assert!(!probe_of(&side).shows(), "shown again, today's plan is drawn until the layer's first product");
}

#[test]
fn an_exact_layer_is_the_product_of_the_hour_each_step_applies() {
    let gate = Gate::opened();
    let mut side = window_side_with(record(), &gate, true);
    let _opens = Opens(Arc::clone(&gate));
    wait_probe_product(&mut side);
    let hour = side.applied().expect("the head has reached an hour");
    gate.close();
    let opener = {
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            gate.wait_reached();
            gate.open();
        })
    };
    let next = hour + 4;
    assert!(next < 40, "the recording holds hour {next}");
    reach(&mut side, next);
    opener.join().unwrap();
    let applied = side.applied().unwrap();
    let probe = probe_of(&side);
    assert_eq!(probe.product().map(|built| built.hour), Some(applied), "the product of the hour applied: {}", window_state(&side, &gate));
    assert_eq!(probe.published(), crate::layers::Published::Current, "and the layer stands at it");
}

#[test]
fn a_layer_finishing_its_product_wakes_the_waiting_whole_picture() {
    let gate = Gate::shut();
    let mut side = window_side_with(record(), &gate, true);
    let _opens = Opens(Arc::clone(&gate));
    side.entry().playhead().pause();
    let hour = side.applied().unwrap();
    side.frame(hour).unwrap();
    gate.wait_reached();
    let probe = side.layers_mut()[1].any_mut().unwrap().downcast_mut::<Whole<Probe>>().unwrap();
    probe.behind = true;
    let (waiting, wait) = std::sync::mpsc::channel();
    side.before_whole_wait = Some(Box::new(move || waiting.send(()).unwrap()));
    std::thread::scope(|scope| {
        let gate = Arc::clone(&gate);
        scope.spawn(move || {
            let began = wait.recv();
            gate.open();
            began.unwrap();
        });
        let result = side.catch_up(hour);
        side.before_whole_wait = None;
        result.unwrap();
    });
    assert_eq!(probe_of(&side).product().unwrap().product.as_ref().ok(), Some(&hour));
}

#[test]
fn a_compose_lets_the_product_built_before_it_go() {
    let gate = Gate::opened();
    let mut side = window_side_with(record(), &gate, true);
    let _opens = Opens(Arc::clone(&gate));
    wait_probe_product(&mut side);
    let hour = side.applied().expect("the head has reached an hour");
    let before = probe_of(&side).product().map(|built| built.generation).unwrap();
    side.entry().playhead().seek(hour);
    recorded::frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    wait_probe_product(&mut side);
    let built = probe_of(&side).product().unwrap();
    assert!(built.generation > before, "rebuilt after compose: {}", window_state(&side, &gate));
    assert_eq!(built.hour, hour, "built again at the hour sought: {}", window_state(&side, &gate));
}

#[test]
fn a_viewing_country_change_lets_the_product_made_for_the_country_before_go() {
    use viewer_foundation::perspective::Country;
    let gate = Gate::opened();
    let mut side = window_side(record(), &gate);
    let _opens = Opens(Arc::clone(&gate));
    wait_probe_product(&mut side);
    assert_eq!(vision_of(&side).ours(), Some(1), "recorded country AAA is viewed before the change");
    let before = probe_of(&side).product().map(|built| built.generation).unwrap();
    let chosen = View { perspective: Choice { country: Country::Tag("BBB".into()), ..Choice::default() }, ..View::default() };
    side.set_view(chosen.clone()).unwrap();
    assert!(probe_of(&side).product().is_none(), "the product made for the country before is let go at the change");
    assert!(!probe_of(&side).uploaded(), "and the scene is given nothing in its place");
    wait_probe_product(&mut side);
    assert_eq!(vision_of(&side).ours(), Some(2), "the viewing country changed to BBB");
    let built = probe_of(&side).product().unwrap();
    assert!(built.generation > before, "built after the change: {}", window_state(&side, &gate));
    assert_eq!(built.seen.as_ref().map(|(choice, _)| choice), Some(&chosen.perspective), "for the chosen country");
}

#[test]
fn the_plan_the_scene_takes_after_a_compose_is_taken_anew_however_many_products_were_taken_first() {
    let gate = Gate::opened();
    let mut side = window_side(ticking(200), &gate);
    let _opens = Opens(Arc::clone(&gate));
    wait_probe_product(&mut side);
    assert_eq!(probe_of(&side).installing(), Some(Continuity::Reset), "the seek composed the layer: the scene takes its first plan anew");
    let (hour, taken) = (side.applied().expect("the head has reached an hour"), probe_of(&side).marked()[1]);
    let next = hour + 8;
    assert!(next < 200, "the recording holds hour {next}");
    reach(&mut side, next);
    wait_probe_product(&mut side);
    assert!(probe_of(&side).marked()[1] > taken, "a second product was taken before the scene was given one");
    assert_eq!(
        probe_of(&side).installing(),
        Some(Continuity::Reset),
        "the product replacing one taken anew is taken anew too: the scene has been given neither"
    );
}

struct Steady;

impl Build for Steady {
    type Product = ();

    fn tables(&self) -> Option<Tables> {
        None
    }

    fn build(&self, _frame: &DecodedFrame, _seen: &Seen) -> io::Result<()> {
        Ok(())
    }

    fn stands(&self, _product: &(), _frame: &DecodedFrame, _seen: &Seen) -> bool {
        true
    }
}

fn steady_side(units: Units, reads: &[TableId]) -> MapSide {
    let entry = open(units, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let steady = Whole::new("steady", Steady, |_, _| true, |_| 0, reads, false).unwrap();
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(vision::Vision::new(vision_content())), Box::new(steady)];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    steady_of(&side).rebuilt().set_source(Arc::clone(&entry), Recording { branch: 1 }, (Choice::default(), None)).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    recorded::frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn steady_of(side: &MapSide) -> &Whole<Steady> {
    layer_of(side, 1)
}

fn next_hour(side: &mut MapSide) -> i32 {
    let before = side.applied().expect("the head has reached an hour");
    let cap = (before + 4).min(side.entry().playhead().timeline().last());
    assert!(cap > before, "the recording has no hour past {before}");
    loop {
        assert!(side.entry().playhead().error().is_none(), "the playhead stopped past hour {before}: {:?}", side.entry().playhead().error());
        recorded::reads_landed(side);
        let step_before = side.entry().playhead().step_state();
        let head = step_before.0.unwrap_or(before).min(cap);
        let frame = if side.composing() { side.frame_waiting(head) } else { side.frame(head) }.unwrap();
        if let Some(hour) = frame.applied.filter(|&applied| applied != before) {
            return hour;
        }
        if side.composing() || frame.steps > 0 || frame.swapped.is_some() || frame.superseded {
            continue;
        }
        let more = side
            .wait_step_after(step_before, cap)
            .unwrap_or_else(|error| panic!("the playhead stopped past hour {before}: {error}"));
        assert!(more, "no step past hour {before} was applied up to {cap}: {}", recorded::seen(side));
    }
}

#[test]
fn a_layer_builds_nothing_while_no_table_it_reads_changes_and_its_product_stands_at_the_newer_hours() {
    let mut side = steady_side(ticking(200), &[TableId::LocalPlayer]);
    let mut last = 0;
    for _ in 0..40 {
        last = next_hour(&mut side);
        assert!(steady_of(&side).rebuilt().looked(last, Instant::now() + Duration::from_secs(30)), "the worker never looked at the rows of hour {last}");
    }
    let built = steady_of(&side).rebuilt().wait(last, Duration::from_secs(30)).unwrap();
    let builds = steady_of(&side).rebuilt().builds();
    assert_eq!(builds.count, 1, "the Clock changed every hour and LocalPlayer never: one build, the first ({builds:?})");
    assert!(builds.confirmed >= 39, "the product stood at each newer hour's rows: {builds:?}");
    assert_eq!(built.current(), last, "the product is current at the newest rows");
    side.settle_all().unwrap();
    assert_eq!(steady_of(&side).published(), Published::Cadence(last), "it shows the newest hour its product stands at");
    assert_eq!(steady_of(&side).carried().entities, 0);
}

#[test]
fn a_layer_with_a_step_every_frame_makes_the_product_of_each_steps_rows_and_waits_for_no_clock() {
    let mut side = steady_side(ticking(520), &[TableId::Clock]);
    let first = steady_of(&side).rebuilt().builds().count;
    let steps = 60u64;
    let mut last = 0;
    for _ in 0..steps {
        last = next_hour(&mut side);
        let built = steady_of(&side).rebuilt().wait(last, Duration::from_secs(30)).unwrap();
        assert_eq!(built.hour, last, "made from the rows of the step applied");
    }
    let builds = steady_of(&side).rebuilt().builds();
    assert!(builds.count - first >= steps, "a product for each step's rows: {builds:?}");
    side.settle_all().unwrap();
    assert_eq!(steady_of(&side).published(), Published::Cadence(last));
}
