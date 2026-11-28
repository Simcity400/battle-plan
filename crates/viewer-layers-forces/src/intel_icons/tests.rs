use super::{Evaluate, Icons, drawn};
use viewer_layer_core::{layers::{Inputs, Layer, View}, test_support::sound_recording::{complete, put, quick, record, sought, units}, playhead_host::{MapSide, open, views::{Pace, Recording}}};
use replay_columnar::{state::Cell, store::Store};
use replay_data::rows::{TableId, tables};
use replay_frame::frame::DecodedFrame;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use viewer_foundation::perspective;
use viewer_raids::map::intel_icons::IntelIcons;
use viewer_map_modes::map_mode::{MapMode, Stock};

const RECORDING: Recording = Recording { branch: 3 };
const HOURS: u32 = 400;
const LAST: replay_data::Hour = HOURS as replay_data::Hour - 1;

struct Count;

impl Evaluate for Count {
    fn icons(&self, frame: &DecodedFrame, _choice: &perspective::Choice) -> IntelIcons {
        let links = frame.complete::<tables::CountryLinks>().map(<[_]>::len).ok();
        let humans = if frame.hour >= 2 { frame.complete::<tables::HumanControllers>().map(<[_]>::len).ok() } else { None };
        let mut icons = IntelIcons { built_for: Some((frame.branch, frame.hour, 0)), ..IntelIcons::default() };
        icons.report.reasons.insert(format!("links {links:?}, humans {humans:?}, on {:?}", std::thread::current().name()), 1);
        icons
    }
}

fn campaign(at: u32, store: &mut Store) {
    complete(store, at, &[TableId::Countries, TableId::CountryLinks, TableId::Provinces, TableId::States, TableId::HumanControllers]);
    match at {
        0 => {
            put(store, TableId::CountryLinks, 0, Some(9), &[("canonical_slot", Cell::U32(18))]);
            put(store, TableId::CountryLinks, 1, Some(2), &[("canonical_slot", Cell::U32(4))]);
            put(store, TableId::HumanControllers, 0, Some(9), &[("count", Cell::I8(1))]);
        }
        300 => put(store, TableId::CountryLinks, 2, Some(5), &[("canonical_slot", Cell::U32(10))]),
        _ => {}
    }
}

fn operatives() -> View {
    View { map_mode: MapMode::Stock(Stock::Operatives), ..View::default() }
}

fn layer(side: &MapSide) -> &Icons {
    side.parts().unwrap().1[0].any().downcast_ref::<Icons>().unwrap()
}

fn side(name: &str) -> MapSide {
    let entry = open(units(record(HOURS, campaign), name), playhead::Settings { speed: 1.0, ..quick() }).unwrap();
    let icons = Icons::new(Arc::new(Count), Pace::Ready);
    let mut side = MapSide::new(entry, vec![Box::new(icons)], operatives()).unwrap();
    side.set_branch(RECORDING.branch);
    side
}

fn state(side: &MapSide) -> String {
    let layer = layer(side);
    let open = layer.open.as_ref().map(|open| (open.busy, open.handed, open.seen.as_ref().map(|(seen, _)| seen.hour), open.reader.frame().map(|frame| frame.hour)));
    format!("icons {:?}, applied {:?}, open (busy, handed, seen, rows, error) {open:?}, tables {:?}, {:?}", layer.icons().map(|(_, hour)| hour), layer.applied, layer.tables(), layer.stats())
}

fn at(side: &mut MapSide, hour: replay_data::Hour, what: &str, mut done: impl FnMut(&mut MapSide) -> bool) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        side.frame(hour).unwrap();
        if !side.composing() && done(side) {
            return;
        }
        assert!(Instant::now() < until, "{what}: never; {}", if side.composing() { side.waiting_state(hour) } else { state(side) });
        std::thread::yield_now();
    }
}

fn icons_at(side: &mut MapSide, hour: replay_data::Hour, what: &str) {
    sought(side, hour);
    settled_icons(side, hour, what);
}

fn settled_icons(side: &mut MapSide, hour: replay_data::Hour, what: &str) {
    at(side, hour, what, |side| {
        side.settle_all().unwrap();
        layer(side).icons().is_some_and(|(_, at)| at == hour)
    });
}

fn learned_at(side: &mut MapSide, hour: replay_data::Hour) {
    icons_at(side, 0, "the icons before learning another table");
    let mut held = drawn(side.layer_parts()).unwrap();
    assert!(!layer(side).tables().contains(&TableId::HumanControllers));
    at(side, hour, "the worker learns the table its content reads", |side| {
        let icons = drawn(side.layer_parts()).expect("while a new table loads, the window keeps its icons");
        let learned = layer(side).tables().contains(&TableId::HumanControllers);
        if learned {
            assert!(Arc::ptr_eq(&held, &icons));
        }
        held = icons;
        learned
    });
    settled_icons(side, hour, "the icons of the hour");
}

