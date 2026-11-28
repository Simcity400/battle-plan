use super::{Entry, MapSide, Opening};
#[cfg(test)]
use super::{OPENING, open, views::tests::{hourly_steps, record}};
use crate::layers::{Carried, Settling, Inputs, Layer, View};
use crate::playhead_host::mirror::Mirrors;
use playhead::Step;
use replay_data::{Hour, rows::TableId};
#[cfg(test)]
use replay_data::scope::CaptureMode;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc::Sender,
    },
};
#[cfg(test)]
use std::{sync::{Mutex, mpsc}, time::Duration};

pub struct Reads {
    tables: Vec<TableId>,
    extra: Option<(TableId, usize)>,
    composes: Arc<AtomicU32>,
}

impl Reads {
    pub fn new(tables: &[TableId]) -> (Box<dyn Layer>, Arc<AtomicU32>) {
        Self::with_column(tables, None)
    }

    pub fn with_column(tables: &[TableId], extra: Option<(TableId, usize)>) -> (Box<dyn Layer>, Arc<AtomicU32>) {
        let composes = Arc::new(AtomicU32::new(0));
        (Box::new(Self { tables: tables.to_vec(), extra, composes: Arc::clone(&composes) }), composes)
    }
}

impl Layer for Reads {
    fn name(&self) -> &'static str {
        "reads"
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        for &table in &self.tables {
            inputs.table(table, true)?;
            inputs.index(table, 0)?;
        }
        if let Some((table, column)) = self.extra {
            inputs.index(table, column)?;
        }
        Ok(())
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        self.composes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}

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

struct Probe {
    dropped: Option<Sender<Option<String>>>,
}

impl Layer for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(TableId::Countries, true)?;
        inputs.index(TableId::Countries, 0)
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        Ok(())
    }

    fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}

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

impl Drop for Probe {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped {
            let _ = dropped.send(std::thread::current().name().map(str::to_owned));
        }
    }
}

