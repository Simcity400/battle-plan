
use super::{Need, Recording, Store};
#[cfg(test)]
use super::{Pace, apart::Apart};
pub use crate::playhead_host::open;
use crate::{
    layers::{Carried, Inputs, Layer, Settling, View},
    playhead_host::{
        MapSide,
        mirror::{Mirror, Mirrors},
        units::Units,
    },
};
use playhead::{
    Settings, Step, Timeline,
    pages::{Pages, Run},
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::{Store as ColumnarStore, Value},
    unit::{BLOCK, BlockPage, Encoder, PartSink, Settings as Writing, reader::Extents},
};
use replay_data::{
    Hour,
    rows::{Field, List, TableId, TableRows},
};
#[cfg(test)]
use replay_data::{rows::tables, scope::CaptureMode};
use replay_frame::frame::TableView;
#[cfg(test)]
use replay_frame::frame::SharedView;
#[cfg(test)]
use std::collections::BTreeSet;
use std::{io, sync::Arc, time::Duration};


pub const COUNTRIES: TableId = TableId::Countries;
pub const COMBATS: TableId = TableId::LandCombats;
pub const STRINGS: [&[u8]; 3] = [b"GER", b"ENG", b"the collector could not read it"];
pub const RECORDING: Recording = Recording { branch: 7 };
pub const LIMIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Memory {
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

pub struct Recorded {
    bytes: Vec<u8>,
    runs: Vec<Run>,
    kinds: Vec<Vec<Kind>>,
}

impl Pages for Recorded {
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

#[derive(Clone)]
pub enum Put {
    Cell(Cell<'static>),
    Unrecorded,
}

impl Put {
    fn field(&self) -> Field<'static> {
        match self {
            Self::Unrecorded => Field::NotRecorded,
            Self::Cell(cell) => match *cell {
                Cell::U8(value) => Field::U8(value),
                Cell::I8(value) => Field::I8(value),
                Cell::U16(value) => Field::U16(value),
                Cell::I16(value) => Field::I16(value),
                Cell::U32(value) => Field::U32(value),
                Cell::I32(value) => Field::I32(value),
                Cell::U64(value) => Field::U64(value),
                Cell::I64(value) => Field::I64(value),
                Cell::Bool(value) => Field::Bool(value),
                Cell::OptionalI32(value) => Field::OptionalI32(value),
                Cell::OptionalU32(value) => Field::OptionalU32(value),
                Cell::OptionalI64(value) => Field::OptionalI64(value),
                Cell::OptionalU64(value) => Field::OptionalU64(value),
                Cell::Bytes(id) => Field::Bytes(STRINGS[id as usize]),
                Cell::U32List(items) => Field::U32List(List::Native(items)),
                Cell::U64List(items) => Field::U64List(List::Native(items)),
                Cell::Opaque(_) => Field::NotRecorded,
            },
        }
    }
}

pub fn country(key: u128, tag: u32, stability: i64, missing: bool) -> (u128, Vec<Put>) {
    let power = if missing { Cell::OptionalI64(None) } else { Cell::OptionalI64(Some(-25_000)) };
    let cells = vec![
        Put::Cell(Cell::U32(key as u32 * 2)),
        Put::Cell(Cell::Bytes(tag)),
        Put::Cell(power),
        Put::Cell(Cell::I64(1_000)),
        Put::Cell(Cell::I64(stability)),
        Put::Cell(Cell::I64(-3_000)),
        Put::Cell(Cell::OptionalI64(Some(12_000))),
        Put::Cell(Cell::U8(1)),
        Put::Cell(Cell::I32(-77)),
        if missing { Put::Unrecorded } else { Put::Cell(Cell::OptionalI32(Some(3))) },
    ];
    (key, cells)
}

pub fn combat(key: u128, day: i32) -> (u128, Vec<Put>) {
    static ATTACKERS: [u64; 3] = [u64::MAX, 4, 1 << 40];
    static CLASSES: [u32; 2] = [9, 0];
    let cells = vec![
        Put::Cell(Cell::U32(4_100)),
        Put::Cell(Cell::I32(12)),
        Put::Cell(Cell::I32(day)),
        Put::Cell(Cell::U64List(&ATTACKERS)),
        Put::Cell(Cell::U64List(&[])),
        Put::Cell(Cell::U32List(&CLASSES)),
        Put::Cell(Cell::U32List(&[])),
    ];
    (key, cells)
}

pub struct Hourly {
    countries: Status,
    country_rows: Vec<(u128, Vec<Put>)>,
    combats: Vec<(u128, Vec<Put>)>,
}

pub fn hourly(hour: u32) -> Hourly {
    let countries = match hour {
        4 => Status::Truncated { missing: 4 },
        5 => Status::Unavailable { reason: 2 },
        6 => Status::NotRecorded,
        _ => Status::Complete,
    };
    let stability = if hour >= 2 { 50_000 } else { 40_000 };
    Hourly {
        countries,
        country_rows: vec![country(9, 1, stability, false), country(2, 0, 10_000, true), country(5, 1, 0, false)],
        combats: vec![combat(30, hour as i32), combat(4, -(hour as i32))],
    }
}

pub fn write_rows(store: &mut ColumnarStore, table: TableId, rows: &[(u128, Vec<Put>)]) {
    let mut changes = store.table(table.index()).unwrap();
    for (slot, (key, cells)) in rows.iter().enumerate() {
        let slot = slot as u32;
        if changes.slot(*key).is_none() {
            changes.insert_at(slot, *key).unwrap();
        }
        for (column, put) in cells.iter().enumerate() {
            match put {
                Put::Cell(cell) => changes.write(slot, column, *cell).unwrap(),
                Put::Unrecorded => changes.put_value(slot, column, Value::Unrecorded).unwrap(),
            }
        }
    }
}

pub fn record(name: &str, hours: u32) -> Units {
    record_with(name, hours, |store, at| {
        let hour = hourly(at);
        store.status(COUNTRIES.index(), hour.countries, true, Some(at as i32), true).unwrap();
        if at == 0 || at == 2 {
            write_rows(store, COUNTRIES, &hour.country_rows);
        }
        store.status(COMBATS.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        write_rows(store, COMBATS, &hour.combats);
        for (table, rows) in [(TableId::CountryLinks, links()), (TableId::HumanControllers, humans())] {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
            if at == 0 {
                write_rows(store, table, &rows);
            }
        }
    })
}

pub fn record_with(name: &str, hours: u32, hour: impl FnMut(&mut ColumnarStore, u32)) -> Units {
    record_with_settings(name, hours, Writing::at(3), hour)
}

pub fn record_with_settings(name: &str, hours: u32, settings: Writing, hour: impl FnMut(&mut ColumnarStore, u32)) -> Units {
    let kinds = replay_recording::schema::schema().unwrap();
    record_with_kinds(name, hours, settings, kinds, hour)
}

pub fn record_with_kinds(name: &str, hours: u32, settings: Writing, kinds: Vec<Vec<Kind>>, mut hour: impl FnMut(&mut ColumnarStore, u32)) -> Units {
    let mut store = ColumnarStore::new(kinds.clone(), vec![false; kinds.len()]);
    store.attach(Encoder::new(&kinds, settings).unwrap());
    let mut memory = Memory::default();
    store.begin_run(0);
    for at in 0..hours {
        store.begin_hour(at as i32, if at == 0 { &STRINGS[..] } else { &[] }).unwrap();
        hour(&mut store, at);
        store.end_hour(&mut memory).unwrap();
        if at % BLOCK == BLOCK - 1 {
            store.end_block();
        }
    }
    if !hours.is_multiple_of(BLOCK) {
        store.end_run(&mut memory).unwrap();
    }
    let run = Run::new(0, memory.pages).unwrap();
    let kinds = Arc::new(kinds);
    let pages: Arc<dyn Pages> = Arc::new(Recorded { bytes: memory.bytes, runs: vec![run], kinds: (*kinds).clone() });
    let timeline = Timeline::whole(&*pages).unwrap();
    Units { name: format!("rows test {name}"), pages, timeline, kinds }
}

pub fn hourly_steps() -> Settings {
    Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }
}

pub fn codec(table: TableId, rows: &[(u128, Vec<Put>)]) -> TableRows {
    let mut sorted: Vec<&(u128, Vec<Put>)> = rows.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    let keys: Vec<u128> = sorted.iter().map(|(key, _)| *key).collect();
    table.rows_from_wire(&keys, &mut |row, column| Ok(sorted[row].1[column].field())).unwrap()
}



fn links() -> Vec<(u128, Vec<Put>)> {
    [(9, 18), (2, 4), (5, 10)].into_iter().map(|(id, slot)| (id, vec![Put::Cell(Cell::U32(slot))])).collect()
}

fn humans() -> Vec<(u128, Vec<Put>)> {
    vec![(9, vec![Put::Cell(Cell::I8(1))]), (5, vec![Put::Cell(Cell::I8(0))])]
}


#[test]
fn a_reader_of_some_columns_or_of_keys_alone_holds_only_those_and_any_other_field_is_refused_by_name() {
    use replay_data::rows::{CountryRow, mask};
    let entry = open(record("columns", 4), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(3);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let named = mask::<CountryRow>(&["stability_raw"]);
    let reader = apart.store().handle().reader_at("columns", &[Need::columns(COUNTRIES, named), Need::keys(COMBATS)], Pace::Step);
    assert!(apart.drive(3), "the store never reached hour 3");
    let frame = reader.wait(|frame| frame.hour == 3, LIMIT).expect("the rows of hour 3");
    let countries = frame.complete_fields::<tables::Countries>(named).unwrap();
    let mut sorted = hourly(3).country_rows;
    sorted.sort_by_key(|(key, _)| *key);
    assert_eq!(countries.iter().map(|row| u128::from(row.id)).collect::<Vec<_>>(), sorted.iter().map(|(key, _)| *key).collect::<Vec<_>>(), "every live row by key");
    for (row, (_, cells)) in countries.iter().zip(&sorted) {
        assert!(matches!(cells[4], Put::Cell(Cell::I64(value)) if value == row.stability_raw), "the named column is the recording's");
    }
    let (whole, missed) = replay_frame::frame::discovering_fields(|| frame.complete::<tables::Countries>().map(<[_]>::len));
    assert!(whole.is_err(), "a read of every field is never served: {whole:?}");
    assert_eq!(missed, vec![(COUNTRIES, u64::MAX)]);
    let tag = mask::<CountryRow>(&["tag"]);
    let (tagged, missed) = replay_frame::frame::discovering_fields(|| frame.complete_fields::<tables::Countries>(tag | named).map(<[_]>::len));
    assert!(tagged.is_err());
    assert_eq!(missed, vec![(COUNTRIES, tag | named)], "the fields asked for, to widen the need");
    let combats = frame.complete_fields::<tables::LandCombats>(0).unwrap();
    assert_eq!(combats.len(), 2, "keys alone: both live combats");
}

#[test]
fn a_view_of_declared_columns_holds_what_the_whole_view_holds_and_a_write_outside_it_makes_nothing() {
    use replay_data::rows::{LandCombatRow, mask};
    let (day, place) = (mask::<LandCombatRow>(&["day"]), mask::<LandCombatRow>(&["province", "duration"]));
    let column = replay_data::rows::column::<LandCombatRow>("day");
    let units = record_with("column sets", 8, |store, at| {
        for (table, rows) in [(COUNTRIES, hourly(0).country_rows), (TableId::CountryLinks, links()), (TableId::HumanControllers, humans())] {
            store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
            if at == 0 {
                write_rows(store, table, &rows);
            }
        }
        store.status(COMBATS.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        if at == 0 {
            write_rows(store, COMBATS, &hourly(0).combats);
            return;
        }
        let mut changes = store.table(COMBATS.index()).unwrap();
        for (slot, day) in [(0_u32, at as i32), (1, -(at as i32))] {
            changes.write(slot, column, Cell::I32(day)).unwrap();
        }
    });
    let entry = open(units, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(1);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let store = apart.store().handle();
    let (moving, still, whole) = (
        store.reader_at("day", &[Need::columns(COMBATS, day)], Pace::Step),
        store.reader_at("place", &[Need::columns(COMBATS, place)], Pace::Step),
        store.reader_at("whole", &[Need::every(COMBATS)], Pace::Step),
    );
    let mut held: Option<SharedView> = None;
    for hour in 1..7 {
        assert!(apart.drive(hour), "the store never reached hour {hour}");
        let at = |reader: &super::Reader| reader.wait(|frame| frame.hour == hour, LIMIT).unwrap_or_else(|error| panic!("hour {hour}: {error}"));
        let (moved, stood, every) = (at(&moving), at(&still), at(&whole));
        let all = every.complete::<tables::LandCombats>().unwrap();
        let days = moved.complete_fields::<tables::LandCombats>(day).unwrap();
        let places = stood.complete_fields::<tables::LandCombats>(place).unwrap();
        assert_eq!(days.iter().map(|row| (row.id, row.day)).collect::<Vec<_>>(), all.iter().map(|row| (row.id, row.day)).collect::<Vec<_>>(), "hour {hour}: the day column is the whole view's");
        assert_eq!(places.iter().map(|row| (row.id, row.province, row.duration)).collect::<Vec<_>>(), all.iter().map(|row| (row.id, row.province, row.duration)).collect::<Vec<_>>(), "hour {hour}: the place columns are the whole view's");
        let view = stood.tables[COMBATS.index()].clone();
        if let Some(before) = held.replace(view.clone()) {
            assert!(SharedView::ptr_eq(&before, &view), "hour {hour}: only the day was written, so the place columns' rows are the same allocation");
        }
        assert!(!SharedView::ptr_eq(&moved.tables[COMBATS.index()], &view), "hour {hour}: each set of columns has rows of its own");
    }
}

#[test]
fn a_table_the_recording_holds_nothing_of_comes_as_not_recorded_and_withholds_no_frame() {
    let entry = open(record("never held", 4), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(1);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let reader = apart.store().handle().reader_at("absent", &[Need::every(COMBATS), Need::every(TableId::Divisions)], Pace::Step);
    assert!(apart.drive(1), "the store never reached hour 1");
    let frame = reader.wait(|frame| frame.hour == 1, LIMIT).expect("the rows of hour 1");
    assert_eq!(*frame.tables[TableId::Divisions.index()], TableView::NotRecorded { mode: CaptureMode::Full }, "refused by name, not waited for");
    assert_eq!(*frame.tables[COMBATS.index()], TableView::Complete(codec(COMBATS, &hourly(1).combats)));
    assert!(reader.arriving().is_empty(), "nothing it declared is still awaited: {:?}", reader.arriving());
}

const DRAWN: TableId = TableId::Countries;

const MADE: TableId = TableId::LandCombats;

const KEYED: TableId = TableId::CountryLinks;

const LEARNED: TableId = TableId::HumanControllers;

struct Maker {
    reader: Option<super::Reader>,
    composes: u32,
    seen: Vec<(Option<Hour>, Option<(u64, Hour, usize)>)>,
    learn: bool,
}

impl Maker {
    fn new() -> Self {
        Self { reader: None, composes: 0, seen: Vec::new(), learn: false }
    }
}

impl Layer for Maker {
    fn name(&self) -> &'static str {
        "maker"
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(DRAWN, true)?;
        inputs.index(DRAWN, 0)
    }

    fn views(&mut self, store: &Store) -> io::Result<()> {
        self.reader = Some(store.reader("test", &[Need::every(MADE)]));
        Ok(())
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        self.composes += 1;
        Ok(())
    }

    fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}

    fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        let frame = self.reader.as_ref().and_then(super::Reader::frame).map(|frame| {
            let rows = match &*frame.tables[MADE.index()] {
                TableView::Complete(rows) => rows.len(),
                _ => usize::MAX,
            };
            (frame.branch, frame.hour, rows)
        });
        self.seen.push((mirrors.get(DRAWN).and_then(Mirror::head), frame));
        if let Some(reader) = &self.reader
            && self.learn
        {
            self.learn = false;
            reader.declare_only(&[MADE, LEARNED]);
        }
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

    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn at_hour(side: &MapSide, hour: Hour) -> bool {
    let _ = hour;
    side.parts().is_some() && maker(side).seen.last().is_some_and(|&(applied, frame)| matches!((applied, frame), (Some(applied), Some((_, hour, _))) if hour == applied))
}

fn maker(side: &MapSide) -> &Maker {
    side.parts().unwrap().1[0].any().downcast_ref::<Maker>().unwrap()
}

fn answered(side: &MapSide) -> Vec<(Hour, (u64, Hour, usize))> {
    maker(side).seen.iter().filter_map(|&(applied, frame)| applied.zip(frame)).collect()
}

fn until(side: &mut MapSide, hour: Hour) {
    for _ in 0..200_000u64 {
        let head = side.entry().playhead().newest().unwrap_or(0);
        side.frame(head).unwrap();
        let applied = side.applied();
        let stood = applied.is_some_and(|applied| at_hour(side, applied));
        if applied.is_some_and(|applied| applied >= hour) && !side.composing() && stood {
            return;
        }
        std::thread::yield_now();
    }
    panic!("the map side never applied hour {hour} with its maker at that hour");
}

fn side_of(name: &str, hours: u32) -> (Arc<crate::playhead_host::Entry>, MapSide) {
    let entry = open(record(name, hours), hourly_steps()).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Maker::new())], View::default()).unwrap();
    side.set_branch(RECORDING.branch);
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    (entry, side)
}

#[test]
fn a_maker_is_given_the_rows_of_the_hour_the_side_applied() {
    let (_entry, mut side) = side_of("views at the hour", 12);
    until(&mut side, 0);
    for _ in 0..200_000u64 {
        if side.applied().is_some_and(|applied| applied >= 6) && !side.composing() && at_hour(&side, side.applied().unwrap()) {
            break;
        }
        let head = side.entry().playhead().newest().unwrap_or(0).min(side.applied().unwrap_or(0) + 4);
        side.frame(head).unwrap();
        std::thread::yield_now();
    }
    let answers = answered(&side);
    assert!(!answers.is_empty(), "the maker was given a frame while the side stepped: {:?}", maker(&side).seen);
    for &(applied, (branch, hour, rows)) in &answers {
        assert!(hour <= applied, "a view is never made from mirrors past the hour applied: applied {applied}, frame {hour}");
        assert_eq!(branch, RECORDING.branch, "the frame carries the recording's branch");
        assert_ne!(rows, usize::MAX, "{MADE:?} is complete in the frame of hour {hour}");
    }
    let (applied, (_, hour, rows)) = *answers.last().expect("a frame");
    assert_eq!(hour, applied, "the last frame stands at the hour the side applied");
    assert!(rows > 0, "it holds the rows of the table only the maker reads");
    assert!(answers.iter().any(|&(applied, (_, hour, _))| hour == applied), "a frame of the hour applied");
}

#[test]
fn a_maker_reads_the_rows_again_after_a_seeks_compose() {
    let (entry, mut side) = side_of("views through a compose", 24);
    until(&mut side, 2);
    let before = answered(&side).len();
    entry.playhead().seek(18);
    until(&mut side, 18);
    let answers = answered(&side);
    assert!(answers.len() > before, "it was given a frame after the compose");
    let (applied, (_, hour, rows)) = *answers.last().expect("a frame");
    assert_eq!(hour, applied, "the frame after a compose stands at the hour applied");
    assert!(applied >= 18, "the side applied the hour sought: {applied}");
    assert!(rows > 0, "its rows are made whole from the mirrors rebuilt");
}


#[test]
fn a_maker_asking_for_other_keys_is_answered_at_the_hour_the_side_stands_at() {
    let (_entry, mut side) = side_of("views keyed at a held hour", 12);
    until(&mut side, 2);
    maker(&side).reader.as_ref().expect("its reader").declare_keyed(KEYED, Arc::new(BTreeSet::new()));
    let last = side.entry().playhead().newest().expect("the recording's hours");
    until(&mut side, last);
    for _ in 0..200_000u64 {
        side.frame(last).unwrap();
        if side.parts().is_some()
            && maker(&side).reader.as_ref().and_then(super::Reader::frame).is_some_and(|frame| !frame.tables[KEYED.index()].is_not_declared())
        {
            break;
        }
        std::thread::yield_now();
    }
    let reader = maker(&side).reader.as_ref().expect("its reader");
    let before = reader.frame().expect("a frame").hour;
    let keys: BTreeSet<u64> = (1..=2).collect();
    reader.declare_keyed(KEYED, Arc::new(keys.clone()));
    let (frame, held) = reader.frame_keyed().expect("a frame made for the keys asked");
    assert_eq!(frame.hour, before, "it is answered at the hour the side stands at, with no step between");
    assert_eq!(held.of(KEYED).map(|held| held.iter().copied().collect::<Vec<u64>>()), Some(keys.into_iter().collect::<Vec<u64>>()), "its frame's rows of {KEYED:?} were made for the keys it asked for");
    assert!(matches!(&*frame.tables[KEYED.index()], TableView::Complete(_)), "{KEYED:?} is complete in it: {:?}", frame.tables[KEYED.index()].held());
}

#[test]
fn a_maker_keeps_its_columns_when_it_reads_a_table_by_key() {
    let entry = open(record("views keyed columns", 4), hourly_steps()).unwrap();
    entry.playhead().seek(0);
    let mut fed = super::apart::Apart::new(&entry, RECORDING.branch).unwrap();
    let columns: u64 = 0b101;
    let reader = fed.store().reader("test", &[Need::columns(MADE, columns)]);
    reader.declare_keyed(KEYED, Arc::new((1..=2).collect()));
    assert!(fed.drive(2), "the store never reached hour 2");
    let frame = reader.frame().expect("a frame of the hour driven to");
    assert_eq!(frame.tables[MADE.index()].fields(), Some(columns), "its rows of {MADE:?} hold the columns it named and no other");
    assert!(!frame.tables[KEYED.index()].is_not_declared(), "the table it reads by key is in its frame");
}

#[test]
fn a_maker_asking_for_other_keys_during_a_same_hour_post_is_answered_by_that_post() {
    let entry = open(record("views keyed mid post", 4), hourly_steps()).unwrap();
    entry.playhead().seek(0);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let reader = Arc::new(apart.store().reader("test", &[Need::every(MADE)]));
    reader.declare_keyed(KEYED, Arc::new(BTreeSet::new()));
    assert!(apart.drive(3), "the store never reached hour 3");
    let (frame, held) = reader.frame_keyed().expect("a frame of hour 3");
    assert_eq!(frame.hour, 3);
    assert_eq!(held.of(KEYED).map(|held| held.len()), Some(0), "no keys asked yet");
    let keys: BTreeSet<u64> = links().iter().map(|&(key, _)| super::prefix(key)).collect();
    *apart.store().shared.chosen.lock().unwrap() = Some(Box::new({
        let (reader, keys) = (Arc::clone(&reader), keys.clone());
        move || reader.declare_keyed(KEYED, Arc::new(keys))
    }));
    assert!(apart.drive(3), "the store posted hour 3 again");
    assert!(apart.store().shared.chosen.lock().unwrap().is_none(), "the post asked for the keys");
    let (frame, held) = reader.frame_keyed().expect("a frame");
    assert_eq!(frame.hour, 3, "answered at the hour the store stands at");
    assert_eq!(held.of(KEYED).map(|held| held.iter().copied().collect::<Vec<u64>>()), Some(keys.into_iter().collect::<Vec<u64>>()), "its frame's rows of {KEYED:?} were made for the keys it asked for");
    match &*frame.tables[KEYED.index()] {
        TableView::Complete(rows) => assert_eq!(rows.len(), links().len(), "every country link's row"),
        other => panic!("{KEYED:?} is complete in its frame: {other:?}"),
    }
}

#[test]
fn keyed_refresh_waits_for_the_rows_and_change_marks_of_an_active_post() {
    let columns = replay_data::rows::mask::<replay_data::rows::CountryRow>(&["stability_raw"]);
    for after_telling in [false, true] {
        let entry = open(record("keyed refresh and row changes", 4), hourly_steps()).unwrap();
        entry.playhead().seek(0);
        let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
        let reader = Arc::new(apart.store().handle().telling("mesh inputs", &[Need::columns(COUNTRIES, columns)]));
        reader.declare_keyed(KEYED, Arc::new(BTreeSet::new()));
        assert!(apart.drive(0));
        assert!(apart.drive(1));
        let (frame, _, _) = reader.take_keyed().unwrap();
        let stability = |frame: &replay_frame::frame::DecodedFrame| {
            frame.complete_fields::<tables::Countries>(columns).unwrap().iter().find(|row| row.id == 9).unwrap().stability_raw
        };
        let cached = stability(&frame);
        assert_eq!(cached, 40_000);
        let published = reader.published();
        let (observed, during_build) = std::sync::mpsc::channel();
        let keys = Arc::new(BTreeSet::from([0]));
        *apart.store().shared.worker_built.lock().unwrap() = Some((after_telling, Box::new({
            let (reader, keys) = (Arc::clone(&reader), Arc::clone(&keys));
            move || {
                reader.declare_keyed(KEYED, keys);
                observed.send(reader.published()).unwrap();
            }
        })));

        assert!(apart.drive(2));
        assert_eq!(during_build.try_recv().unwrap(), published);
        assert_eq!(reader.published(), published + 1);
        let (frame, told, held) = reader.take_keyed().unwrap();
        assert_eq!(frame.hour, 2);
        assert_eq!(held.of(KEYED), Some(&keys));
        let updated = match told.get(&COUNTRIES) {
            Some(Some(rows)) if rows.get(&9).is_some_and(|changed| changed & columns != 0) => stability(&frame),
            Some(None) => stability(&frame),
            _ => cached,
        };
        assert_eq!(updated, 50_000, "the cache must see the changed row in the first publication");
    }
}

fn learned_at(side: &MapSide) -> Option<(Hour, usize)> {
    let frame = maker(side).reader.as_ref().and_then(super::Reader::frame)?;
    if frame.tables[LEARNED.index()].is_not_declared() {
        return None;
    }
    match &*frame.tables[LEARNED.index()] {
        TableView::Complete(rows) => Some((frame.hour, rows.len())),
        _ => None,
    }
}

fn until_learned(side: &mut MapSide, head: Option<Hour>, why: &str) -> (Hour, usize) {
    for _ in 0..200_000u64 {
        let at = head.unwrap_or_else(|| side.entry().playhead().newest().unwrap_or(0));
        side.frame(at).unwrap();
        if let Some(held) = learned_at(side) {
            return held;
        }
        std::thread::yield_now();
    }
    let arriving = maker(side).reader.as_ref().map(super::Reader::arriving);
    let mirror = side.parts().map(|(mirrors, _)| mirrors.get(LEARNED).is_some());
    panic!(
        "the maker never read {LEARNED:?} {why}: arriving {arriving:?}, mirror {mirror:?}, applied {:?}, newest {:?}, head {head:?}",
        side.applied(),
        side.entry().playhead().newest()
    );
}

#[test]
fn a_table_learned_while_playing_reads_at_the_hour_the_side_applied() {
    let (_entry, mut side) = side_of("views learned playing", 24);
    until(&mut side, 4);
    assert!(learned_at(&side).is_none(), "{LEARNED:?} is in no frame before a maker reads it");
    side.layers_mut()[0].any_mut().and_then(|layer| layer.downcast_mut::<Maker>()).expect("the maker").learn = true;
    let (hour, rows) = until_learned(&mut side, None, "while the side played");
    assert!(rows > 0, "the rows of {LEARNED:?} it learned");
    let applied = side.applied().expect("the side applied an hour");
    assert!(hour <= applied, "the frame holding {LEARNED:?} stands at an hour the side applied: {hour} against {applied}");
    until(&mut side, (hour + 2).min(23));
    let (after, _) = learned_at(&side).expect("it keeps reading what it learned");
    assert_eq!(Some(after), side.applied(), "once learned it stands at the hour the side applies, like every other table it reads");
}

#[test]
fn a_table_learned_while_a_stepped_frame_holds_the_head_reads_at_that_head() {
    let (entry, mut side) = side_of("views learned held", 24);
    until(&mut side, 6);
    entry.playhead().play(0.0);
    let held = side.applied().expect("the side applied an hour");
    let (mirrors, _) = side.parts().unwrap();
    assert!(mirrors.get(LEARNED).is_none(), "an undeclared table has no mirror");
    assert!(!mirrors.get(DRAWN).unwrap().holds(1), "an undeclared column has no mirror");
    side.layers_mut()[0].any_mut().and_then(|layer| layer.downcast_mut::<Maker>()).expect("the maker").learn = true;
    side.catch_up(held).unwrap();
    let (hour, rows) = learned_at(&side).expect("the whole settle delivered the late table");
    assert!(rows > 0, "the rows of {LEARNED:?} it learned");
    assert_eq!(hour, held, "it reads {LEARNED:?} at the hour the stepped frame holds, never at a newer one");

    let view = side.views().handle();
    let deferred = view.reader_later("brief late view", &[Need::every(LEARNED)], Pace::Step);
    maker(&side).reader.as_ref().unwrap().declare_only(&[MADE]);
    side.catch_up(held).unwrap();
    assert!(deferred.frame_of(held).is_some());
    drop(deferred);
    side.catch_up(held).unwrap();
    assert!(side.parts().unwrap().0.get(LEARNED).is_none(), "closing the final reader releases the mirror");
}

#[test]
fn a_typed_view_spare_keeps_only_the_next_patch_candidate() {
    let entry = open(record("spare lifetime", 4), hourly_steps()).unwrap();
    let width = entry.kinds()[COUNTRIES.index()].len();
    let columns: Vec<_> = (0..width).collect();
    let mut lane = playhead::lane::Lane::new(Arc::clone(&entry.pages), 0, COUNTRIES.index(), &columns, true).unwrap();
    lane.seek(0).unwrap();
    let mirror = Mirror::rebuild(&lane, &columns);
    let make = || SharedView::from(super::view::make(COUNTRIES, &mirror, (&entry.kinds()[COUNTRIES.index()], super::view::EVERY), false, &mut Vec::new()).0);
    let (first, second) = (make(), make());
    let mut made = super::Columns::new();
    let TableView::Complete(rows) = first.held() else { panic!("expected recorded countries") };
    made.rows = rows.len();
    made.keep(first.clone(), &[]);
    made.keep(second.clone(), &[]);

    assert!(!first.is_shared(), "the first snapshot can drop when its last reader lets go");
    assert!(second.is_shared(), "the predecessor can still supply the next patch");

    made.built = Some((made.stamp, make()));
    made.touched = Ok(super::Touched::default());
    made.make(COUNTRIES.index(), super::view::EVERY, (&mirror, &entry.kinds()[COUNTRIES.index()], false), &mut Vec::new(), &mut super::Made::default());
    assert!(!second.is_shared(), "a step that reuses the current view releases the spare");
}

#[test]
fn a_mirror_short_of_the_posted_hour_makes_no_frame_of_that_hour() {
    let entry = open(record("stalled short", 8), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(2);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let views = apart.store().handle();
    let reader = views.reader_at("short", &[Need::every(COMBATS)], Pace::Step);
    assert!(apart.drive(3), "the store never reached hour 3");
    reader.wait(|frame| frame.hour == 3, LIMIT).unwrap_or_else(|error| panic!("the rows of hour 3: {error}"));
    apart.post_at(4);
    assert_eq!(reader.frame().map(|frame| frame.hour), Some(3), "no frame of hour 4 from mirrors standing at 3");
    assert!(views.held().iter().any(|&(table, bytes)| table == COMBATS && bytes > 0));
    {
        let mut jobs = super::lock(&views.shared.jobs);
        jobs.made.told.push((COMBATS.index(), super::view::EVERY, Some(vec![(1, 1)])));
        jobs.running += 1;
    }
    drop(apart);
    assert!(views.held().is_empty(), "a handle outliving its store cannot keep the built views alive");
    assert!(reader.frame().is_none());
    let mut jobs = super::lock(&views.shared.jobs);
    assert!(jobs.made.told.is_empty(), "a retired store keeps no pending row changes");
    assert_eq!(jobs.running, 1, "a reader still republishing can finish after the store has retired");
    jobs.running -= 1;
}

#[test]
fn a_display_frames_reader_gives_the_newest_frame_no_later_than_the_hour_shown() {
    let entry = open(record("display through", 8), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(2);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let reader = apart.store().handle().reader_later("display", &[Need::every(COMBATS)], Pace::Step);
    for hour in [2, 3, 5] {
        assert!(apart.drive(hour), "the store never reached hour {hour}");
        reader.wait(|frame| frame.hour == hour, LIMIT).unwrap_or_else(|error| panic!("the rows of hour {hour}: {error}"));
    }
    let through = |hour: Hour| reader.frame_now_through(hour).map(|frame| frame.hour);
    assert_eq!(reader.frame_now().map(|frame| frame.hour), Some(5), "its newest frame is past the hour shown");
    assert_eq!(through(5), Some(5));
    let between = through(4);
    assert!(between.is_some_and(|hour| (3..=4).contains(&hour)), "the newest frame no later than the hour shown, older than the newest: {between:?}");
    assert_eq!(through(2), Some(2));
    assert_eq!(through(1), None, "no frame for an hour before every frame it was given");
    entry.playhead().seek(7);
    assert!(apart.drive(7), "the store never reached hour 7 after the seek");
    reader.wait(|frame| frame.hour == 7, LIMIT).unwrap_or_else(|error| panic!("the rows of hour 7: {error}"));
    assert_eq!(through(7), Some(7));
    assert_eq!(through(5), None, "the frame of hour 5 is of the history before the seek");
}

#[test]
fn recorded_unavailable_with_head_wording_emits_no_typed_head_failure() {
    use super::view::{HeadFailure, HeadFailureKind};
    let wording = HeadFailure {
        table: COUNTRIES,
        kind: HeadFailureKind::BuildError(format!("{COUNTRIES:?} is held without keys")),
    }
    .format();
    let kinds = replay_recording::schema::schema().unwrap();
    let mut store = ColumnarStore::new(kinds.clone(), vec![false; kinds.len()]);
    store.attach(Encoder::new(&kinds, Writing::at(3)).unwrap());
    let mut memory = Memory::default();
    store.begin_run(0);
    let strings: [&[u8]; 1] = [wording.as_bytes()];
    store.begin_hour(0, &strings).unwrap();
    store.status(COUNTRIES.index(), Status::Unavailable { reason: 0 }, true, Some(0), true).unwrap();
    store.end_hour(&mut memory).unwrap();
    store.end_run(&mut memory).unwrap();
    let run = Run::new(0, memory.pages).unwrap();
    let kinds = Arc::new(kinds);
    let pages: Arc<dyn Pages> = Arc::new(Recorded { bytes: memory.bytes, runs: vec![run], kinds: (*kinds).clone() });
    let timeline = Timeline::whole(&*pages).unwrap();
    let units = Units { name: "rows test head wording".into(), pages, timeline, kinds };

    let entry = open(units, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(0);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let reader = apart.store().handle().reader_at("wording", &[Need::every(COUNTRIES)], Pace::Step);
    assert!(apart.drive(0), "the store never reached hour 0");
    let frame = reader.wait(|frame| frame.hour == 0, LIMIT).expect("hour 0");
    assert_eq!(&*frame.tables[COUNTRIES.index()], &TableView::Unavailable { reason: wording.clone() });
    let (latest, failures) = reader.frame_with_failures().expect("latest frame");
    assert_eq!(latest.hour, 0);
    assert_eq!(&*latest.tables[COUNTRIES.index()], &TableView::Unavailable { reason: wording });
    assert!(failures.is_empty(), "a recorded unavailability carries no typed head failure: {failures:?}");
}

#[test]
fn head_row_build_error_emits_typed_build_error_failure() {
    use super::view::{self, HeadFailure, HeadFailureKind};
    let units = record("head build error", 1);
    let mut lane = playhead::lane::Lane::new(Arc::clone(&units.pages), 0, COUNTRIES.index(), &[], false).unwrap();
    lane.seek(0).unwrap();
    let mirror = Mirror::rebuild(&lane, &[]);
    let (view, made) = view::make(COUNTRIES, &mirror, (&[], view::EVERY), false, &mut Vec::new());
    let expected = HeadFailureKind::BuildError(format!("{COUNTRIES:?} is held without keys"));
    assert_eq!(made.failure, Some(expected.clone()));
    assert_eq!(
        view,
        TableView::Unavailable {
            reason: HeadFailure { table: COUNTRIES, kind: expected }.format(),
        }
    );
}

#[test]
fn keyed_view_replacement_clears_whole_view_failure_for_same_table() {
    use super::view::HeadFailureKind;
    let table = COMBATS;
    let valid_key = 1u128 << 32;
    let broken_key = 2u128 << 32;
    let valid = combat(valid_key, 0);
    let mut broken = combat(broken_key, 0);
    broken.1[0] = Put::Unrecorded;
    let units = record_with("keyed replaces whole failure", 1, |store, at| {
        store.status(table.index(), Status::Complete, true, Some(at as i32), true).unwrap();
        write_rows(store, table, &[valid.clone(), broken.clone()]);
    });
    let entry = open(units, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    entry.playhead().seek(0);
    let mut apart = Apart::new(&entry, RECORDING.branch).unwrap();
    let whole = apart.store().handle().reader_at("whole", &[Need::every(table)], Pace::Step);
    let both = apart.store().handle().reader_at("both", &[Need::every(table)], Pace::Step);
    both.declare_keyed(table, Arc::new(BTreeSet::from([super::prefix(valid_key)])));
    assert!(apart.drive(0), "the store never reached hour 0");
    let _ = whole.wait(|frame| frame.hour == 0, LIMIT).expect("whole hour 0");
    let _ = both.wait(|frame| frame.hour == 0, LIMIT).expect("both hour 0");

    let (whole_frame, whole_failures) = whole.frame_with_failures().expect("whole frame");
    assert!(
        matches!(&*whole_frame.tables[table.index()], TableView::Unavailable { .. }),
        "whole view including malformed row must fail: {:?}",
        whole_frame.tables[table.index()]
    );
    assert!(
        whole_failures
            .iter()
            .any(|failure| failure.table == table && matches!(failure.kind, HeadFailureKind::BuildError(_))),
        "whole view must record a BuildError: {whole_failures:?}"
    );

    let (frame, failures) = both.frame_with_failures().expect("both frame");
    match &*frame.tables[table.index()] {
        TableView::Complete(rows) => assert_eq!(rows.len(), 1),
        other => panic!("keyed selection excluding malformed row must be complete: {other:?}"),
    }
    assert!(
        failures.is_empty(),
        "successful keyed replacement must not retain whole-view failure: {failures:?}"
    );
}

#[cfg(test)]
fn recording_without_divisions(name: &str) -> Units {
    use playhead::pages::EntryRef;

    struct OlderRecording(Arc<dyn Pages>);

    impl Pages for OlderRecording {
        fn extents(&self, run: usize) -> &dyn Extents { self.0.extents(run) }
        fn runs(&self) -> &[Run] { self.0.runs() }
        fn kinds(&self, table: usize) -> Option<&[Kind]> { self.0.kinds(table) }

        fn table_entry(&self, run: usize, block: usize, table: usize) -> io::Result<EntryRef> {
            if table == TableId::Divisions.index() { Ok(EntryRef::empty()) } else { self.0.table_entry(run, block, table) }
        }
    }

    let mut units = record(name, 1);
    units.pages = Arc::new(OlderRecording(units.pages));
    units
}

#[test]
fn posting_after_a_selection_change_does_not_publish_the_same_reply_twice() {
    use std::sync::Mutex;

    let table = TableId::Divisions;
    let entry = open(recording_without_divisions("selected reply post race"), hourly_steps()).unwrap();
    let mut store = Store::new(&entry);
    let reader = store.reader("selected reply post race", &[Need::every(table)]);
    let mirrors = Arc::new(Mirrors::default());
    reader.declare_keyed(table, Arc::new(BTreeSet::new()));
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    assert!(reader.frame().is_some(), "the initial empty selection was published");
    let published = reader.published();
    let store = Arc::new(Mutex::new(store));
    let weak = Arc::downgrade(&store);
    store.lock().unwrap().set_wake(Arc::new(move || {
        if let Some(store) = weak.upgrade() {
            store.lock().unwrap().post(&mirrors, 0, RECORDING.branch, 0, false);
        }
    }));
    reader.declare_keyed(table, Arc::new(BTreeSet::from([1])));
    assert_eq!(reader.published(), published + 1);
    assert_eq!(reader.frame_keyed().unwrap().1.of(table).unwrap().as_ref(), &BTreeSet::from([1]));

    let shared = Arc::clone(&reader.shared);
    let id = reader.id;
    *store.lock().unwrap().shared.chosen.lock().unwrap() = Some(Box::new(move || super::republish(&shared, id)));
    reader.declare_keyed(table, Arc::new(BTreeSet::from([1, 2])));
    assert_eq!(reader.published(), published + 2);
    drop(store);
    drop(reader);
}

#[test]
fn an_unrecorded_selection_is_answered_once() {
    let entry = open(recording_without_divisions("unrecorded selection"), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut store = Store::new(&entry);
    let reader = store.reader("unrecorded divisions", &[Need::every(TableId::Divisions)]);
    let keys = Arc::new(BTreeSet::from([1]));
    reader.declare_keyed(TableId::Divisions, Arc::clone(&keys));
    let mirrors = Arc::new(Mirrors::default());
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    let (frame, held) = reader.frame_keyed().unwrap();
    assert_eq!(*frame.tables[TableId::Divisions.index()], TableView::NotRecorded { mode: CaptureMode::Full });
    assert_eq!(held.of(TableId::Divisions), Some(&keys));
    let published = reader.published();
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    assert_eq!(reader.published(), published);
}

#[test]
fn an_exact_column_uses_the_same_failure_and_key_publication_as_a_mirror() {
    use super::{SCRIPT_VALUES as table, view::HeadFailureKind};
    let good = 1u128 << 32;
    let units = record_with("exact column publication", 1, |store, _| {
        store.status(table.index(), Status::Complete, true, Some(0), true).unwrap();
        write_rows(store, table, &[(good, vec![Put::Cell(Cell::I64(7))]), (2u128 << 32, vec![Put::Unrecorded])]);
    });
    let entry = open(units, hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let mut store = Store::new(&entry);
    let whole = store.reader("explicit whole", &[Need::every(table)]);
    let selected = store.reader("explicit selection", &[Need::every(table)]);
    let mirrors = Arc::new(Mirrors::default());
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    let published = selected.published();
    selected.declare_keyed(table, Arc::new(BTreeSet::from([super::prefix(good)])));
    assert_eq!(selected.published(), published + 1);
    let (frame, failures) = whole.frame_with_failures().unwrap();
    assert!(matches!(&*frame.tables[table.index()], TableView::Unavailable { .. }));
    assert!(matches!(&failures[..], [failure] if failure.table == table && matches!(failure.kind, HeadFailureKind::BuildError(_))));
    let refused_bytes = super::view_bytes(&frame.tables[table.index()]);
    let (frame, failures) = selected.frame_with_failures().unwrap();
    assert!(failures.is_empty());
    assert_eq!(frame.complete::<tables::ScriptVariableValues>().unwrap()[0].value_raw, 7);
    let held = store.held();
    assert_eq!(held.iter().find(|&&(id, _)| id == table).unwrap().1, refused_bytes + super::view_bytes(&frame.tables[table.index()]));
    let (_, published) = selected.newest().unwrap();
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    assert_eq!(selected.newest().unwrap().1, published);
}

#[test]
fn an_exact_selection_carries_changed_values_even_when_the_selected_keys_expand() {
    use super::SCRIPT_VALUES as table;

    let units = record_with("exact column changes", 2, |store, at| {
        store.status(table.index(), Status::Complete, true, Some(at as Hour), true).unwrap();
        write_rows(store, table, &[(1u128 << 32, vec![Put::Cell(Cell::I64(-20 + i64::from(at)))]), (2u128 << 32, vec![Put::Cell(Cell::I64(4))])]);
    });
    let entry = open(units, hourly_steps()).unwrap();
    let mut store = Store::new(&entry);
    let reader = Arc::new(store.handle().telling("explicit selection with changes", &[Need::every(table)]));
    reader.declare_keyed(table, Arc::new(BTreeSet::from([1])));
    let mirrors = Arc::new(Mirrors::default());
    store.post(&mirrors, 0, RECORDING.branch, 0, false);
    let (frame, _, _) = reader.take_keyed().unwrap();
    assert_eq!(frame.complete::<tables::ScriptVariableValues>().unwrap()[0].value_raw, -20);

    let expanded = Arc::new(BTreeSet::from([1, 2]));
    *store.shared.chosen.lock().unwrap() = Some(Box::new({
        let (reader, keys) = (Arc::clone(&reader), Arc::clone(&expanded));
        move || reader.declare_keyed(table, keys)
    }));
    store.post(&mirrors, 1, RECORDING.branch, 0, false);
    let (frame, told, keys) = reader.take_keyed().unwrap();
    assert_eq!(frame.hour, 1);
    assert_eq!(keys.of(table), Some(&expanded));
    let rows = frame.complete::<tables::ScriptVariableValues>().unwrap();
    assert_eq!(rows.iter().map(|row| row.value_raw).collect::<Vec<_>>(), vec![-19, 4]);
    assert_eq!(told.get(&table), Some(&None));
}
