use super::{Held, Scripts};
use viewer_layer_core::layers::{Layer, Settling};
use viewer_layer_core::playhead_host::{open, units::Units, views::{Need, Recording, Views, apart::Apart}};
use dashboard_text::triggers::World;
use content_reader_sound::music::{Music, Song, Track};
use playhead::{
    Settings, Timeline,
    pages::{Pages, Run},
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::{Store, Value},
    unit::{BLOCK, BlockPage, Encoder, PartSink, Settings as Writing, reader::Extents},
};
use replay_data::{
    Hour,
    rows::{Field, List, Opaque, RowCodec, RowsVisit, TableId, TableSchema, WarRelationRow, tables},
    scope::CaptureMode,
};
use replay_frame::frame::{DecodedFrame, TableView};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use viewer_foundation::{perspective, trigger_content::TriggerContent};
use viewer_raids::map::raids::{self, test_support};

const RECORDING: Recording = Recording { branch: 3 };
const HOURS: u32 = 120;

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

struct Recorded {
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

struct Strings<'a>(&'a mut BTreeMap<Vec<u8>, u32>);

impl RowsVisit for Strings<'_> {
    type Output = ();
    fn rows<R: RowCodec>(self, rows: &[R]) {
        for row in rows {
            row.visit_wire(&mut |_, field| {
                if let Field::Bytes(bytes) = field {
                    let next = self.0.len() as u32;
                    self.0.entry(bytes.to_vec()).or_insert(next);
                }
                Ok(())
            })
            .unwrap();
        }
    }
}

struct Put<'a> {
    store: &'a mut Store,
    table: TableId,
    strings: &'a BTreeMap<Vec<u8>, u32>,
}

impl RowsVisit for Put<'_> {
    type Output = ();
    fn rows<R: RowCodec>(self, rows: &[R]) {
        let mut changes = self.store.table(self.table.index()).unwrap();
        let mut opaque = Vec::new();
        for (slot, row) in rows.iter().enumerate() {
            let slot = slot as u32;
            changes.insert_at(slot, row.key()).unwrap();
            row.visit_wire(&mut |column, field| {
                match field {
                    Field::NotRecorded => changes.put_value(slot, column, Value::Unrecorded),
                    Field::Empty => changes.put_value(slot, column, Value::Missing),
                    Field::Opaque(Opaque::Encoded(bytes, _)) => changes.write(slot, column, Cell::Opaque(bytes)),
                    Field::Opaque(Opaque::Native(value)) => {
                        opaque.clear();
                        value.encode_value(&mut opaque)?;
                        changes.write(slot, column, Cell::Opaque(&opaque))
                    }
                    field => changes.write(slot, column, cell(field, self.strings)),
                }
            })
            .unwrap();
        }
    }
}

fn cell<'a>(field: Field<'a>, strings: &BTreeMap<Vec<u8>, u32>) -> Cell<'a> {
    match field {
        Field::U8(value) => Cell::U8(value),
        Field::I8(value) => Cell::I8(value),
        Field::U16(value) => Cell::U16(value),
        Field::I16(value) => Cell::I16(value),
        Field::U32(value) => Cell::U32(value),
        Field::I32(value) => Cell::I32(value),
        Field::U64(value) => Cell::U64(value),
        Field::I64(value) => Cell::I64(value),
        Field::Bool(value) => Cell::Bool(value),
        Field::OptionalI32(value) => Cell::OptionalI32(value),
        Field::OptionalU32(value) => Cell::OptionalU32(value),
        Field::OptionalI64(value) => Cell::OptionalI64(value),
        Field::OptionalU64(value) => Cell::OptionalU64(value),
        Field::Bytes(bytes) => Cell::Bytes(strings[bytes]),
        Field::U32List(List::Native(items)) => Cell::U32List(items),
        Field::U64List(List::Native(items)) => Cell::U64List(items),
        _ => panic!("the fixture holds a field the test cannot write"),
    }
}