fn both(first: &mut MapSide, second: &mut MapSide, hour: i32, seen: &mut Vec<String>) {
    first.catch_up(hour).unwrap();
    note("first", first, hour, seen);
    for _ in 0..50_000_000 {
        second.frame(hour).unwrap();
        note("second", second, hour, seen);
        if second.applied() == Some(hour) && !second.composing() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("the second map did not reach hour {hour} ({seen:?})");
}

fn note(name: &str, side: &MapSide, hour: i32, seen: &mut Vec<String>) {
    let frame = side.last_frame();
    if frame.exact || frame.resets > 0 {
        seen.push(format!("{name} at {hour}: exact {}, resets {}, applied {:?}, newest {:?}", frame.exact, frame.resets, frame.applied, side.entry().playhead().newest()));
    }
}

fn play(entry: &Entry, first: &mut MapSide, second: Option<&mut MapSide>, seen: &mut Vec<String>, done: impl FnMut(&MapSide, Option<&MapSide>) -> bool, each: impl FnMut()) {
    play_to(entry, first, second, seen, Hour::MAX, done, each);
}

fn play_to(entry: &Entry, first: &mut MapSide, mut second: Option<&mut MapSide>, seen: &mut Vec<String>, cap: Hour, mut done: impl FnMut(&MapSide, Option<&MapSide>) -> bool, mut each: impl FnMut()) {
    for _ in 0..50_000_000u64 {
        let composing = first.composing() || second.as_deref().is_some_and(MapSide::composing);
        let head = match first.applied() {
            None => 0,
            Some(hour) if composing => hour,
            Some(hour) => entry.playhead().newest().unwrap_or(hour).min(hour + 1),
        }
        .min(cap);
        let stepped = first.frame(head).unwrap().steps > 0;
        note("first", first, head, seen);
        if let Some(second) = second.as_deref_mut() {
            second.frame(head).unwrap();
            note("second", second, head, seen);
        }
        each();
        if done(first, second.as_deref()) {
            return;
        }
        if !stepped {
            std::thread::yield_now();
        }
    }
    let (report, _, counters) = entry.playhead().held();
    panic!("the maps did not get there with the head at most {cap}: first at {:?}, second at {:?}, newest {:?}, preparing {}, {counters:?}; {seen:?}", first.applied(), second.as_deref().map(MapSide::applied), entry.playhead().newest(), report.preparing);
}

fn arrived(opening: &Opening) -> std::io::Result<MapSide> {
    for _ in 0..50_000_000u64 {
        if let Some(made) = opening.poll() {
            return made;
        }
        std::thread::yield_now();
    }
    panic!("the opening delivered nothing");
}

#[test]
fn maps_of_one_recording_share_its_playhead_each_with_its_own_reader_and_one_closing_resets_no_other() {
    let entry = open(record("shared maps", 16), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    assert!(Arc::ptr_eq(&entry, &open(record("shared maps", 16), hourly_steps()).unwrap()), "a second map of the same timeline gets the same playhead");
    let (layer, first_composes) = Reads::new(&[TableId::Countries]);
    let mut first = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    assert!(!entry.anchored());
    first.anchor(1, false, 0.0, true);
    assert!(entry.anchored(), "a map joining now takes this clock instead of seeking");
    let mut seen = Vec::new();
    for hour in 1..4 {
        first.catch_up(hour).unwrap();
        note("first", &first, hour, &mut seen);
    }
    assert_eq!(first_composes.load(Ordering::Relaxed), 1, "{seen:?}");
    seen.clear();
    let (layer, second_composes) = Reads::new(&[TableId::Countries, TableId::CountryLinks]);
    let mut second = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    for hour in 4..14 {
        both(&mut first, &mut second, hour, &mut seen);
        for _ in 0..2_000_000 {
            if second.draws("reads") {
                break;
            }
            std::thread::yield_now();
            second.frame(hour).unwrap();
            note("second", &second, hour, &mut seen);
        }
    }
    let (report, _, counters) = entry.playhead().held();
    assert_eq!(second_composes.load(Ordering::Relaxed), 1, "the second map composes once, when its tables join (joins {}, join holds {}, preparing {}, second at {:?} holding {:?}; {seen:?})", counters.joins, counters.join_holds, report.preparing, second.applied(), second.held().mirrors);
    assert_eq!(first_composes.load(Ordering::Relaxed), 1, "the first map is neither sought nor reset by the second opening (playhead {counters:?}; {seen:?})");
    assert_eq!(first.applied(), second.applied(), "both follow the one clock");
    drop(second);
    for hour in 14..16 {
        first.catch_up(hour).unwrap();
        note("first", &first, hour, &mut seen);
    }
    assert_eq!(first_composes.load(Ordering::Relaxed), 1, "nor by it closing ({seen:?})");
    drop(first);
    assert_eq!(Arc::strong_count(&entry), 1, "closed, the maps hold nothing of the playhead");
}

#[test]
fn a_map_opened_off_the_frame_thread_arrives_while_another_plays_and_resets_nothing() {
    let records = 24;
    let entry = open(record("opening", records), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let (layer, first_composes) = Reads::new(&[TableId::Countries]);
    let mut first = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    entry.playhead().play(1.0);
    entry.playhead().seek(0);
    let mut seen = Vec::new();
    play(&entry, &mut first, None, &mut seen, |first, _| !first.composing() && first.applied() >= Some(2), || {});
    let (gate, wait) = mpsc::channel::<()>();
    let (layer, second_composes) = Reads::new(&[TableId::Countries, TableId::CountryLinks]);
    let (made_on, joining) = (Arc::new(Mutex::new(None)), Arc::clone(&entry));
    let named = Arc::clone(&made_on);
    let opening = Opening::spawn(move || {
        wait.recv().unwrap();
        *named.lock().unwrap() = std::thread::current().name().map(str::to_owned);
        MapSide::new(joining, vec![layer], View::default())
    })
    .unwrap();
    let held_from = first.applied().unwrap();
    play(&entry, &mut first, None, &mut seen, |first, _| !first.composing() && first.applied() >= Some(held_from + 4), || {
        assert!(opening.poll().is_none(), "the reader is still being made while the first map plays");
    });
    gate.send(()).unwrap();
    let mut second = arrived(&opening).expect("the reader is made");
    assert_eq!(made_on.lock().unwrap().as_deref(), Some(OPENING), "made off the caller's thread");
    let declared = first.applied().unwrap();
    let by = declared + hourly_steps().ring as Hour + 1;
    let last = by + 2;
    assert!(last < records as Hour, "the recording holds the steps the bound allows");
    play_to(&entry, &mut first, Some(&mut second), &mut seen, by, |first, second| {
        let second = second.expect("open");
        second.draws("reads") && !second.composing() && second.applied() == first.applied()
    }, || {});
    play_to(&entry, &mut first, Some(&mut second), &mut seen, last, |first, second| {
        let second = second.expect("open");
        !first.composing() && !second.composing() && first.applied() == Some(last) && second.applied() == first.applied()
    }, || {});
    assert_eq!(second_composes.load(Ordering::Relaxed), 1, "it composes once, when its tables join ({seen:?})");
    assert_eq!(first_composes.load(Ordering::Relaxed), 1, "the first map is never reset by it ({seen:?})");
    assert_eq!(first.applied(), second.applied(), "both follow the one clock");
    drop(second);
    let held = Arc::strong_count(&entry);
    let (gate, wait) = mpsc::channel::<()>();
    let (dropped, dropped_on) = mpsc::channel();
    let joining = Arc::clone(&entry);
    let abandoned = Opening::spawn(move || {
        wait.recv().unwrap();
        MapSide::new(joining, vec![Box::new(Probe { dropped: Some(dropped) })], View::default())
    })
    .unwrap();
    drop(abandoned);
    gate.send(()).unwrap();
    let thread = dropped_on.recv_timeout(Duration::from_secs(60)).expect("the reader no longer wanted is dropped");
    assert_eq!(thread.as_deref(), Some(OPENING), "a reader no longer wanted is dropped by its opening's thread");
    assert_eq!(Arc::strong_count(&entry), held, "only the first map holds the playhead");
    play(&entry, &mut first, None, &mut seen, |first, _| !first.composing() && first.applied() >= Some(last + 1), || {});
    assert_eq!(first_composes.load(Ordering::Relaxed), 1, "nor by the reader let go ({seen:?})");
}