fn play(side: &mut MapSide, from: replay_data::Hour, what: &str, mut each: impl FnMut(&MapSide, &viewer_layer_core::playhead_host::Frame) -> bool) {
    side.entry().playhead().play(4.0);
    let mut applied = from;
    for _ in 0..50_000_000u64 {
        assert!(applied < LAST, "{what}: the recording ended first; {}", state(side));
        let newest = side.entry().playhead().newest().unwrap_or(applied);
        let frame = side.frame(newest.min(applied + 4)).unwrap();
        if let Some(hour) = frame.applied.filter(|_| frame.steps > 0) {
            applied = hour;
            side.settle_all().unwrap();
            if each(side, &frame) {
                return;
            }
        } else {
            std::thread::yield_now();
        }
    }
    panic!("{what}: play never reached what the test waits for (applied {applied})");
}

fn read(side: &MapSide) -> Option<(String, i32)> {
    let (icons, hour) = layer(side).icons()?;
    Some((icons.report.reasons.keys().next()?.clone(), hour))
}

#[test]
fn the_icons_are_evaluated_over_rows_at_the_head_on_their_worker_learning_the_tables_their_scripts_read() {
    let mut side = side("intel icons at the head");
    learned_at(&mut side, 2);
    assert_eq!(read(&side).unwrap().0, "links Some(2), humans Some(1), on Some(\"intel icons\")");
    assert_eq!(drawn(side.layer_parts()).unwrap().built_for, Some((RECORDING.branch, 2, 0)));
    let mut seen = Vec::new();
    play(&mut side, 2, "the icons at the head", |side, frame| {
        let applied = frame.applied.unwrap();
        if let Some((text, hour)) = read(side) {
            assert_eq!(hour, applied, "an unlimited settle gives the icons of the hour applied");
            assert_eq!(drawn(side.layer_parts()).unwrap().built_for, Some((RECORDING.branch, applied, 0)));
            seen.push((applied, text));
        }
        applied >= 340
    });
    assert!(seen.iter().filter(|(hour, _)| *hour < 300).all(|(_, text)| text == "links Some(2), humans Some(1), on Some(\"intel icons\")"));
    let after = seen.last().unwrap();
    assert!(after.0 >= 340);
    assert_eq!(after.1, "links Some(3), humans Some(1), on Some(\"intel icons\")");
    assert!(layer(&side).tables().contains(&TableId::HumanControllers), "learned: {:?}", layer(&side).tables());
    let stats = layer(&side).stats();
    assert!(stats.learning >= 1, "the evaluation that read the table undeclared was discarded: {stats:?}");
}

#[test]
fn in_another_map_mode_it_declares_nothing_and_holds_nothing_and_opens_again_with_the_tables_it_learned() {
    let mut side = side("intel icons closed");
    learned_at(&mut side, 2);
    let learned = layer(&side).stats().learning;
    side.set_view(View::default()).unwrap();
    let mut declared = Inputs::new(side.entry().kinds(), "intel icons");
    let mut probe = Icons::new(Arc::new(Count), Pace::PerSecond(4));
    probe.inputs(&View::default(), &mut declared).unwrap();
    assert!(declared.into_tables().is_empty(), "another mode declares no table");
    assert!(!layer(&side).is_open(), "no reader and no worker");
    assert!(layer(&side).icons().is_none() && drawn(side.layer_parts()).is_none(), "no icons");
    assert!(layer(&side).held() <= 64 * std::mem::size_of::<TableId>(), "only the names of the tables it learned: {}", layer(&side).held());
    side.set_view(operatives()).unwrap();
    at(&mut side, 2, "the reader opens again", |side| layer(side).is_open());
    icons_at(&mut side, 40, "the icons opened again");
    let stats = layer(&side).stats();
    assert_eq!(stats.learning, learned, "opened with the tables it learned, nothing is discarded");
    assert_eq!((stats.opened, stats.closed), (2, 1));
    assert!(stats.loading >= 1);
}

#[test]
fn a_whole_settle_at_each_step_gets_the_icons_of_each_hour_with_no_one_else_marking_the_reader_ready() {
    let mut side = side("intel icons stepped whole");
    learned_at(&mut side, 2);
    let mut steps = 0;
    play(&mut side, 2, "the icons of each hour applied", |side, frame| {
        assert_eq!(layer(side).icons().map(|(_, hour)| hour), frame.applied, "the icons of the hour applied: {}", state(side));
        steps += 1;
        steps >= 8
    });
}

