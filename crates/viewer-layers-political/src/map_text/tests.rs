
use super::{MapText, Presented, Source, TABLES};
use viewer_layer_core::{layers::{Layer, Upstream, View}, test_support::recorded, vision, playhead_host::{MapSide, open}};
use content_reader_key_names::buildings::Buildings;
use content_reader_localisation::localisation::Localisation;
use map_display_data::{MapTextResult, PopupResult};
use playhead::Settings;
use replay_columnar::{manifest::Status, state::Cell, store::Store};
use replay_data::rows::{
    Field, MapEvent, MapEventFamily, MapEventFamilyHealth, MapEventGeneralCapture, MapEventParadrop, MapEventPayload, NativeValue, Opaque, TableId,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use viewer_foundation::visibility::FogContent;
use viewer_map_text::animated_map_text::events;

const BRANCH: u64 = 0;

struct Content {
    text: events::Content,
    localisation: Localisation,
    fog: Result<FogContent, String>,
}

impl Source for Content {
    fn text(&self) -> &events::Content {
        &self.text
    }

    fn localisation(&self) -> &Localisation {
        &self.localisation
    }

    fn place(&self, _province: u32) -> Option<([f32; 3], [f32; 4])> {
        None
    }

    fn fog(&self) -> &Result<FogContent, String> {
        &self.fog
    }
}

fn source() -> Arc<dyn Source> {
    let localisation = [("CAPTURED_GENERAL_MAP_TEXT".to_owned(), "Captured".to_owned()), ("PROV_TEXT_UNITS_PARACHUTED".to_owned(), "Landed".to_owned())].into_iter().collect();
    Arc::new(Content { text: events::Content::new(&Buildings::default()), localisation, fog: Ok(fog()) })
}

fn fog() -> FogContent {
    use viewer_foundation::visibility::{Topology, test_defines};
    let neighbors = BTreeMap::from([(1, vec![2]), (2, vec![1])]);
    FogContent { topology: Topology { land: BTreeSet::from([1]), sea: BTreeSet::from([2]), neighbors }, defines: test_defines() }
}

fn vision_content() -> Arc<vision::Content> {
    Arc::new(vision::Content::new(Ok(fog())))
}

fn opaque(value: &impl NativeValue) -> Vec<u8> {
    let Field::Opaque(Opaque::Native(value)) = value.field() else { panic!("an opaque field") };
    let mut bytes = Vec::new();
    value.encode_value(&mut bytes).unwrap();
    bytes
}

fn captured(sequence: u64, hour: i32, owner: u32, captor: u32) -> MapEvent {
    MapEvent { sequence, hour, payload: MapEventPayload::GeneralCaptured(MapEventGeneralCapture { owner, captor, province: 1 }) }
}

fn events_at(at: u32) -> Vec<MapEvent> {
    match at {
        6 => vec![captured(5, 6, 101, 102), captured(6, 6, 102, 103)],
        10 => vec![captured(9, 10, 101, 103)],
        _ => Vec::new(),
    }
}

fn next(at: u32) -> u64 {
    (0..=at).flat_map(events_at).map(|event| event.sequence + 1).max().unwrap_or(1)
}

fn record(recorded: impl Fn(u32) -> bool) -> viewer_layer_core::playhead_host::units::Units {
    record_with(recorded, |_, _| {})
}

fn insert_whole(store: &mut Store, table: TableId, slot: u32, key: u128) {
    use replay_columnar::state::Kind;
    let kinds = replay_recording::schema::schema().unwrap();
    let mut changes = store.table(table.index()).unwrap();
    changes.insert_at(slot, key).unwrap();
    for (column, kind) in kinds[table.index()].iter().enumerate() {
        let cell = match kind {
            Kind::U8 => Cell::U8(0),
            Kind::I8 => Cell::I8(0),
            Kind::U16 => Cell::U16(0),
            Kind::I16 => Cell::I16(0),
            Kind::U32 => Cell::U32(0),
            Kind::I32 => Cell::I32(0),
            Kind::U64 => Cell::U64(0),
            Kind::I64 => Cell::I64(0),
            Kind::Bool => Cell::Bool(false),
            Kind::OptionalI32 => Cell::OptionalI32(None),
            Kind::OptionalU32 => Cell::OptionalU32(None),
            Kind::OptionalI64 => Cell::OptionalI64(None),
            Kind::OptionalU64 => Cell::OptionalU64(None),
            Kind::Bytes => Cell::Bytes(0),
            Kind::U32List => Cell::U32List(&[]),
            Kind::U64List => Cell::U64List(&[]),
            Kind::Opaque => continue,
        };
        changes.write(slot, column, cell).unwrap();
    }
}

fn record_with(recorded: impl Fn(u32) -> bool, more: impl Fn(u32, &mut Store)) -> viewer_layer_core::playhead_host::units::Units {
    let mut tables: Vec<TableId> = viewer_foundation::perspective::VISION_TABLES.to_vec();
    tables.extend(TABLES);
    tables.sort_unstable();
    tables.dedup();
    let column = recorded::column;
    let health = [MapEventFamilyHealth { installed: true, enabled: true, complete: true, ..MapEventFamilyHealth::default() }; MapEventFamily::COUNT];
    let health = opaque(&health);
    static RECORDED: AtomicUsize = AtomicUsize::new(0);
    let name = format!("map text test {}", RECORDED.fetch_add(1, Ordering::Relaxed));
    recorded::record_naming(&name, 30, &[b"AAA", b"BBB", b"CCC"], |at, store| {
        let write = |store: &mut Store, table: TableId, slot: u32, field: &str, cell: Cell<'_>| store.table(table.index()).unwrap().write(slot, column(table, field), cell).unwrap();
        let insert = |store: &mut Store, table: TableId, slot: u32, key: u128| {
            if table == TableId::MapEvents {
                store.table(table.index()).unwrap().insert_at(slot, key).unwrap();
            } else {
                insert_whole(store, table, slot, key);
            }
        };
        if at == 0 {
            for (slot, raw) in [101u32, 102, 103].into_iter().enumerate() {
                insert(store, TableId::CountryLinks, slot as u32, u128::from(raw));
                write(store, TableId::CountryLinks, slot as u32, "canonical_slot", Cell::U32(raw - 100));
                insert(store, TableId::Countries, slot as u32, u128::from(raw));
                write(store, TableId::Countries, slot as u32, "canonical_slot", Cell::U32(raw - 100));
                write(store, TableId::Countries, slot as u32, "tag", Cell::Bytes(slot as u32));
            }
            insert(store, TableId::LocalPlayer, 0, 0);
            write(store, TableId::LocalPlayer, 0, "country", Cell::U32(101));
            write(store, TableId::LocalPlayer, 0, "observer_country", Cell::U32(0));
            insert(store, TableId::MapEvents, 0, 0);
        }
        let events = opaque(&events_at(at));
        let first = if at == 0 { 1 } else { next(at - 1) };
        write(store, TableId::MapEvents, 0, "epoch", Cell::U64(1));
        write(store, TableId::MapEvents, 0, "from_hour", Cell::I32(at as i32 - 1));
        write(store, TableId::MapEvents, 0, "to_hour", Cell::I32(at as i32));
        write(store, TableId::MapEvents, 0, "first_sequence", Cell::U64(first));
        write(store, TableId::MapEvents, 0, "next_sequence", Cell::U64(next(at)));
        write(store, TableId::MapEvents, 0, "health", Cell::Opaque(&health));
        write(store, TableId::MapEvents, 0, "events", Cell::Opaque(&events));
        more(at, store);
        for &table in &tables {
            let status = if table == TableId::MapEvents && !recorded(at) { Status::NotRecorded } else { Status::Complete };
            store.status(table.index(), status, true, Some(at as i32), true).unwrap();
        }
    })
}

fn settings(hours: f64) -> Settings {
    Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: hours, rate: 1.0, ..Settings::default() }
}