fn record(name: &str, frame: &DecodedFrame) -> Units {
    let kinds = replay_recording::schema::schema().unwrap();
    let mut strings = BTreeMap::new();
    for view in &frame.tables {
        match &**view {
            TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.visit(Strings(&mut strings)),
            TableView::Unavailable { reason } => {
                let next = strings.len() as u32;
                strings.entry(reason.as_bytes().to_vec()).or_insert(next);
            }
            TableView::NotRecorded { .. } => {}
        }
    }
    let mut named: Vec<(&Vec<u8>, &u32)> = strings.iter().collect();
    named.sort_by_key(|(_, id)| **id);
    let named: Vec<&[u8]> = named.into_iter().map(|(bytes, _)| bytes.as_slice()).collect();
    let mut store = Store::new(kinds.clone(), vec![false; kinds.len()]);
    store.attach(Encoder::new(&kinds, Writing::at(3)).unwrap());
    let mut memory = Memory::default();
    store.begin_run(0);
    for at in 0..HOURS {
        store.begin_hour(at as i32, if at == 0 { &named } else { &[] }).unwrap();
        for table in TableId::ALL {
            let view = &*frame.tables[table.index()];
            let status = match view {
                TableView::Complete(_) => Status::Complete,
                TableView::Truncated { missing, .. } => Status::Truncated { missing: *missing },
                TableView::Unavailable { reason } => Status::Unavailable { reason: strings[reason.as_bytes()] },
                TableView::NotRecorded { .. } => Status::NotRecorded,
            };
            store.status(table.index(), status, true, Some(at as i32), true).unwrap();
            if at == 0
                && let TableView::Complete(rows) | TableView::Truncated { rows, .. } = view
            {
                rows.visit(Put { store: &mut store, table, strings: &strings });
            }
        }
        store.end_hour(&mut memory).unwrap();
        if at % BLOCK == BLOCK - 1 {
            store.end_block();
        }
    }
    if !HOURS.is_multiple_of(BLOCK) {
        store.end_run(&mut memory).unwrap();
    }
    let run = Run::new(0, memory.pages).unwrap();
    let kinds = Arc::new(kinds);
    let pages: Arc<dyn Pages> = Arc::new(Recorded { bytes: memory.bytes, runs: vec![run], kinds: (*kinds).clone() });
    let timeline = Timeline::whole(&*pages).unwrap();
    Units { name: format!("scripts test {name}"), pages, timeline, kinds }
}

fn hourly_steps() -> Settings {
    Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 1.0, rate: 1.0, ..Settings::default() }
}

fn music() -> Arc<viewer_audio::Content> {
    let mut music = Music::default();
    for (index, body) in ["base=1", "base=7 modifier={has_war_with=ENG factor=2}"].into_iter().enumerate() {
        music.tracks.push(Track { name: format!("t{index}"), path: format!("music/t{index}.ogg"), volume: 1.0, source: String::new() });
        music.songs.push(Song { track: index, station: "on".into(), chance: content_reader_parser::parser::parse(body.as_bytes()).unwrap().items, source: String::new() });
    }
    Arc::new(viewer_audio::Content::new(Default::default(), None).with_music(music, Some(vec!["on".into()]), 0))
}

fn fixture() -> DecodedFrame {
    let mut frame = test_support::frame();
    test_support::put::<tables::WarRelations>(&mut frame, vec![WarRelationRow { first_country: 10, second_country: 11, start_hour: 0, end_hour: i32::MAX, ..Default::default() }]);
    frame.branch = RECORDING.branch;
    frame
}

struct Fixture {
    fed: Apart,
    views: Views,
    raids: Arc<raids::RaidAssets>,
    triggers: Arc<TriggerContent>,
    audio: Arc<viewer_audio::Content>,
    tables: engine_executable::executable_tables::Tables,
    hour: Hour,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let tables = content_reader_script::test_game::enter();
        let mut raids = test_support::unnamed_assets();
        test_support::set_counts(&mut raids);
        let raids = Arc::new(raids);
        let triggers = Arc::new(raids.trigger_content.clone());
        let entry = open(record(name, &fixture()), hourly_steps()).unwrap();
        entry.set_capture(CaptureMode::Full);
        entry.playhead().seek(1);
        let mut fed = Apart::new(&entry, RECORDING.branch).unwrap();
        assert!(fed.drive(1), "the store never reached hour 1");
        let views = fed.store().handle();
        Self { fed, views, raids, triggers, audio: music(), tables, hour: 1 }
    }

    fn content(&self) -> Held {
        Held { raids: Arc::clone(&self.raids), triggers: Arc::clone(&self.triggers), audio: Some(Arc::clone(&self.audio)), tables: Some(self.tables) }
    }

    fn scripts(&self, needs: &[Need]) -> Scripts {
        let mut scripts = Scripts::with(self.content(), needs.to_vec());
        scripts.views = Some(self.views.clone());
        scripts.shown = (true, true);
        scripts.applied = Some(self.hour);
        scripts.clock = super::Clock::Stepped(Duration::ZERO);
        scripts.block_catch_up = true;
        scripts
    }

    fn posted(&mut self) {
        assert!(self.fed.drive(self.hour), "the store never posted hour {}", self.hour);
    }

    fn step(&mut self) {
        self.hour += 1;
        assert!(self.fed.drive(self.hour), "the store never reached hour {}", self.hour);
    }

    fn held(&self) -> Vec<TableId> {
        self.fed.store().held().iter().map(|(table, _)| *table).collect()
    }

}