#[test]
fn same_hour_republication_retains_installed_icons_failure_provenance() {
    use viewer_layer_core::playhead_host::views::{apart::Apart, tests::{COMBATS, combat, hourly_steps, record_with, write_rows}, view::HeadFailureKind};
    use replay_columnar::manifest::Status;
    use replay_data::scope::CaptureMode;
    use replay_frame::frame::TableView;

    struct StampEval;
    impl Evaluate for StampEval {
        fn icons(&self, frame: &DecodedFrame, _choice: &perspective::Choice) -> IntelIcons {
            let mut icons = IntelIcons {
                built_for: Some((frame.branch, frame.hour, 0)),
                ..IntelIcons::default()
            };
            let status = if frame.tables[COMBATS.index()].is_not_declared() {
                "combats_absent"
            } else {
                match &*frame.tables[COMBATS.index()] {
                    TableView::Unavailable { .. } => "combats_unavailable",
                    _ => "combats_complete",
                }
            };
            icons.report.reasons.insert(status.to_owned(), 1);
            icons
        }
    }

    let mut broken = combat(2u128 << 32, 0);
    broken.1[0] = viewer_layer_core::playhead_host::views::tests::Put::Unrecorded;
    let units = record_with("same hour provenance", 1, |store, at| {
        for table in super::CORE {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        store.status(COMBATS.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        write_rows(store, COMBATS, &[broken.clone()]);
    });
    let entry = open(units, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(0);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let mut icons = Icons::new(Arc::new(StampEval), Pace::Step);
    icons.views(apart.store()).unwrap();
    icons.shown = true;
    icons.applied = Some(0);
    icons.ensure_open();

    assert!(apart.drive(0), "store reached hour 0");
    let published_a = icons.open.as_ref().unwrap().reader.wait(|frame| frame.hour == 0, Duration::from_secs(10)).unwrap();
    assert_eq!(published_a.hour, 0);
    icons.offer();
    let done_a = icons.open.as_ref().unwrap().done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!icons.take(done_a));
    let (shown_a, hour_a, failures_a) = icons.icons_with_failures().expect("installed icons A");
    assert_eq!(hour_a, 0);
    assert!(shown_a.report.reasons.contains_key("combats_absent"));
    assert!(failures_a.is_empty());

    let mut with_broken = super::CORE.to_vec();
    with_broken.push(COMBATS);
    let before_b = icons.open.as_ref().unwrap().reader.published();
    icons.open.as_ref().unwrap().reader.declare_only(&with_broken);
    assert!(apart.drive(0), "store republished hour 0 with failing table");
    let after_b = icons.open.as_ref().unwrap().reader.wait_newer(before_b, Duration::from_secs(10));
    assert!(after_b > before_b);
    let (latest_b, latest_failures_b) = icons.open.as_ref().unwrap().reader.frame_with_failures().unwrap();
    assert_eq!(latest_b.hour, 0);
    assert_eq!(latest_failures_b.len(), 1);
    assert_eq!(latest_failures_b[0].table, COMBATS);
    assert!(matches!(latest_failures_b[0].kind, HeadFailureKind::BuildError(_)));

    let (still_a, still_hour_a, still_failures_a) = icons.icons_with_failures().expect("icons A still installed");
    assert_eq!(still_hour_a, 0);
    assert!(still_a.report.reasons.contains_key("combats_absent"));
    assert!(
        still_failures_a.is_empty(),
        "same-hour republication with failure must not attach to older installed icons: {still_failures_a:?}"
    );

    icons.offer();
    let done_b = icons.open.as_ref().unwrap().done.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!icons.take(done_b));
    let (shown_b, hour_b, failures_b) = icons.icons_with_failures().expect("installed icons B");
    assert_eq!(hour_b, 0);
    assert!(shown_b.report.reasons.contains_key("combats_unavailable"));
    assert_eq!(failures_b.len(), 1);
    assert_eq!(failures_b[0].failure.table, COMBATS);
    assert!(matches!(failures_b[0].failure.kind, HeadFailureKind::BuildError(_)));
    assert!(matches!(&*failures_b[0].view, TableView::Unavailable { .. }));

    let before_c = icons.open.as_ref().unwrap().reader.published();
    icons.open.as_ref().unwrap().reader.declare_only(&super::CORE);
    assert!(apart.drive(0), "store republished clean hour 0");
    let after_c = icons.open.as_ref().unwrap().reader.wait_newer(before_c, Duration::from_secs(10));
    assert!(after_c > before_c);
    let (latest_c, latest_failures_c) = icons.open.as_ref().unwrap().reader.frame_with_failures().unwrap();
    assert_eq!(latest_c.hour, 0);
    assert!(latest_failures_c.is_empty());

    let (still_b, still_hour_b, still_failures_b) = icons.icons_with_failures().expect("icons B still installed");
    assert_eq!(still_hour_b, 0);
    assert!(still_b.report.reasons.contains_key("combats_unavailable"));
    assert_eq!(
        still_failures_b.len(),
        1,
        "same-hour clean republication must not drop genuine failure provenance for still-installed icons"
    );
    assert_eq!(still_failures_b[0].failure.table, COMBATS);
    assert!(matches!(still_failures_b[0].failure.kind, HeadFailureKind::BuildError(_)));
}