fn layer(side: &MapSide) -> &MapText {
    side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<MapText>()).unwrap()
}

fn play(units: viewer_layer_core::playhead_host::units::Units, step: i32, last: i32, settled: bool, mut each: impl FnMut(i32, bool, &mut MapSide)) -> MapSide {
    let entry = open(units, settings(f64::from(step))).unwrap();
    let text = MapText::new(source(), BRANCH).with_rows(Arc::clone(&entry));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(vision::Vision::new(vision_content())), Box::new(text)], View::default()).unwrap();
    assert!(side.refusals().next().is_none(), "no layer refused: {:?}", side.refusals().collect::<Vec<_>>());
    entry.playhead().play(f64::from(step));
    entry.playhead().seek(0);
    let mut applied = None::<i32>;
    loop {
        let head = match applied {
            None => 0,
            Some(hour) if side.composing() => hour,
            Some(hour) => entry.playhead().newest().unwrap_or(hour).min(hour + step),
        };
        let frame = side.frame(head).unwrap();
        applied = frame.applied.or(applied);
        if (frame.steps > 0 || frame.swapped.is_some()) && !side.composing()
            && let Some(hour) = applied
        {
            if settled {
                side.settle_all().unwrap();
            }
            each(hour, frame.swapped.is_some(), &mut side);
            if hour >= last {
                return side;
            }
        } else {
            std::thread::yield_now();
        }
    }
}

