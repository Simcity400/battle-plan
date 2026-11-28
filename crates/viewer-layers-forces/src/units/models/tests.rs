use super::{Models, RetirementFlag, Select, tables};
use viewer_layer_core::{layers::{Carried, Inputs, Layer, Settling, View}, playhead_host::{Entry, MapSide, open, mirror::Mirrors, views::{tests::{COMBATS, RECORDING, combat, hourly_steps, record_with, write_rows}, self}, units::Units}};
use playhead::Step;
use replay_columnar::manifest::Status;
use replay_data::{Hour, rows::TableId, scope::CaptureMode};
use replay_frame::frame::DecodedFrame;
use std::{
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use viewer_foundation::perspective::Choice;
use viewer_unit_models::{trade_routes::ConvoyLine, unit_models::PreparedModels};

#[derive(Default)]
struct Counted {
    jobs: AtomicUsize,
}

impl Select for Counted {
    fn choose_meshes(&self, _frame: &DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), _stacks: &std::collections::HashMap<u64, super::super::meshes::Asked>, _asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> io::Result<super::super::meshes::Chosen> {
        Err(io::Error::other("no unit meshes in this test"))
    }

    fn prepare(&self, _frame: &DecodedFrame, _perspective: &Choice, _convoys: Option<&[ConvoyLine]>, _naval: bool, _kept: &mut super::Kept) -> io::Result<PreparedModels> {
        self.jobs.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other("no unit models in this test"))
    }
}

struct Maker {
    select: Arc<dyn Select>,
    learned: Vec<TableId>,
    views: Option<views::Views>,
    models: Option<Models>,
    joins: Arc<Mutex<Option<Settling>>>,
    head: Option<Hour>,
}

impl Maker {
    fn new(select: Arc<dyn Select>, learned: &[TableId], joins: &Arc<Mutex<Option<Settling>>>) -> Self {
        Self { select, learned: learned.to_vec(), views: None, models: None, joins: Arc::clone(joins), head: None }
    }

    fn models(&mut self) -> Option<&mut Models> {
        if self.models.is_none()
            && let Some(views) = &self.views
        {
            self.models = Some(Models::new(Arc::clone(&self.select), views, &self.learned));
        }
        self.models.as_mut()
    }
}

impl Layer for Maker {
    fn name(&self) -> &'static str {
        "maker"
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(TableId::Clock, true)
    }

    fn views(&mut self, store: &views::Store) -> io::Result<()> {
        self.views = Some(store.handle());
        self.models();
        Ok(())
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        if let Some(models) = self.models() {
            models.composed();
        }
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.head = Some(step.last());
    }

    fn ask(&mut self, _mirrors: &Mirrors, _upstream: &viewer_layer_core::layers::Upstream<'_>) -> io::Result<bool> {
        if let Some(models) = self.models() {
            models.ask(&Choice::default(), None, true);
        }
        Ok(true)
    }

    fn join(&mut self, _mirrors: &Mirrors, _upstream: &viewer_layer_core::layers::Upstream<'_>, _settling: Settling) -> io::Result<()> {
        let (joins, head) = (*self.joins.lock().unwrap(), self.head);
        if let (Some(settling), Some(models)) = (joins, self.models()) {
            models.join(head, settling, &RetirementFlag::default());
        }
        Ok(())
    }

    fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        Ok(())
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn held(&self) -> usize {
        0
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

fn maker(side: &MapSide) -> &Maker {
    side.parts().expect("the side's parts are home").1[0].any().downcast_ref::<Maker>().expect("the maker layer")
}

fn models(side: &MapSide) -> &Models {
    maker(side).models.as_ref().expect("the models' worker started")
}

fn side_of(recorded: Units, layer: Maker) -> (Arc<Entry>, MapSide) {
    let entry = open(recorded, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], View::default()).unwrap();
    side.set_branch(RECORDING.branch);
    entry.playhead().seek(0);
    (entry, side)
}

fn step_to(side: &mut MapSide, hour: Hour) {
    stepping(side, hour, false);
}

fn answered_at(side: &mut MapSide, hour: Hour) {
    stepping(side, hour, true);
}

