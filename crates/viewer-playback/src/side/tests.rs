use viewer_layer_core::test_support::host_recording::{Sum, record, units, busy, quick};
use super::SideThread;
use viewer_layer_core::layers::{Layer, View};
use crate::map_open::MapSide;
use viewer_layer_core::playhead_host::{Head, open};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn side(name: &str, layer: Box<dyn Layer>) -> SideThread {
    side_waking(name, layer, Arc::new(|| {}))
}

fn side_waking(name: &str, layer: Box<dyn Layer>, wake: Arc<dyn Fn() + Send + Sync>) -> SideThread {
    let (recording, _) = record(1, 8, busy);
    let entry = open(units(recording, name), quick()).unwrap();
    let side = SideThread::new(MapSide::with_products(entry, vec![layer], View::default()).unwrap(), wake).unwrap();
    side.anchor(0, false, 1.0, true);
    side
}

#[test]
fn the_frames_window_never_waits_while_the_sides_thread_prepares_a_step() {
    let composes = Arc::new(Mutex::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let (mut layer, release, began) = Sum::held_settle(&composes);
    layer.settle_armed = Some(Arc::clone(&armed));
    let mut side = side("window never waits", Box::new(layer));
    side.driven(|side| viewer_layer_core::test_support::recorded::frames_at(side, 0, "composed at hour 0", |side, frame| frame.swapped.is_some() && !side.composing()));
    armed.store(true, Ordering::Release);
    side.ask(View::default(), Head::Paced(7), None, None);
    began.recv().expect("the layer entered settle while holding the side");
    for _ in 0..100 {
        assert!(side.window().is_none(), "no window while the side's thread holds the side");
        assert!(side.products().missed.is_some(), "the frame is told what the side's thread was doing, rather than waiting for it");
    }
    release.send(()).unwrap();
    side.wait_window(|side| side.applied() == Some(7)).unwrap();
    assert_eq!(side.products().missed, None, "the window is given again once settle finishes");
}

#[test]
fn retiring_the_side_releases_a_layer_waiting_in_its_step() {
    let composes = Arc::new(Mutex::new(0));
    let (mut layer, stop, in_step) = Sum::held_settle(&composes);
    layer.stop_on_retirement = Some(stop);
    let side = side("retirement during a step", Box::new(layer));
    side.ask(View::default(), Head::Stepped(5), None, None);
    in_step.recv().expect("the layer is settling with the side held");
    drop(side);
}

#[test]
fn a_view_asked_between_two_steps_is_taken_at_the_next_one() {
    let composes = Arc::new(Mutex::new(0));
    let mut side = side("view between steps", Box::new(Sum::new(&composes)));
    let high = View { camera_height: 4.0, ..View::default() };
    side.ask(View::default(), Head::Paced(4), None, None);
    side.wait_window(|side| side.applied() == Some(4)).unwrap();
    side.ask(high, Head::Paced(6), None, None);
    side.wait_window(|side| side.view().camera_height == 4.0).unwrap();
    let heights = side.with_side(|side| side.parts().unwrap().1[0].any().downcast_ref::<Sum>().unwrap().viewed.clone());
    assert!(heights.contains(&4.0), "the camera the view asked for reached the layer: {heights:?}");
    assert_eq!(side.view().camera_height, 4.0, "and the products say the view its picture stands at");
}

#[test]
fn a_stepped_frame_is_told_when_the_side_stops_in_its_step() {
    let composes = Arc::new(Mutex::new(0));
    let (layer, release, began) = Sum::settle_stops(&composes);
    let mut side = side("stepped failure", Box::new(layer));
    side.ask(View::default(), Head::Stepped(5), None, None);
    began.recv().expect("the side's thread settles the step");
    let _ = release.send(());
    assert!(side.stepped_window(5).is_none(), "no window once the side has stopped");
    let why = side.failure().expect("the frame is told why it has no window");
    assert!(why.contains("the test layer settles no further"), "named by the layer that stopped it: {why}");
}

#[test]
fn a_stepped_frame_refuses_by_name_rather_than_wait_for_a_picture_nothing_will_make() {
    let composes = Arc::new(Mutex::new(0));
    let mut side = side("stepped bound", Box::new(Sum::new(&composes).times_out_at(6)));
    side.ask(View::default(), Head::Stepped(3), None, None);
    drop(side.stepped_window(3).expect("the picture of the hour asked"));
    let short = (Duration::ZERO, Duration::ZERO);
    assert!(side.whole_at(6, short.0, short.1).is_none(), "no window for an hour nothing stands at");
    let why = side.take_refusal().expect("refused by name");
    assert!(why.contains("hour 6") && why.contains("Stepped(3)") && why.contains("whole at Some(3)"), "says what it waited for: {why}");
    assert!(side.failure().is_none(), "the side does not stop for a capture's refusal");
    drop(side.stepped_window(3).expect("the side still answers the frames after"));
    side.ask(View::default(), Head::Stepped(6), None, None);
    assert!(side.stepped_window(6).is_none(), "no window for an hour not made whole");
    let why = side.take_refusal().expect("refused by name");
    assert!(why.contains("times out once"), "says why: {why}");
    assert!(side.failure().is_none(), "the side does not stop for a capture's refusal");
    side.ask(View::default(), Head::Stepped(6), None, None);
    drop(side.stepped_window(6).expect("the refused hour asked again is made whole"));
    assert!(side.take_refusal().is_none());
}

#[test]
fn a_seek_while_catching_up_leaves_the_rows_driven_at_the_stepped_hour() {
    let composes = Arc::new(Mutex::new(0));
    let (layer, release, began) = Sum::held_settle(&composes);
    let mut side = side("seek while catching up", Box::new(layer));
    side.ask(View::default(), Head::Stepped(6), None, None);
    began.recv().expect("the side's thread settles within its catch-up");
    let before = side.entry().playhead().epoch();
    side.entry().anchor(2, false, 1.0, true);
    assert_ne!(side.entry().playhead().epoch(), before, "the seek is in force");
    let _ = release.send(());
    let mut window = side.stepped_window(6).expect("the picture of the hour asked");
    window.take(Default::default());
    drop(window);
    assert_eq!(side.applied(), Some(6), "the side stands at the stepped hour, so its views do");
}

#[test]
fn a_stepped_frames_window_is_the_picture_of_exactly_its_hour() {
    let composes = Arc::new(Mutex::new(0));
    let mut side = side("stepped picture", Box::new(Sum::new(&composes)));
    for hour in [3, 5, 6] {
        side.ask(View::default(), Head::Stepped(hour), None, None);
        let mut window = side.stepped_window(hour).expect("the picture of the hour asked");
        window.take(Default::default());
        drop(window);
        assert_eq!(side.applied(), Some(hour), "every step to the hour is applied");
        assert_eq!(side.last_frame().drawn, Some(hour), "and every layer shows it");
    }
    let high = View { camera_height: 4.0, ..View::default() };
    side.ask(high, Head::Stepped(6), None, None);
    let mut window = side.stepped_window(6).expect("the picture of the view asked");
    window.take(Default::default());
    drop(window);
    assert_eq!(side.view().camera_height, 4.0, "the picture stands at the view asked");
}

#[test]
fn a_paused_side_wakes_on_seek_mode_switch_and_landed_answer_and_stops_at_rest() {
    use std::sync::mpsc;
    use viewer_layer_core::{playhead_host::views::Need, test_support::host_recording::table};
    let (woke_send, woke_recv) = mpsc::channel::<()>();
    let composes = Arc::new(Mutex::new(0));
    let mut side = side_waking("paused wake", Box::new(Sum::new(&composes)), Arc::new(move || {
        let _ = woke_send.send(());
    }));
    fn drive_until(side: &mut SideThread, woke_recv: &mpsc::Receiver<()>, mut done: impl FnMut(&SideThread) -> bool) {
        for _ in 0..64 {
            woke_recv.recv_timeout(Duration::from_secs(5)).expect("side woke the frame");
            side.driven(|_| ());
            if done(side) {
                return;
            }
        }
        panic!("paused side did not settle within 64 woken turns");
    }

    side.ask(View::default(), Head::Paced(3), None, None);
    drive_until(&mut side, &woke_recv, |side| side.applied() == Some(3) && side.on_screen().is_some_and(|on| on.settled && on.drawn == Some(3)));

    side.anchor(6, false, 0.0, true);
    side.ask(View::default(), Head::Paced(6), None, None);
    drive_until(&mut side, &woke_recv, |side| side.applied() == Some(6) && side.on_screen().is_some_and(|on| on.settled && on.drawn == Some(6)));

    let switched = View { map_mode: viewer_map_modes::map_mode::MapMode::Stock(viewer_map_modes::map_mode::Stock::Supply), ..View::default() };
    side.ask(switched.clone(), Head::Paced(6), None, None);
    drive_until(&mut side, &woke_recv, |side| side.mode_on_screen() == Some(&switched.map_mode) && side.on_screen().is_some_and(|on| on.settled && on.drawn == Some(6)));

    let ask_before = side.shared.asked().expect("ask held").serial;
    side.ask(switched.clone(), Head::Paced(6), None, None);
    {
        let turn = super::lock(&side.shared.held);
        let ask = side.shared.asked().expect("ask held");
        assert_eq!(ask.serial, ask_before, "an identical live ask does not advance serial");
        assert!(super::rests(&turn, &ask, 6, false, false), "a settled paused side rests on an identical ask");
    }
    assert!(woke_recv.try_recv().is_err(), "an identical ask at rest wakes nothing");

    let table = table(0);
    let reader = side.views.reader("late column at the paused head", &[Need::columns(table, 1 << 1)]);
    drive_until(&mut side, &woke_recv, |_| reader.frame_of(6).is_some());
    assert!(side.with_side(|side| side.parts().unwrap().0.get(table).unwrap().holds(1)));
    assert_eq!(side.view().map_mode, switched.map_mode);

    let (send, _receive) = viewer_runtime::landed::channel::<u32>();
    send.send(1).unwrap();
    woke_recv.recv_timeout(Duration::from_secs(5)).expect("a landed answer wakes a resting paused side");
}

#[test]
fn a_view_change_keeps_independent_products_in_the_window() {
    let (recording, _) = record(2, 8, busy);
    let entry = open(units(recording, "hide one layer"), quick()).unwrap();
    let first = Sum::new(&Arc::new(Mutex::new(0)));
    let mut second = Sum::new(&Arc::new(Mutex::new(0)));
    (second.second, second.hide_high) = (true, true);
    let map = MapSide::with_products(entry, vec![Box::new(first), Box::new(second)], View::default()).unwrap();
    let mut side = SideThread::new(map, Arc::new(|| {})).unwrap();
    side.anchor(0, false, 1.0, true);
    side.ask(View::default(), Head::Paced(3), None, None);
    side.wait_window(|side| side.applied() == Some(3) && !side.composing() && side.draws("sum") && side.draws("second")).unwrap();
    side.ask(View { camera_height: 4.0, ..View::default() }, Head::Paced(3), None, None);
    side.wait_window(|side| {
        assert!(side.draws("sum") && !side.composing(), "every window keeps the independent product during the view change");
        side.view().camera_height == 4.0
    }).unwrap();
    assert!(side.with_side(|side| side.parts().is_some()));
}

struct StubOverlay {
    pending_refusal: Arc<AtomicBool>,
    refused: bool,
    hour: Option<replay_data::Hour>,
}

impl viewer_layer_core::playhead_host::Overlaying<crate::map_open::Products> for StubOverlay {
    fn ask(&mut self, _ask: crate::overlay::Ask) {}
    fn ask_step(&mut self, side: viewer_layer_core::playhead_host::OverlayStep<'_>, _whole: bool, _lay: bool, _moved: Arc<viewer_unit_reads::counter_move::MovedHours>) -> bool {
        self.hour = side.applied;
        true
    }
    fn join(&mut self, _layers: &[Box<dyn Layer>], _perspective: &viewer_foundation::perspective::Choice, _branch: Option<replay_data::BranchId>, _settling: viewer_layer_core::layers::Settling) -> bool {
        true
    }
    fn stepped(&mut self) {}
    fn stands_at(&self, hour: replay_data::Hour) -> bool {
        !self.refused && self.hour.is_some_and(|held| held >= hour)
    }
    fn take(&mut self) -> Option<viewer_runtime::landed::Landed<crate::overlay::Made>> {
        if self.pending_refusal.load(Ordering::Acquire) {
            self.refused = true;
            self.hour = None;
        }
        None
    }
    fn reset(&mut self) {
        self.hour = None;
        self.refused = false;
    }
    fn generation(&self) -> u64 {
        0
    }
    fn hour(&self) -> Option<replay_data::Hour> {
        (!self.refused).then_some(self.hour).flatten()
    }
    fn loading(&self) -> &[String] {
        &[]
    }
    fn missed(&self) -> &[replay_data::rows::TableId] {
        &[]
    }
    fn refused(&self) -> bool {
        self.refused
    }
    fn needs_reask(&self) -> bool {
        false
    }
    fn offered_stale(&self, _layers: viewer_layer_core::playhead_host::LayerParts<'_>, _branch: Option<replay_data::BranchId>) -> bool {
        false
    }
}

#[test]
fn a_paused_side_with_an_overlay_rests_when_settled_and_wakes_on_refusal() {
    use std::sync::mpsc;
    let (woke_send, woke_recv) = mpsc::channel::<()>();
    let composes = Arc::new(Mutex::new(0));
    let refused = Arc::new(AtomicBool::new(false));
    let (recording, _) = record(1, 8, busy);
    let entry = open(units(recording, "paused overlay rest"), quick()).unwrap();
    let mut map_side = MapSide::with_products(entry, vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    map_side.set_overlay(Box::new(StubOverlay { pending_refusal: Arc::clone(&refused), refused: false, hour: None }));
    let mut side = SideThread::new(map_side, Arc::new(move || {
        let _ = woke_send.send(());
    })).unwrap();
    fn drive_until(side: &mut SideThread, woke_recv: &mpsc::Receiver<()>, mut done: impl FnMut(&SideThread) -> bool) {
        for _ in 0..64 {
            woke_recv.recv_timeout(Duration::from_secs(5)).expect("side woke the frame");
            side.driven(|_| ());
            if done(side) {
                return;
            }
        }
        panic!("paused side with overlay did not settle within 64 woken turns");
    }
    side.anchor(0, false, 1.0, true);
    side.ask(View::default(), Head::Paced(4), None, None);
    drive_until(&mut side, &woke_recv, |side| side.applied() == Some(4) && side.on_screen().is_some_and(|on| on.settled && on.drawn == Some(4)));
    {
        let turn = super::lock(&side.shared.held);
        let ask = side.shared.asked().expect("ask held");
        assert!(super::rests(&turn, &ask, 4, false, false), "a settled paused side with an overlay rests");
    }
    assert!(woke_recv.try_recv().is_err(), "no extra wake once settled");
    refused.store(true, Ordering::Release);
    let (send, _recv) = viewer_runtime::landed::channel::<()>();
    send.send(()).unwrap();
    drive_until(&mut side, &woke_recv, |side| side.products().overlay_refused);
    assert!(side.products().overlay_refused && !side.products().overlay_coming() && side.products().overlay_hour.is_none());
    let turn = super::lock(&side.shared.held);
    let ask = side.shared.asked().expect("ask held");
    assert!(super::rests(&turn, &ask, 4, false, false), "a refused overlay settles and rests");
}