fn shown(presented: &Presented) -> (i32, Vec<(u64, Result<Option<Vec<u8>>, String>)>) {
    let text = presented.shown().unwrap_or_else(|reason| panic!("an interval, not {reason}"));
    let MapTextResult::Value { interval } = text.interval else { panic!("a recorded interval: {:?}", text.interval) };
    let events = interval.events.into_iter().map(|event| (event.sequence, match event.popup {
        PopupResult::Value { popup } => Ok(popup.map(|popup| popup.text)),
        PopupResult::Refused { reason } => Err(reason),
    }));
    (interval.to_hour, events.collect())
}

#[test]
fn every_interval_the_head_steps_to_is_made_once_and_shows_the_viewer_its_own() {
    let mut taken = 0;
    let mut seen = Vec::new();
    play(record(|_| true), 1, 12, true, |hour, swapped, side| {
        let pending = layer(side).pending();
        if swapped {
            assert!(pending[0].seek(), "a reset's interval is presented as sought");
            taken = 0;
        } else {
            assert_eq!(pending.len(), taken + 1, "one interval a one-hour step, at hour {hour}");
        }
        let made = pending.last().unwrap();
        assert_eq!(made.at(), (BRANCH, hour), "made for the hour its step applied");
        for made in &pending[taken..] {
            let (to_hour, events) = shown(made);
            assert_eq!(to_hour, made.at().1, "the interval ending at the hour it was made for");
            seen.extend(events);
        }
        taken = pending.len();
    });
    assert_eq!(seen, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None)), (9, Ok(Some(b"Captured".to_vec())))], "each event once, the viewer's shown and another's withheld");
}

#[test]
fn a_step_of_four_hours_makes_its_last_hour_and_a_seek_makes_the_head_as_sought() {
    let mut steps = Vec::new();
    let mut side = play(record(|_| true), 4, 12, true, |hour, swapped, side| {
        let made = layer(side).pending().last().map(|made| (*made).clone()).unwrap();
        assert_eq!(made.at().1, hour, "the step's last hour");
        if !swapped {
            steps.push((hour, shown(&made).1.len()));
        }
    });
    assert_eq!(steps.iter().map(|&(hour, _)| hour).collect::<Vec<_>>(), [4, 8, 12], "steps of four hours");
    assert!(steps.iter().all(|&(hour, events)| events == events_at(hour as u32).len()), "each step its last hour's events, never those of the hours it passed (6, 10): {steps:?}");
    side.entry().playhead().seek(6);
    loop {
        let frame = side.frame(6).unwrap();
        if frame.swapped.is_some() && !side.composing() {
            break;
        }
        std::thread::yield_now();
    }
    side.settle_all().unwrap();
    let pending = layer(&side).pending();
    let made = pending.last().expect("the sought hour's interval");
    assert_eq!((made.at().1, made.seek()), (6, true), "the head's interval after a seek, presented as sought: {:?}", pending.iter().map(|made| made.at()).collect::<Vec<_>>());
    assert_eq!(shown(made).1.len(), 2, "the sought hour's two events");
}

