
use super::{Fill, ModeFills, Paint, TABLES};
use viewer_layer_core::{layers::{Layer, Published, View}, test_support::recorded, playhead_host::{MapSide, open, views::Recording, units::Units}};
use engine_executable::executable_tables::Tables;
use map_display_data::{
    landscape::Regions,
    province_field::{ProvinceField, Storage},
};
use playhead::{Settings, Timeline};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::Store,
};
use replay_data::rows::TableId;
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
use viewer_map_modes::map_mode::{MapMode, Stock};

const LIMIT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    moved: Condvar,
}

impl Gate {
    fn open(&self) {
        self.state.lock().unwrap().0 = true;
        self.moved.notify_all();
    }

    fn reached(&self) -> bool {
        self.state.lock().unwrap().1
    }

    fn pass(&self) -> io::Result<()> {
        let until = Instant::now() + LIMIT;
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        while !state.0 {
            let now = Instant::now();
            if now >= until {
                return Err(io::Error::other("the test never opened the gate"));
            }
            state = self.moved.wait_timeout(state, until - now).unwrap().0;
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

struct Painter {
    reads: Vec<TableId>,
    gate: Arc<Gate>,
}

impl Paint for Painter {
    fn tables(&self) -> Option<Tables> {
        None
    }

    fn row_tables(&self, mode: Stock) -> Vec<TableId> {
        mode.tables().to_vec()
    }

    fn paint(&self, frame: &DecodedFrame, _choice: &Choice, mode: &MapMode, _selected: Option<u32>) -> io::Result<Fill> {
        self.gate.pass()?;
        for table in &self.reads {
            let _ = std::hint::black_box(&*frame.tables[table.index()]);
        }
        let field = || ProvinceField(Arc::new(Storage::Dense(Arc::new(Vec::new()))));
        let regions = Regions { country_of: field(), owner_of: field(), state_of: field(), country_colors: Default::default(), warnings: Vec::new(), strategic_attributes: Default::default(), mode: None };
        Ok(Fill { mode: mode.clone(), regions: Arc::new(regions), zone: map_display_data::retained::Retained::new(Default::default()), classes: Default::default(), highlighted: Default::default() })
    }
}

fn record() -> Units {
    static RECORDED: AtomicUsize = AtomicUsize::new(0);
    let name = format!("mode fills test {}", RECORDED.fetch_add(1, Ordering::Relaxed));
    let mut tables: Vec<TableId> = TABLES.to_vec();
    tables.extend([TableId::Clock, TableId::StateResources]);
    let pages = viewer_layer_core::test_support::rail_recording::recorded(40, |_| Vec::new(), |at, store: &mut Store| {
        if at == 0 {
            store.table(TableId::Clock.index()).unwrap().insert_at(0, 0).unwrap();
            store.table(TableId::Clock.index()).unwrap().write(0, recorded::column(TableId::Clock, "speed"), Cell::I32(1)).unwrap();
        }
        for &table in &tables {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        }
    });
    let kinds = Arc::new((0..TableId::ALL.len()).map(|table| pages.kinds(table).map(<[Kind]>::to_vec).unwrap_or_default()).collect());
    let timeline = Timeline::whole(&*pages).unwrap();
    Units { name, pages, timeline, kinds }
}

fn view(mode: Stock) -> View {
    View { map_mode: MapMode::Stock(mode), ..View::default() }
}

fn side_of(units: Units, painter: Painter, at: View) -> MapSide {
    let entry = open(units, Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }).unwrap();
    let layer = super::new(Arc::new(painter), false).unwrap().in_window(Arc::clone(&entry), Recording { branch: 1 }).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], at).unwrap();
    assert!(side.refusals().next().is_none());
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    until(&mut side, "the first reset swapped in", |side, frame| frame.swapped.is_some() && !side.composing());
    side
}

fn until(side: &mut MapSide, what: &str, mut done: impl FnMut(&MapSide, &viewer_layer_core::playhead_host::Frame) -> bool) {
    let deadline = Instant::now() + LIMIT;
    loop {
        let head = side.entry().playhead().newest().unwrap_or(0);
        let frame = side.frame(head).unwrap();
        if done(side, &frame) {
            return;
        }
        assert!(Instant::now() < deadline, "not reached within 120 s: {what}");
        if frame.steps == 0 && frame.swapped.is_none() {
            std::thread::yield_now();
        }
    }
}