fn today(fixture: &Fixture, hour: Hour, ours: u32) -> (String, String) {
    engine_executable::executable_tables::scope(Some(fixture.tables), || today_in(fixture, hour, ours))
}

fn read_by(fixture: &Fixture, ours: u32) -> Vec<TableId> {
    let mut found: Vec<TableId> = Vec::new();
    loop {
        let mut frame = self::fixture();
        for table in TableId::ALL.into_iter().filter(|table| !found.contains(table)) {
            frame.tables[table.index()] = replay_frame::frame::not_declared(table);
        }
        let (_, missed) = engine_executable::executable_tables::scope(Some(fixture.tables), || {
            replay_frame::frame::discovering(|| {
                let _ = super::ours(&frame, &perspective::Choice::default());
                let paths = raids::paths::HourPaths::new(&fixture.raids, &frame);
                let _ = raids::icons(&fixture.raids, &frame, ours, &paths);
                let world = World::read(&frame, fixture.triggers.facts());
                let _ = map_products::music_inputs::Memo::new().chances(&frame, &fixture.audio, ours, &world);
            })
        });
        if missed.is_empty() {
            return found;
        }
        found.extend(missed);
    }
}

fn today_in(fixture: &Fixture, hour: Hour, ours: u32) -> (String, String) {
    let mut frame = self::fixture();
    frame.hour = hour;
    let paths = raids::paths::HourPaths::new(&fixture.raids, &frame);
    let icons = raids::icons(&fixture.raids, &frame, ours, &paths);
    let world = World::read(&frame, fixture.triggers.facts());
    let music = map_products::music_inputs::Memo::new().chances(&frame, &fixture.audio, ours, &world);
    (format!("{:?} {:?}", icons.icons, icons.report), format!("{:?} {:?}", music.weights, music.refusals))
}

fn asked(scripts: &mut Scripts, settling: Settling) -> io::Result<()> {
    let mirrors = viewer_layer_core::playhead_host::mirror::Mirrors::default();
    let upstream = viewer_layer_core::layers::Upstream::new(&[]);
    assert!(Layer::ask(scripts, &mirrors, &upstream)?, "the scripts join a maker of their own");
    Layer::join(scripts, &mirrors, &upstream, settling)
}

fn step(fixture: &mut Fixture, scripts: &mut Scripts, settling: Settling) -> io::Result<()> {
    scripts.ensure_open();
    fixture.step();
    scripts.applied = Some(fixture.hour);
    asked(scripts, settling)
}

fn published(scripts: &Scripts, ours: u32, hour: Hour) -> Option<(String, String)> {
    let icons = scripts.raids(RECORDING.branch, ours).filter(|icons| icons.built_for.is_some_and(|(_, at, _)| at == hour) && icons.report.cycle_complete())?;
    let (_, music) = scripts.music_at().filter(|(at, _)| *at == hour)?;
    Some((format!("{:?} {:?}", icons.icons, icons.report), format!("{:?} {:?}", music.weights, music.refusals)))
}