#[test]
fn a_map_events_table_not_recorded_makes_today_s_unavailable_interval_by_name() {
    play(record(|at| at >= 8), 4, 12, true, |hour, _, side| {
        let made = layer(side).pending().last().map(|made| (*made).clone()).unwrap();
        let text = made.shown().unwrap_or_else(|reason| panic!("today's interval, not {reason}"));
        match (hour < 8, &text.interval) {
            (true, MapTextResult::Unavailable { reason }) => assert!(!reason.is_empty(), "named"),
            (false, MapTextResult::Value { interval }) => assert_eq!(interval.to_hour, hour),
            (_, interval) => panic!("hour {hour}: {interval:?}"),
        }
    });
}

fn frames_until(side: &mut MapSide, hour: i32, what: &str, mut done: impl FnMut(&MapSide) -> bool) {
    for _ in 0..50_000_000u64 {
        if done(side) {
            return;
        }
        let frame = side.frame(hour).unwrap();
        if frame.steps == 0 && frame.swapped.is_none() {
            std::thread::yield_now();
        }
    }
    panic!("{what} never held: {}", recorded::seen(side));
}

#[test]
fn a_step_with_a_map_event_makes_nothing_on_the_frame_thread_and_is_presented_once_on_a_later_frame() {
    let mut side = play(record(|_| true), 1, 12, false, |_, _, _| {});
    frames_until(&mut side, 12, "every interval made", |side| !layer(side).waiting() && layer(side).pending().last().is_some_and(|made| made.at().1 == 12));
    let layer = layer(&side);
    let [_, in_place, on_worker] = layer.marked();
    assert_eq!(on_worker, 2, "the intervals of hours 6 and 10, which hold events, were made on the worker");
    assert_eq!(in_place, 11, "only the eleven intervals of hours 0 to 12 holding no event were made on the frame thread");
    let hours: Vec<i32> = layer.pending().iter().map(|made| made.at().1).collect();
    assert_eq!(hours, (0..=12).collect::<Vec<_>>(), "every interval once, in order");
    let events: Vec<u64> = layer.pending().iter().flat_map(|made| shown(made).1).map(|(sequence, _)| sequence).collect();
    assert_eq!(events, [5, 6, 9], "each event once");
}

#[test]
fn a_seek_back_decides_the_sought_hours_events_from_its_own_rows_not_the_old_heads() {
    let mut side = play(record(|_| true), 1, 12, true, |_, _, _| {});
    side.entry().playhead().seek(6);
    frames_until(&mut side, 6, "the sought hour composed", |side| !side.composing() && layer(side).pending().last().is_some_and(|made| made.at().1 == 6));
    side.settle_all().unwrap();
    let made = layer(&side).pending().last().map(|made| (*made).clone()).unwrap();
    assert_eq!((made.at().1, made.seek()), (6, true), "the sought hour's interval");
    assert_eq!(shown(&made).1, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None))], "hour 6's events decided from hour 6's rows");
}

fn relinked(at: u32, store: &mut Store) {
    if at == 8 {
        store.table(TableId::CountryLinks.index()).unwrap().write(1, recorded::column(TableId::CountryLinks, "canonical_slot"), Cell::U32(1)).unwrap();
    }
}