fn fills(side: &MapSide) -> &ModeFills {
    side.parts().unwrap().1[0].any().downcast_ref::<ModeFills>().unwrap()
}

fn shown_mode(side: &MapSide) -> Option<MapMode> {
    fills(side).product().and_then(|built| built.product.as_ref().ok()).map(|fill| fill.mode.clone())
}

#[test]
fn a_mode_fill_shows_its_own_modes_picture_from_its_first_product_and_nothing_at_the_default() {
    let gate = Arc::new(Gate::default());
    let _opens = Opens(Arc::clone(&gate));
    let air = view(Stock::StrategicAir);
    let mut side = side_of(record(), Painter { reads: vec![TableId::Subjects], gate: Arc::clone(&gate) }, air.clone());

    assert!(side.parts().unwrap().0.get(TableId::Clock).is_some(), "at another mode it declares the Clock");
    until(&mut side, "the worker's first build", |side, _| !side.composing() && gate.reached());
    assert_eq!(fills(&side).published(), Published::Nothing, "before its first picture it has published nothing");
    assert!(fills(&side).product().is_none());
    assert_eq!(fills(&side).carried().entities, 0, "the worker's build is not the frame thread's work");
    assert_eq!(super::lines(fills(&side)), vec!["map mode fill: loading, no picture of map mode strategic_air at the head yet".to_owned()], "loading, by name");

    gate.open();
    let hour = side.applied().expect("the head has reached an hour");
    side.settle_all().unwrap();
    let built = fills(&side).product().unwrap();
    assert_eq!(built.product.as_ref().map(|fill| fill.mode.clone()).ok(), Some(air.map_mode.clone()), "built for the view's mode: {:?}", built.product.as_ref().err());
    assert_eq!(fills(&side).published(), Published::Current, "once built it stands at the hour applied: its picture is the hour's");
    assert_eq!(shown_mode(&side), Some(air.map_mode.clone()));
    let tables = fills(&side).rebuilt().tables();
    assert!(TABLES.iter().all(|table| tables.contains(table)), "it declares the tables the painter reads: {tables:?}");
    assert_eq!(fills(&side).refusal(), None);

    let resources = view(Stock::Resources);
    side.set_view(resources.clone()).unwrap();
    assert!(fills(&side).rebuilt().tables().contains(&TableId::StateResources), "the new mode declares its rows before asking its worker");
    assert_eq!(shown_mode(&side), Some(air.map_mode.clone()), "the preceding picture holds during the change");
    assert!(!fills(&side).mode_ready());
    side.settle_all().unwrap();
    assert_eq!(shown_mode(&side), Some(resources.map_mode), "the new picture replaces the preceding one");
    assert!(fills(&side).mode_ready());

    side.set_view(View::default()).unwrap();
    side.frame(hour).unwrap();
    assert!(fills(&side).product().is_none(), "at the default mode it holds no picture");
    assert_eq!(fills(&side).held(), 0);
    assert_eq!(fills(&side).carried().entities, 0);
    assert_eq!(fills(&side).published(), Published::Current, "drawing nothing, it holds no hour back");
    assert!(super::lines(fills(&side)).is_empty());

    let gate = Arc::new(Gate::default());
    gate.open();
    let units = viewer_layer_core::test_support::recorded::without(record(), TableId::Factions);
    let mut side = side_of(units, Painter { reads: vec![TableId::Factions], gate }, air);
    until(&mut side, "the refused picture taken", |side, _| !side.composing() && fills(side).product().is_some());
    let why = fills(&side).refusal().map(str::to_owned).unwrap_or_else(|| {
        let taken = fills(&side).product().map(|built| (built.hour, built.product.as_ref().map(|fill| fill.mode.token()).map_err(Clone::clone)));
        panic!(
            "the picture is not refused: the layer took {taken:?} (hour, the mode drawn or the refusal), its lines {:?}, its tables {:?}",
            super::lines(fills(&side)),
            fills(&side).rebuilt().tables()
        )
    });
    assert!(why.contains("holds no Factions") && why.contains("mode_fills"), "the refusal names the table and the layer: {why}");
    assert!(super::lines(fills(&side)).iter().any(|line| line.contains("holds no Factions")), "{:?}", super::lines(fills(&side)));
    assert!(!fills(&side).rebuilt().tables().contains(&TableId::Factions), "the table is never declared");
}