#[test]
fn the_script_readers_read_rows_at_the_head_learning_their_tables_and_close_with_the_view() {
    let mut fixture = Fixture::new("readers");
    let ours = super::ours(&self::fixture(), &perspective::Choice::default()).unwrap();
    let read = read_by(&fixture, ours);
    assert!(read.contains(&tables::WarRelations::ID), "{read:?}");
    let primed: Vec<TableId> = read.iter().copied().filter(|&table| table != tables::WarRelations::ID).collect();
    let mut scripts = fixture.scripts(&primed.iter().map(|&table| Need::keys(table)).collect::<Vec<Need>>());
    assert!(scripts.raids(RECORDING.branch, ours).is_none() && scripts.music_at().is_none(), "nothing while the tables load: the window shows today's");
    let reached = loop {
        let tables: Vec<TableId> = scripts.needs().iter().map(|need| need.table).collect();
        assert!(fixture.hour + 2 < HOURS as Hour, "the recording ran out of hours before the scripts evaluated: {tables:?}");
        step(&mut fixture, &mut scripts, Settling::Whole).unwrap();
        assert_eq!(scripts.stats().ran_out, 0, "deterministic catch-up must never run out");
        if let Some(reached) = published(&scripts, ours, fixture.hour) {
            break reached;
        }
    };
    let learned: Vec<TableId> = scripts.needs().iter().map(|need| need.table).collect();
    assert!(learned.contains(&tables::WarRelations::ID) && learned.contains(&tables::RaidTargetStatuses::ID), "learned by reading: {learned:?}");
    assert_eq!(reached, today(&fixture, fixture.hour, ours), "the same as today's evaluation over today's frame of the hour");
    assert!(fixture.held().contains(&tables::WarRelations::ID), "the store keeps the views the scripts read");
    scripts.shown = (false, false);
    if let Some(closing) = scripts.close() {
        closing.join().expect("the scripts' closing thread");
    }
    assert!(!fixture.held().contains(&tables::WarRelations::ID), "closed, the scripts' reader lets its tables go");
    assert!(scripts.raids(RECORDING.branch, ours).is_none() && scripts.music_at().is_none(), "closed, it holds nothing");
    fixture.step();
    assert!(fixture.held().is_empty(), "no reader is left, so the store keeps no view: {:?}", fixture.held());
    let mut stopped = fixture.scripts(&primed.iter().map(|&table| Need::every(table)).collect::<Vec<Need>>());
    stopped.ensure_open();
    let (dead_send, dead_done) = viewer_runtime::landed::channel();
    drop(dead_send);
    if let Some(open) = stopped.open.as_mut() {
        open.done = dead_done;
        open.busy = true;
    }
    let err = asked(&mut stopped, Settling::Whole).unwrap_err().to_string();
    assert!(err.contains("worker stopped"), "{err}");
}

#[test]
fn a_stopped_picture_shows_the_scripts_product_of_its_own_hour() {
    let mut fixture = Fixture::new("stopped");
    let ours = super::ours(&self::fixture(), &perspective::Choice::default()).unwrap();
    let primed: Vec<Need> = read_by(&fixture, ours).iter().map(|&table| Need::every(table)).collect();
    let mut scripts = fixture.scripts(&primed);
    let mut stopped = 0;
    while stopped < 3 {
        assert!(fixture.hour + 2 < HOURS as Hour, "the recording ran out of hours");
        step(&mut fixture, &mut scripts, Settling::Whole).unwrap();
        assert_eq!(scripts.stats().ran_out, 0, "deterministic catch-up must never run out");
        let Some(reached) = published(&scripts, ours, fixture.hour) else { continue };
        assert_eq!(reached, today(&fixture, fixture.hour, ours), "hour {}: the stopped picture is today's evaluation of its own hour", fixture.hour);
        assert_eq!(viewer_layer_core::layers::Layer::published(&scripts), viewer_layer_core::layers::Published::Current, "its product stands at the rows it has seen");
        stopped += 1;
    }
    let held = fixture.hour;
    let waited = scripts.stats().waited;
    let (pending, no_answer) = viewer_runtime::landed::channel();
    drop(pending);
    let open = scripts.open.as_mut().unwrap();
    let done = std::mem::replace(&mut open.done, no_answer);
    step(&mut fixture, &mut scripts, Settling::step()).unwrap();
    let open = scripts.open.as_mut().unwrap();
    assert!(open.busy, "the new hour is in flight");
    open.done = done;
    assert_eq!(scripts.stats().waited, waited);
    assert_eq!(Layer::published(&scripts), crate::scripts::Published::Cadence(held));
    assert_eq!(published(&scripts, ours, held), Some(today(&fixture, held, ours)));
    assert!(scripts.stats().rated > 0, "the music chances' rate withheld reads of them while the hours ran");
    let withheld = scripts.stats().rated;
    assert!(!scripts.music_due(false), "inside its interval the rate withholds the chances");
    assert_eq!(scripts.stats().rated, withheld + 1);
    scripts.clock.step(super::MUSIC_RATE.interval());
    assert!(scripts.music_due(false), "its interval elapsed, the chances are read again");
    assert_eq!(scripts.stats().rated, withheld + 1, "and nothing more is withheld");
    step(&mut fixture, &mut scripts, Settling::Whole).unwrap();
    let stopped_at = fixture.hour;
    scripts.applied = Some(stopped_at);
    fixture.posted();
    asked(&mut scripts, Settling::Whole).unwrap();
    assert_eq!(scripts.stats().ran_out, 0);
    assert_eq!(published(&scripts, ours, stopped_at), Some(today(&fixture, stopped_at, ours)), "the picture the hours stopped on is its own hour's");
}