fn stepped_to(entry: &Arc<viewer_layer_core::playhead_host::Entry>, attached: bool, last: i32) -> MapSide {
    let mut text = MapText::new(source(), BRANCH);
    if attached {
        text = text.with_rows(Arc::clone(entry));
    }
    let mut side = MapSide::new(Arc::clone(entry), vec![Box::new(vision::Vision::new(vision_content())), Box::new(text)], View::default()).unwrap();
    entry.playhead().seek(0);
    frames_until(&mut side, 0, "the seek composed", |side| !side.composing() && side.applied() == Some(0));
    for hour in 1..=last {
        frames_until(&mut side, hour, "a step an hour", |side| side.last_frame().applied == Some(hour));
        side.settle_all().unwrap();
    }
    side
}

fn take_batch_frame(side: &mut MapSide, hour: i32) -> viewer_layer_core::playhead_host::Frame {
    for _ in 0..50_000_000u64 {
        let frame = side.frame(hour).unwrap();
        if frame.steps > 0 {
            return frame;
        }
        std::thread::yield_now();
    }
    panic!("no frame consumed steps through hour {hour}: {}", recorded::seen(side));
}

#[test]
fn three_steps_in_one_frame_decide_the_first_steps_events_from_its_own_rows() {
    let entry = open(record_with(|_| true, relinked), Settings { step_threads: 1, fetch_threads: 1, ahead: 4, ring: 6, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = stepped_to(&entry, true, 5);
    entry.playhead().wait_newest(8).expect("the steps through hour 8 built");
    let frame = take_batch_frame(&mut side, 8);
    assert_eq!((frame.steps, frame.applied), (3, Some(8)), "hours 6, 7 and 8 in one frame");
    side.settle_all().unwrap();
    let pending = layer(&side).pending();
    let six = pending.iter().find(|made| made.at().1 == 6).expect("hour 6's interval");
    assert_eq!(shown(six).1, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None))], "decided from hour 6's rows: 102's general is not the viewer's yet");
}

#[test]
fn a_request_from_before_a_seek_back_never_steps_the_context_past_the_sought_hour() {
    let entry = open(record_with(|_| true, relinked), Settings { step_threads: 1, fetch_threads: 1, ahead: 4, ring: 6, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = stepped_to(&entry, true, 12);
    entry.playhead().seek(6);
    entry.playhead().wait_newest(8).expect("the new seek's steps through hour 8 built");
    side.settle_all().unwrap();
    assert_eq!(side.applied(), Some(12), "settling the old hour leaves the new seek for the next frame");
    frames_until(&mut side, 6, "the sought hour composed", |side| !side.composing() && layer(side).pending().last().is_some_and(|made| made.at().1 == 6));
    side.settle_all().unwrap();
    let made = layer(&side).pending().last().map(|made| (*made).clone()).unwrap();
    assert_eq!((made.at().1, made.seek()), (6, true), "the sought hour's interval");
    assert_eq!(shown(&made).1, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None))], "decided from hour 6's rows: 102's general is not the viewer's yet");
}

#[test]
fn an_interval_whose_tables_arrive_with_a_later_step_is_made_then_once() {
    let entry = open(record(|_| true), Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = stepped_to(&entry, false, 5);
    layer(&side).attach(Arc::clone(&entry));
    for hour in 6..=8 {
        frames_until(&mut side, hour, "a step an hour", |side| side.last_frame().applied == Some(hour));
        side.settle_all().unwrap();
    }
    let layer = layer(&side);
    assert_eq!(layer.marked()[2], 1, "hour 6's interval made on the worker once");
    let six = layer.pending().into_iter().find(|made| made.at().1 == 6).expect("hour 6's interval");
    assert_eq!(shown(six).1, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None))], "its events decided from rows, not refused");
}

#[test]
fn a_layer_composed_anew_while_steps_past_the_head_are_built_makes_the_heads_interval() {
    let side = play(record(|_| true), 1, 6, true, |_, _, _| {});
    side.entry().playhead().wait_newest(7).expect("a step past the head built");
    let (mirrors, layers) = side.parts().unwrap();
    let at = layers.iter().position(|layer| layer.any().is::<MapText>()).unwrap();
    let text = layers[at].any().downcast_ref::<MapText>().unwrap();
    let mut fresh = text.fresh();
    fresh.compose_with(mirrors, &Upstream::new(&layers[..at])).unwrap();
    assert_eq!(fresh.product().map(|made| made.at().1), Some(6), "the head's interval");
    assert_eq!(text.differ(&fresh), (0, None), "the same as the layer stepped there");
}