fn stepping(side: &mut MapSide, hour: Hour, answered: bool) {
    for _ in 0..5_000_000u64 {
        viewer_layer_core::test_support::recorded::reads_landed(side);
        side.frame(hour).unwrap();
        let stood = !answered || (side.parts().is_some() && models(side).answered().is_some_and(|at| at >= hour));
        if side.applied() == Some(hour) && !side.composing() && stood {
            return;
        }
        std::thread::yield_now();
    }
    panic!("the map side never {} hour {hour}: it applied {:?}", if answered { "answered" } else { "applied" }, side.applied());
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen within ten seconds");
        std::thread::yield_now();
    }
}

fn clock(name: &str, hours: u32) -> Units {
    record_with(name, hours, |store, at| {
        store.status(TableId::Clock.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        let mut clock = store.table(TableId::Clock.index()).unwrap();
        if at == 0 {
            clock.insert_at(0, 0).unwrap();
        }
        clock.write(0, viewer_layer_core::test_support::recorded::column(TableId::Clock, "hour"), replay_columnar::state::Cell::I32(at as i32)).unwrap();
    })
}

#[test]
fn the_models_declare_the_tables_they_are_known_to_read_and_those_learned_not_the_map_selection() {
    let entry = open(record_with("models tables", 2, |_, _| {}), hourly_steps()).unwrap();
    let store = views::Store::new(&entry);
    let models = Models::new(Arc::new(Counted::default()), &store.handle(), &[TableId::Operations]);
    let mut wanted = tables();
    wanted.push(TableId::Operations);
    wanted.sort_unstable_by_key(|table| table.index());
    assert_eq!(models.tables(), wanted);
    assert!(tables().len() < dashboard_base::map_tables::selected_tables().len() / 2, "{} tables", tables().len());
    assert!(!models.tables().contains(&TableId::AirRegionalMissions), "a table the models do not read is not among theirs");
}

#[test]
fn a_build_begins_only_for_rows_whose_declared_tables_changed() {
    let speed = viewer_layer_core::test_support::recorded::column(TableId::Clock, "speed");
    let recorded = record_with("models jobs", 3, |store, at| {
        for table in [COMBATS, TableId::Clock] {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
        match at {
            0 => {
                write_rows(store, COMBATS, &[combat(30, 0), combat(4, 0)]);
                let mut clock = store.table(TableId::Clock.index()).unwrap();
                clock.insert_at(0, 0).unwrap();
                clock.write(0, speed, replay_columnar::state::Cell::I32(1)).unwrap();
            }
            1 => write_rows(store, COMBATS, &[combat(30, 1)]),
            2 => store.table(TableId::Clock.index()).unwrap().write(0, speed, replay_columnar::state::Cell::I32(2)).unwrap(),
            _ => {}
        }
    });
    let counted = Arc::new(Counted::default());
    let joins = Arc::new(Mutex::new(Some(Settling::Whole)));
    let (_entry, mut side) = side_of(recorded, Maker::new(Arc::clone(&counted) as Arc<dyn Select>, &[], &joins));
    for (hour, jobs) in [(0, 1), (1, 1), (2, 2)] {
        answered_at(&mut side, hour);
        assert_eq!(counted.jobs.load(Ordering::SeqCst), jobs, "hour {hour}: a build only when a table it reads moved ({:?} read)", models(&side).tables());
    }
    assert!(models(&side).refused.as_deref().is_some_and(|why| why.contains("no unit models in this test")), "{:?}", models(&side).refused);
}

#[test]
fn a_late_row_declaration_does_not_leave_the_old_models_made_at_their_hour() {
    let joins = Arc::new(Mutex::new(None));
    let (_entry, mut side) = side_of(clock("models late declaration", 2), Maker::new(Arc::new(Counted::default()), &[], &joins));
    answered_at(&mut side, 0);
    until("the first models", || models(&side).stands() == Some(0));
    assert!(models(&side).made_at(0));
    models(&side).reader.declare_only(&[TableId::Clock]);
    assert!(!models(&side).made_at(0), "clearing the reader frame invalidates the old models");
}

#[derive(Default)]
struct Gated {
    begun: Mutex<Vec<Hour>>,
    held: Mutex<bool>,
    changed: Condvar,
}

impl Gated {
    fn open(&self) {
        *self.held.lock().unwrap() = false;
        self.changed.notify_all();
    }

    fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    fn begun(&self) -> Vec<Hour> {
        self.begun.lock().unwrap().clone()
    }
}

impl Select for Gated {
    fn choose_meshes(&self, _frame: &DecodedFrame, _ours: u32, _kept: (&viewer_unit_models::unit_models::KeptChoices, Option<&std::collections::BTreeSet<u64>>), _stacks: &std::collections::HashMap<u64, super::super::meshes::Asked>, _asked: &std::collections::HashSet<u64>, _told: &viewer_unit_models::unit_models::Told, _parts: &mut dyn FnMut(&'static str, u64)) -> io::Result<super::super::meshes::Chosen> {
        Err(io::Error::other("no unit meshes in this test"))
    }

    fn prepare(&self, frame: &DecodedFrame, _perspective: &Choice, _convoys: Option<&[ConvoyLine]>, _naval: bool, _kept: &mut super::Kept) -> io::Result<PreparedModels> {
        self.begun.lock().unwrap().push(frame.hour);
        let until = Instant::now() + Duration::from_secs(10);
        let mut held = self.held.lock().unwrap();
        while *held {
            let Some(left) = until.checked_duration_since(Instant::now()) else { break };
            held = self.changed.wait_timeout(held, left).unwrap().0;
        }
        Ok(PreparedModels::drawn_by_layers())
    }
}

#[test]
fn the_models_are_asked_at_the_apply_and_build_before_any_join() {
    let gated = Arc::new(Gated::default());
    gated.hold();
    let joins = Arc::new(Mutex::new(None));
    let (_entry, mut side) = side_of(clock("models asked", 4), Maker::new(Arc::clone(&gated) as Arc<dyn Select>, &[], &joins));
    step_to(&mut side, 0);
    until("the build of the rows asked for", || !gated.begun().is_empty());
    assert!(models(&side).in_flight() > 0, "the build is running with no join");
    gated.open();
}

#[test]
fn steps_faster_than_a_build_keep_the_latest_finished_models() {
    let gated = Arc::new(Gated::default());
    let joins = Arc::new(Mutex::new(Some(Settling::Whole)));
    let (_entry, mut side) = side_of(clock("models faster", 12), Maker::new(Arc::clone(&gated) as Arc<dyn Select>, &[], &joins));
    answered_at(&mut side, 0);
    for _ in 0..5_000_000u64 {
        if models(&side).stands().is_some() {
            break;
        }
        side.frame(0).unwrap();
        std::thread::yield_now();
    }
    assert_eq!(models(&side).stands(), Some(0), "the models of hour 0: {:?}", models(&side).refused);
    let first = Arc::clone(&models(&side).prepared.as_ref().expect("models made").1);

    gated.hold();
    *joins.lock().unwrap() = Some(Settling::step());
    step_to(&mut side, 1);
    until("the build of hour 1", || gated.begun().len() > 1);
    for hour in 2..=8 {
        step_to(&mut side, hour);
        assert_eq!(models(&side).stands(), Some(0), "hour {hour}: the figures of the last finished build are shown");
        assert!(std::ptr::eq(Arc::as_ptr(&models(&side).prepared.as_ref().expect("models").1), Arc::as_ptr(&first)), "hour {hour}: the same models, not remade");
    }
    assert_eq!(gated.begun(), vec![0, 1], "one build at a time: the seven hours stepped over while it ran queued no build of their own");
    gated.open();

    until("the models of the newest rows", || {
        side.frame(8).unwrap();
        models(&side).stands() == Some(8)
    });
    let begun = gated.begun();
    assert_eq!(begun.last(), Some(&8), "the hour it ends at is the newest rows': {begun:?}");
    assert!(begun.windows(2).all(|pair| pair[0] < pair[1]), "the hours it builds only move forward: {begun:?}");
}