fn paradropped(at: u32, store: &mut Store) {
    let provinces = TableId::Provinces;
    if at == 0 {
        insert_whole(store, provinces, 0, 1);
        store.table(provinces.index()).unwrap().write(0, recorded::column(provinces, "controller"), Cell::U32(101)).unwrap();
    }
    if at == 7 {
        store.table(provinces.index()).unwrap().write(0, recorded::column(provinces, "controller"), Cell::U32(103)).unwrap();
    }
    if at == 6 {
        let landing = vec![MapEvent { sequence: 5, hour: 6, payload: MapEventPayload::ParadropLanding(MapEventParadrop { province: 1, country: 103, lost: false, fog: true }) }];
        store.table(TableId::MapEvents.index()).unwrap().write(0, recorded::column(TableId::MapEvents, "events"), Cell::Opaque(&opaque(&landing))).unwrap();
    }
}

#[test]
fn two_steps_in_one_frame_decide_the_first_steps_paradrop_by_its_own_hours_fog() {
    let entry = open(record_with(|_| true, paradropped), Settings { step_threads: 1, fetch_threads: 1, ahead: 4, ring: 6, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = stepped_to(&entry, true, 5);
    entry.playhead().wait_newest(7).expect("the steps through hour 7 built");
    let frame = take_batch_frame(&mut side, 7);
    assert_eq!((frame.steps, frame.applied), (2, Some(7)), "hours 6 and 7 in one frame");
    side.settle_all().unwrap();
    let vision = side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<vision::Vision>()).unwrap();
    assert_eq!(vision.province_byte(1), Some(0), "the settled vision, hour 7's, does not see province 1");
    let pending = layer(&side).pending();
    let six = pending.iter().find(|made| made.at().1 == 6).expect("hour 6's interval");
    assert_eq!(shown(six).1, [(5, Ok(Some(b"Landed".to_vec())))], "decided by hour 6's fog: the viewer sees the landing");
}

fn switched(at: u32, store: &mut Store) {
    if at == 7 {
        store.table(TableId::LocalPlayer.index()).unwrap().write(0, recorded::column(TableId::LocalPlayer, "country"), Cell::U32(102)).unwrap();
    }
}

#[test]
fn two_steps_in_one_frame_decide_the_first_steps_events_for_its_own_hours_viewer() {
    let entry = open(record_with(|_| true, switched), Settings { step_threads: 1, fetch_threads: 1, ahead: 4, ring: 6, speed: 1.0, rate: 1.0, ..Settings::default() }).unwrap();
    let mut side = stepped_to(&entry, true, 5);
    let ours = |side: &MapSide| side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<vision::Vision>()).unwrap().ours();
    let before = ours(&side);
    entry.playhead().wait_newest(7).expect("the steps through hour 7 built");
    let frame = take_batch_frame(&mut side, 7);
    assert_eq!((frame.steps, frame.applied), (2, Some(7)), "hours 6 and 7 in one frame");
    side.settle_all().unwrap();
    assert!(before.is_some() && ours(&side) != before, "the settled vision is hour 7's viewer, not hour 5's");
    let pending = layer(&side).pending();
    let six = pending.iter().find(|made| made.at().1 == 6).expect("hour 6's interval");
    assert_eq!(shown(six).1, [(5, Ok(Some(b"Captured".to_vec()))), (6, Ok(None))], "decided for hour 6's viewer, 101");
}

#[test]
fn the_worker_ends_with_the_layer() {
    let side = play(record(|_| true), 1, 6, true, |_, _, _| {});
    let alive = layer(&side).worker_alive().expect("hour 6's events started the worker");
    assert!(alive.load(Ordering::Acquire), "running while the layer stands");
    drop(side);
    while alive.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}
