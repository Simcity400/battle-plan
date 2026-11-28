use crate::test_support::host_recording::{KINDS, Recording, Sum, busy, quick, record, record_with, sum_of, table, units};
use super::{MapSide, mirror::Mirror, open};
use crate::playhead_host::units::Units;
use crate::layers::{Carried, Settling, Dirty, Inputs, Layer, Num, View};
use crate::playhead_host::mirror::{Cell as Read, Mirrors};
use playhead::{
    Piece, Settings, Step, Timeline,
    changes::Changes,
    lane::Lane,
    pages::Pages,
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::{Store, TableChanges, TableState, Value},
    unit::{BLOCK, Settings as Writing},
};
use replay_data::rows::TableId;
use std::{
    io,
    sync::{
        Arc, Mutex, mpsc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

fn eventful(at: u32, table: usize, store: &mut Store) {
    let status = if (300..310).contains(&at) {
        Status::NotRecorded
    } else if (400..410).contains(&at) {
        Status::Unavailable { reason: at }
    } else {
        Status::Complete
    };
    store.status(table, status, true, Some(at as i32), true).unwrap();
    if status != Status::Complete {
        return;
    }
    let mut changes: TableChanges<'_> = store.table(table).unwrap();
    if at == 0 {
        for slot in 0..12 {
            changes.insert_at(slot, 1000 + u128::from(slot)).unwrap();
            changes.write(slot, 0, Cell::I32(slot as i32)).unwrap();
            changes.write(slot, 1, Cell::OptionalI64(Some(i64::from(slot) - 5))).unwrap();
            changes.write(slot, 2, Cell::U32List(&[slot, slot + 1])).unwrap();
            changes.write(slot, 3, Cell::Bytes(0)).unwrap();
        }
        return;
    }
    let slot = at % 12;
    if at % 37 == 0 && slot == (at / 37) % 12 {
        return;
    }
    changes.write(slot, 0, Cell::I32((at as i32) * if slot % 2 == 0 { 1 } else { -3 })).unwrap();
    match at % 5 {
        0 => changes.write(slot, 1, Cell::OptionalI64(None)).unwrap(),
        1 => changes.put_value(slot, 1, Value::Unrecorded).unwrap(),
        2 => changes.write(slot, 2, Cell::U32List(&[at, at * 2, at * 3])).unwrap(),
        3 => changes.write(slot, 3, Cell::Bytes(at)).unwrap(),
        _ => changes.write(slot, 1, Cell::OptionalI64(Some(-i64::from(at)))).unwrap(),
    }
    if at % 37 == 0 {
        let gone = (at / 37) % 12;
        changes.delete(gone).unwrap();
        changes.insert_at(gone, 5000 + u128::from(at)).unwrap();
        changes.write(gone, 0, Cell::I32(-(at as i32))).unwrap();
        changes.put_value(gone, 2, Value::Unrecorded).unwrap();
    }
    if at % 53 == 0 {
        let extra = 12 + (at / 53) % 4;
        if changes.slot(9000 + u128::from(extra)).is_some() {
            changes.delete(extra).unwrap();
        } else {
            changes.insert_at(extra, 9000 + u128::from(extra)).unwrap();
            changes.write(extra, 0, Cell::I32(extra as i32)).unwrap();
        }
    }
}

fn differ(mirror: &Mirror, cells: &TableState, columns: &[usize]) -> Option<String> {
    let slots = (cells.slots() as usize).max(mirror.slots());
    for slot in 0..slots as u32 {
        let live = cells.live().get(slot);
        if mirror.live(slot) != live {
            return Some(format!("slot {slot}: live {} against {live}", mirror.live(slot)));
        }
        if !live {
            continue;
        }
        if mirror.key(slot) != cells.key(slot) {
            return Some(format!("slot {slot}: key {:?} against {:?}", mirror.key(slot), cells.key(slot)));
        }
        if mirror.slot(cells.key(slot).unwrap()) != Some(slot) {
            return Some(format!("slot {slot}: its key does not find it"));
        }
        for &column in columns {
            let held = mirror.owned(column, slot);
            let wanted = match cells.value(column, slot) {
                Value::Unrecorded => Read::Unrecorded,
                Value::Missing => Read::Missing,
                Value::Number(bits) => Read::Value(bits.to_le_bytes()[..width(KINDS[column])].to_vec()),
                Value::Text(id) => Read::Value(u64::from(id).to_le_bytes()[..4].to_vec()),
                Value::List(bytes) => Read::Value(bytes.to_vec()),
            };
            if held != wanted {
                return Some(format!("slot {slot} column {column}: {held:?} against {wanted:?}"));
            }
            if let Value::Text(id) = cells.value(column, slot)
                && mirror.string(id) != Some(format!("text {id}").as_bytes())
            {
                return Some(format!("slot {slot}: string {id} is {:?}", mirror.string(id)));
            }
        }
    }
    None
}

fn width(kind: Kind) -> usize {
    match kind {
        Kind::I32 => 4,
        _ => 8,
    }
}

#[test]
fn a_mirror_from_a_reset_equals_the_lane_and_stays_equal_over_steps() {
    let (recording, _) = record(1, 2 * BLOCK + 100, eventful);
    let pages: Arc<dyn Pages> = recording;
    let all = [0, 1, 2, 3];
    for (from, columns) in [(0, &all[..]), (BLOCK + 17, &all[..]), (40, &[0, 3][..])] {
        let mut lane = Lane::new(Arc::clone(&pages), 0, 0, columns, true).unwrap();
        lane.seek(from as i32).unwrap();
        let mut mirror = Mirror::rebuild(&lane, columns);
        assert_eq!(differ(&mirror, lane.cells(), columns), None, "rebuilt at hour {from}");
        let mut changes = Changes::default();
        let (mut to, mut steps, mut not_recorded, mut unavailable) = (from as i32 + 7, 0, 0, 0);
        while to < (2 * BLOCK + 100) as i32 {
            lane.fetch_through(to).unwrap();
            let head = lane.step(to, &mut changes).unwrap();
            mirror.apply(&changes);
            assert_eq!(differ(&mirror, lane.cells(), columns), None, "after the step to hour {head}");
            assert_eq!(mirror.bytes(), mirror.bytes_walked(), "the bytes kept as the cells are written, after the step to hour {head}");
            assert_eq!(mirror.head(), Some(head));
            assert_eq!(mirror.status(), Some(lane.status().0));
            if mirror.status() == Some(Status::NotRecorded) {
                not_recorded += 1;
                assert!(mirror.not_recorded().is_some_and(|why| why.contains("not recorded")));
                assert!(!mirror.complete());
            }
            if let Some(Status::Unavailable { reason }) = mirror.status() {
                unavailable += 1;
                let text = format!("text {reason}");
                assert_eq!(mirror.reason(), Some(text.as_str()), "the reason's text, not its id");
                assert!(mirror.not_recorded().is_some_and(|why| why.ends_with(&format!("is unavailable: {text}"))));
            }
            if head == to {
                to += 7;
            }
            steps += 1;
        }
        assert!(steps > 30);
        assert!(not_recorded > 0 || from > 310, "the stretch not recorded reached the mirror as such");
        assert!(unavailable > 0 || from > 410, "the stretch unavailable reached the mirror with its reason");
    }
}

fn read_whole(recording: Arc<Recording>) -> super::units::Read {
    let kinds = Arc::new(recording.kinds.clone());
    let pages: Arc<dyn Pages> = recording;
    super::units::Read { pages, kinds, branches: Arc::from(Vec::new()) }
}

#[test]
fn callers_asking_for_one_recordings_units_at_once_share_one_build() {
    use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc};
    let (recording, _) = record(1, BLOCK, busy);
    let builds = Arc::new(AtomicUsize::new(0));
    let (started, building) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let first = {
        let (recording, builds) = (Arc::clone(&recording), Arc::clone(&builds));
        std::thread::spawn(move || {
            super::units::shared("one build", || {
                builds.fetch_add(1, Ordering::SeqCst);
                started.send(()).unwrap();
                released.recv().unwrap();
                Ok(read_whole(recording))
            })
            .unwrap()
        })
    };
    building.recv().unwrap();
    let second = {
        let (recording, builds) = (Arc::clone(&recording), Arc::clone(&builds));
        std::thread::spawn(move || {
            super::units::shared("one build", || {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(read_whole(recording))
            })
            .unwrap()
        })
    };
    release.send(()).unwrap();
    let (first, second) = (first.join().unwrap(), second.join().unwrap());
    assert_eq!(builds.load(Ordering::SeqCst), 1, "the map and follow opening one recording build its units once");
    assert!(Arc::ptr_eq(&first.pages, &second.pages), "and read the same pages");
}

fn frames_until(side: &mut MapSide, done: impl FnMut(&MapSide, &super::Frame) -> bool) {
    crate::test_support::recorded::frames_until(side, done);
}

fn step_frame(side: &mut MapSide, head: i32) -> super::Frame {
    let frame = side.frame_synced(head).unwrap();
    assert!(frame.steps > 0, "no frame consumed steps through hour {head}: applied {:?}", side.applied());
    frame
}

fn sum_layer(side: &MapSide, at: usize) -> &Sum {
    side.parts().unwrap().1[at].any().downcast_ref::<Sum>().unwrap()
}

#[test]
fn a_reset_swaps_mirrors_and_composed_arrays_together_and_applies_the_steps_taken_meanwhile() {
    let (recording, states) = record(1, BLOCK + 200, busy);
    let entry = open(units(recording, "reset"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let (release, gate) = mpsc::channel();
    let mut layer = Sum::new(&composes);
    layer.gate = Some(Arc::new(Mutex::new(gate)));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    frames_until(&mut side, |_, frame| {
        if frame.resets > 0 {
            assert_eq!(frame.applied, Some(100), "the frame that takes the reset composes the hour sought, the steps taken with it left for after the swap");
        }
        frame.resets > 0
    });
    assert!(side.composing() && side.parts().is_none(), "the mirrors and layers are on the worker");
    let deferred = side.held().pending;
    entry.playhead().wait_newest(100 + 3 * 4).unwrap();
    let head = entry.playhead().newest().unwrap();
    let frame = side.frame(head).unwrap();
    assert!(frame.composing && frame.steps == 0, "nothing is applied while composing");
    assert_eq!(side.held().pending, deferred, "steps built meanwhile stay in the ring");
    release.send(()).unwrap();
    let mut swapped = false;
    frames_until(&mut side, |side, frame| {
        if frame.swapped.is_some() {
            assert_eq!((frame.steps, side.applied()), (0, Some(100)), "the swap frame shows the picture composed for the hour sought alone");
        }
        swapped |= frame.swapped.is_some();
        swapped && frame.steps > 0 && !side.composing() && side.held().pending == 0
    });
    let (mirrors, _) = side.parts().unwrap();
    let mirror = mirrors.get(table(0)).unwrap();
    let head = mirror.head().unwrap();
    assert!(head > 100 + 3 * 4 - 1, "the steps taken meanwhile are applied: head {head}");
    let column = Num::of(table(0), 0);
    assert_eq!(differ(mirror, &states[0][head as usize], &[0]), None);
    side.settle_all().unwrap();
    assert_eq!(sum_layer(&side, 0).total(), sum_of(side.parts().unwrap().0.get(table(0)).unwrap(), column));
    assert_eq!(*composes.lock().unwrap(), 1);
}

#[test]
fn changed_inputs_redeclare_and_reset_only_the_tables_whose_columns_changed() {
    let (recording, _) = record(2, BLOCK, busy);
    let entry = open(units(recording, "redeclare"), quick()).unwrap();
    let (first, second) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
    let mut other = Sum::new(&second);
    other.second = true;
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&first)), Box::new(other)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    assert_eq!((*first.lock().unwrap(), *second.lock().unwrap()), (1, 1));
    let kept = std::ptr::from_ref::<Mirror>(side.parts().unwrap().0.get(table(0)).unwrap());
    side.set_view(View { camera_height: 2.0, ..View::default() }).unwrap();
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let mirrors = side.parts().unwrap().0;
    assert!(mirrors.get(table(1)).unwrap().holds(1), "the table with a new column is held");
    assert!(std::ptr::eq(mirrors.get(table(0)).unwrap(), kept), "the other table's mirror is kept");
    assert_eq!((*first.lock().unwrap(), *second.lock().unwrap()), (1, 2), "only the layer reading the reset table composed");
}

#[test]
fn a_column_the_schema_lacks_refuses_the_layer_by_name() {
    let kinds = replay_recording::schema::schema().unwrap();
    let mut inputs = Inputs::new(&kinds, "political");
    let missing = inputs.number::<u32>(TableId::Provinces, "no_such_field").unwrap_err().to_string();
    assert!(missing.contains("political") && missing.contains("Provinces.no_such_field"), "{missing}");
    let mistyped = inputs.number::<i64>(TableId::Provinces, "owner").unwrap_err().to_string();
    assert!(mistyped.contains("Provinces.owner is a U32 column"), "{mistyped}");
    assert!(inputs.number::<u32>(TableId::Provinces, "owner").is_ok());
}

#[test]
fn a_disconnected_compose_worker_returns_its_layers_and_mirrors() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "closed-compose-worker"), quick()).unwrap();
    let count = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(entry, vec![Box::new(Sum::new(&count))], View::default()).unwrap();
    let (jobs, receive) = mpsc::channel();
    drop(receive);
    side.worker.jobs = jobs;
    assert_eq!(side.send_away(vec![true]).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    let (_, layers) = side.parts().expect("mirrors returned after the failed handoff");
    assert_eq!(layers.len(), 1);
    assert!(!side.composing());
}

#[test]
fn a_hook_in_the_slot_runs_on_every_step_until_removed() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(Arc::clone(&recording), "hook"), quick()).unwrap();
    assert!(Arc::ptr_eq(&open(units(recording, "hook"), quick()).unwrap(), &entry), "a second caller gets the same playhead");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let held = Arc::clone(&seen);
    let registry = Arc::downgrade(&entry.hooks);
    let id = entry.hooks().add(Box::new(move |step, lanes| {
        let unlocked = registry.upgrade().unwrap().held.try_lock().is_ok();
        held.lock().unwrap().push((step.last(), lanes.get(0).and_then(|lane| lane.head()), unlocked));
    }));
    let composes = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |_, frame| frame.applied.is_some_and(|hour| hour >= 40));
    let steps = seen.lock().unwrap().clone();
    assert!(steps.len() >= 5);
    assert!(steps.iter().all(|&(last, lane, unlocked)| lane == Some(last) && unlocked), "the hook sees the lanes at the step's end: {steps:?}");
    entry.hooks().remove(id);
    let count = seen.lock().unwrap().len();
    frames_until(&mut side, |_, frame| frame.applied.is_some_and(|hour| hour >= 80));
    assert!(seen.lock().unwrap().len() <= count + 4, "a removed hook runs no more once the ring's steps are built");
}

#[test]
fn a_renumber_at_a_packed_block_end_remaps_the_mirror() {
    let settings = Writing { restore_blocks: 1, whole_every: 1, pack_margin: 8, ..Writing::at(3) };
    let (recording, _) = record_with(settings, 1, 2 * BLOCK, |at, table, store| {
        store.status(table, Status::Complete, true, Some(at as i32), true).unwrap();
        let mut changes = store.table(table).unwrap();
        if at < 100 {
            changes.insert_at(at, u128::from(at) + 1000).unwrap();
            changes.write(at, 0, Cell::I32(at as i32)).unwrap();
            changes.write(at, 2, Cell::U32List(&[at, at + 1])).unwrap();
            changes.write(at, 3, Cell::Bytes(at)).unwrap();
        } else if at < 190 {
            changes.delete(at - 100).unwrap();
        } else if (BLOCK + 5..BLOCK + 50).contains(&at) {
            for slot in changes.held().rows().collect::<Vec<_>>() {
                changes.write(slot, 0, Cell::I32(-(at as i32))).unwrap();
            }
        }
    });
    assert!(recording.runs[0].held_pages().unwrap()[0].tables[0].packed, "the fixture packs its first block's end");
    let pages: Arc<dyn Pages> = recording;
    let columns = [0, 1, 2, 3];
    let mut lane = Lane::new(Arc::clone(&pages), 0, 0, &columns, true).unwrap();
    lane.seek(0).unwrap();
    let mut mirror = Mirror::rebuild(&lane, &columns);
    let mut changes = Changes::default();
    let (mut to, mut renumbered) = (99, 0);
    while to < 2 * BLOCK as i32 {
        lane.fetch_through(to).unwrap();
        let head = lane.step(to, &mut changes).unwrap();
        mirror.apply(&changes);
        assert_eq!(differ(&mirror, lane.cells(), &columns), None, "after the step to hour {head}");
        assert_eq!(mirror.bytes(), mirror.bytes_walked(), "the bytes kept as the cells are written, after the step to hour {head}");
        if changes.renumber().is_some() {
            renumbered += 1;
            assert_eq!(mirror.rows().collect::<Vec<_>>(), (0..10).collect::<Vec<u32>>());
            assert_eq!((0..10).map(|slot| mirror.key(slot).unwrap()).collect::<Vec<_>>(), (1090..1100).collect::<Vec<u128>>());
            assert_eq!(mirror.slot(1095), Some(5), "the key index follows the renumber");
        }
        if head == to {
            to += 100;
        }
    }
    assert_eq!(renumbered, 1);
}

#[test]
fn a_renumber_moves_only_the_rows_past_the_first_freed_slot() {
    let settings = Writing { restore_blocks: 1, whole_every: 1, pack_margin: 8, ..Writing::at(3) };
    let (recording, _) = record_with(settings, 1, 2 * BLOCK, |at, table, store| {
        store.status(table, Status::Complete, true, Some(at as i32), true).unwrap();
        let mut changes = store.table(table).unwrap();
        if at < 100 {
            changes.insert_at(at, u128::from(at) + 1000).unwrap();
            changes.write(at, 0, Cell::I32(at as i32)).unwrap();
            changes.write(at, 3, Cell::Bytes(at)).unwrap();
        } else if at < 190 {
            changes.delete(at - 95).unwrap();
        }
    });
    assert!(recording.runs[0].held_pages().unwrap()[0].tables[0].packed, "the fixture packs its first block's end");
    let pages: Arc<dyn Pages> = recording;
    let columns = [0, 1, 3];
    let mut lane = Lane::new(Arc::clone(&pages), 0, 0, &columns, true).unwrap();
    lane.seek(0).unwrap();
    let mut mirror = Mirror::rebuild(&lane, &columns);
    let mut changes = Changes::default();
    let (mut to, mut renumbered) = (99, 0);
    while to < 2 * BLOCK as i32 {
        lane.fetch_through(to).unwrap();
        let head = lane.step(to, &mut changes).unwrap();
        mirror.apply(&changes);
        assert_eq!(differ(&mirror, lane.cells(), &columns), None, "after the step to hour {head}");
        if changes.renumber().is_some() {
            renumbered += 1;
            assert_eq!(mirror.rows().collect::<Vec<_>>(), (0..10).collect::<Vec<u32>>());
            assert_eq!((0..10).map(|slot| mirror.key(slot).unwrap()).collect::<Vec<_>>(), (1000..1005).chain(1095..1100).collect::<Vec<u128>>());
            assert_eq!((mirror.slot(1003), mirror.slot(1097), mirror.slot(1050)), (Some(3), Some(7), None), "the key index follows the renumber");
            assert_eq!(mirror.moved(), 5, "the rows before the first freed slot stay where they are");
        }
        if head == to {
            to += 100;
        }
    }
    assert_eq!(renumbered, 1);
}

#[test]
fn a_mirror_of_an_earlier_format_reads_the_columns_added_since_not_recorded() {
    use playhead::pages::{Span, WorldPages};
    use replay_recording::columnar_frames::Recording;
    use replay_journal::{fixtures, synthetic::{Input, Sample}};
    use replay_data::rows::{DivisionRowAt141, compatibility::CAPTURE_ADDITIONS_FORMAT};
    let table = TableId::Divisions;
    let divisions = [DivisionRowAt141 { id: (3_u64 << 32) | 1, province: 11, ..Default::default() }, DivisionRowAt141 { id: (4_u64 << 32) | 1, province: 12, ..Default::default() }];
    let (bytes, _) = fixtures::written_at(recorder_derive::writers_within, CAPTURE_ADDITIONS_FORMAT - 1, b"format 141", &[(1, 0, 1)], &[table], |synthetic, _, ordinal| {
        let rows = &divisions[..if ordinal < 20 { 1 } else { 2 }];
        synthetic.table(table, Input::Snapshot { rows, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
    });
    let pages: Arc<dyn Pages> = Arc::new(WorldPages::over(Recording::open(bytes).unwrap(), Span::Whole).unwrap());
    let every: Vec<usize> = (0..table.field_count()).collect();
    let [province, theatre] = ["province", "theatre"].map(|name| every.iter().copied().find(|&column| table.field_name(column) == Some(name)).unwrap());
    let mut lane = Lane::new(Arc::clone(&pages), 0, table.index(), &every, true).unwrap();
    lane.seek(10).unwrap();
    let mut mirror = Mirror::rebuild(&lane, &every);
    assert_eq!((mirror.width(), mirror.holds(province), mirror.holds(theatre)), (table.field_count() - 1, true, false));
    let read = |mirror: &Mirror| {
        let mut rows: Vec<(u128, Read<u64>, Read<u64>)> = mirror.rows().map(|slot| (mirror.key(slot).unwrap(), mirror.cell(province, slot), mirror.cell(theatre, slot))).collect();
        rows.sort_unstable_by_key(|row| row.0);
        rows
    };
    let row = |division: &DivisionRowAt141| (u128::from(division.id), Read::Value(u64::from(division.province)), Read::Unrecorded);
    assert_eq!(read(&mirror), [row(&divisions[0])], "the reset");
    let mut changes = Changes::default();
    lane.fetch_through(i32::MAX).unwrap();
    for to in [19, 20, 40] {
        assert_eq!(lane.step(to, &mut changes).unwrap(), to);
        mirror.apply(&changes);
        assert_eq!(mirror.refused(), 0, "no change set names theatre");
    }
    assert_eq!(read(&mirror), [row(&divisions[0]), row(&divisions[1])], "after the steps");
}

#[test]
fn a_map_on_an_earlier_format_plays_with_changed_tables_withheld_and_newer_tables_not_recorded() {
    use super::{
        details,
        shared_maps_tests::Reads,
        views::{Need, Pace, Stepped},
    };
    use playhead::pages::{Span, WorldPages};
    use replay_data::{
        rows::{CountryRow, compatibility::CAPTURE_ADDITIONS_FORMAT, tables},
        scope::CaptureMode,
    };
    use replay_frame::frame::TableView;
    use replay_journal::{
        fixtures,
        synthetic::{Input, Sample},
    };
    use replay_recording::columnar_frames::{Frames, Recording};
    use viewer_foundation::tables::Seen;

    let countries = [CountryRow { id: 7, tag: b"TST".to_vec(), stability_raw: 5, ..Default::default() }];
    let (bytes, _) = fixtures::written_at(recorder_derive::writers_within, CAPTURE_ADDITIONS_FORMAT - 1, b"format 141 map", &[(1, 0, 1)], &[TableId::Countries], |synthetic, _, _| {
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
    });
    let recording = Recording::open(bytes).unwrap();
    let mut frames = Frames::new(&recording, &TableId::ALL).unwrap();
    frames.seek(0, 5).unwrap();
    let today = frames.frame(1).unwrap();
    drop(frames);
    let opened = Arc::new(WorldPages::over(recording, Span::Whole).unwrap());
    assert!(opened.kinds(TableId::VariantStats.index()).is_none(), "VariantStats is withheld on format 141");
    assert!(opened.table_entry(0, 0, TableId::VariantStats.index()).unwrap().first.is_some(), "format 141 wrote table 44 before its layout changed");
    let kinds = Arc::new(opened.schema().to_vec());
    let pages: Arc<dyn Pages> = opened;
    let timeline = Timeline::whole(&*pages).unwrap();
    let units = Units { name: "format 141 map".into(), pages, timeline, kinds };
    let entry = open(units, quick()).unwrap();
    entry.set_capture(CaptureMode::Full);

    let mut inputs = Inputs::new(entry.kinds(), "test");
    let refused = inputs.table(TableId::VariantStats, true).unwrap_err().to_string();
    assert!(refused.contains("test") && refused.contains("VariantStats"), "{refused}");

    let (layer, _) = Reads::new(&[TableId::Countries]);
    let mut side = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    side.set_branch(1);
    let reader = side.views().handle().reader_at(
        "older format",
        &[Need::every(TableId::Countries), Need::every(TableId::VariantStats), Need::every(TableId::ScriptVariableValues)],
        Pace::Step,
    );
    let mut stepped = Stepped::new(&entry, &[TableId::Countries, TableId::VariantStats, TableId::ScriptVariableValues]).unwrap();
    side.anchor(5, false, 0.0, true);
    entry.playhead().wait_declarations().unwrap();
    side.catch_up(5).unwrap();
    let declared = side.readers();
    assert_eq!(declared.iter().map(|(table, ..)| *table).collect::<Vec<_>>(), [TableId::Countries]);
    let (frame, failures) = reader.frame_with_failures().unwrap();
    assert_eq!(frame.hour, 5);
    let assert_frame = |frame: &replay_frame::frame::DecodedFrame| {
        let rows = frame.complete::<tables::Countries>().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 7);
        match &*frame.tables[TableId::VariantStats.index()] {
            TableView::Unavailable { reason } => {
                assert!(reason.contains("VariantStats") && reason.contains("144") && reason.contains("141"), "{reason}");
            }
            other => panic!("changed table must be unavailable with its format reason, got {other:?}"),
        }
        assert_eq!(&*frame.tables[TableId::ScriptVariableValues.index()], &TableView::NotRecorded { mode: CaptureMode::Full });
    };
    assert_frame(&frame);
    assert!(failures.is_empty(), "a withheld layout table carries no typed head failure: {failures:?}");

    let epoch = stepped.epoch();
    while stepped.take_through(5, epoch) != Some(5) {
        std::thread::yield_now();
    }
    assert_frame(&stepped.frame(1, 5).unwrap());
    assert_frame(&stepped.frame_past(1, 5).unwrap());

    let (entries, differing, first, absent) = details::differ(&side, &today);
    assert!(entries >= TableId::ALL.len() as u64 - absent.len() as u64);
    assert!(!absent.contains(&TableId::VariantStats));
    assert_eq!((differing, first), (0, None));

    match details::seen(&entry, TableId::VariantStats, 5, Duration::ZERO) {
        Seen::Unavailable(reason) => assert!(reason.contains("VariantStats") && reason.contains("144"), "{reason}"),
        other => panic!("expected withheld layout reason, got {other:?}"),
    }
    assert_eq!(details::seen(&entry, TableId::ScriptVariableValues, 5, Duration::ZERO), Seen::NotRecorded(Some(CaptureMode::Full)));
    let lines = details::of_map(&side);
    assert!(lines.iter().any(|line| line.starts_with("VariantStats: unavailable (") && line.contains("144")), "{lines:?}");
    assert!(lines.iter().any(|line| *line == format!("ScriptVariableValues: not recorded in a {} recording", CaptureMode::Full)));
}

#[test]
fn script_values_are_read_from_the_requested_hour_without_building_a_mirror() {
    use playhead::pages::{Span, WorldPages};
    use replay_data::{format::FormatVersion, rows::{CountryRow, ScriptVariableValueRow}};
    use replay_frame::frame::{KeyedNumber, SharedView, view_reads};
    use replay_journal::{fixtures, synthetic::{Input, Sample}};
    use replay_recording::columnar_frames::Recording;

    let values = TableId::ScriptVariableValues;
    let (bytes, _) = fixtures::written_at(recorder_derive::writers_within, FormatVersion::CURRENT.version(), b"script values", &[(1, 0, 3)], &[TableId::Countries, values], |synthetic, _, ordinal| {
        let countries = [CountryRow { id: 7, tag: b"TST".to_vec(), ..Default::default() }];
        let rows = [ScriptVariableValueRow { scope_kind: 2, scope_id: 7, var_id: 9, value_raw: -20 + i64::from(ordinal) }];
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        synthetic.table(values, Input::Snapshot { rows: &rows, partial: (ordinal == 2).then_some(1), read: true, sample: Sample::FromReadBit }).unwrap();
    });
    let opened = Arc::new(WorldPages::over(Recording::open(bytes).unwrap(), Span::Whole).unwrap());
    let kinds = Arc::new(opened.schema().to_vec());
    let pages: Arc<dyn Pages> = opened;
    let timeline = Timeline::whole(&*pages).unwrap();
    let hour = timeline.first();
    let entry = open(Units { name: "script values".into(), pages, timeline, kinds }, quick()).unwrap();
    let (layer, _) = super::shared_maps_tests::Reads::new(&[TableId::Countries]);
    let mut side = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    side.set_branch(1);
    let reader = side.views().reader("script values", &[super::views::Need::every(TableId::Countries)]);
    let key = (2u128 << 64) | (7u128 << 32) | 9;
    let read = |side: &mut MapSide, hour| {
        side.anchor(hour, false, 0.0, true);
        side.catch_up(hour).unwrap();
        assert!(side.parts().unwrap().0.get(values).is_none());
        let frame = reader.frame().unwrap();
        assert_eq!(frame.hour, hour);
        frame.keyed_i64(values, Some(key)).unwrap()
    };
    assert_eq!(read(&mut side, hour), KeyedNumber::Value(Some(-20)));
    assert_eq!(read(&mut side, hour + 1), KeyedNumber::Value(Some(-19)));
    assert!(matches!(read(&mut side, hour + 2), KeyedNumber::Unavailable(reason) if reason.contains("truncated") && reason.contains("ScriptVariableValues")));
    assert_eq!(read(&mut side, hour), KeyedNumber::Value(Some(-20)));
    let original = reader.frame().unwrap();
    let (_, reads) = view_reads::record(|| original.keyed_i64(values, Some(key)).unwrap());
    assert_eq!(reads.len(), 1);
    assert_eq!(read(&mut side, hour + 1), KeyedNumber::Value(Some(-19)));
    let next = reader.frame().unwrap();
    assert!(SharedView::ptr_eq(&original.tables[values.index()], &next.tables[values.index()]), "a keyed source is not a changed row view");
    assert!(!reads[0].is(&next.tables[values.index()]), "a lookup at a different hour invalidates the recorded dependency");

    let all = side.views().reader("explicit script values", &[super::views::Need::every(values)]);
    side.catch_up(hour + 1).unwrap();
    let frame = all.frame().expect("the explicit reader receives the written table");
    let rows = frame.complete::<replay_data::rows::tables::ScriptVariableValues>().unwrap();
    assert_eq!(rows[0].value_raw, -19);
    assert!(side.parts().unwrap().0.get(values).is_none());
}

#[test]
fn variant_views_follow_the_recording_without_a_full_map_mirror() {
    use playhead::pages::{Span, WorldPages};
    use replay_data::{format::FormatVersion, rows::{CountryRow, VariantDefinitionRow, tables}};
    use replay_frame::frame::{SharedView, TableView};
    use replay_journal::{fixtures, synthetic::{Input, Sample}};
    use replay_recording::columnar_frames::Recording;

    let variants = TableId::VariantDefinitions;
    let (bytes, _) = fixtures::written_at(recorder_derive::writers_within, FormatVersion::CURRENT.version(), b"variant views", &[(1, 0, 1)], &[TableId::Countries, variants], |synthetic, _, ordinal| {
        let countries = [CountryRow { id: 7, tag: b"TST".to_vec(), ..Default::default() }];
        let token = if ordinal < 2 { 9 } else { 10 };
        let name = if ordinal == 0 { b"before".to_vec() } else { b"after".to_vec() };
        let rows = [
            VariantDefinitionRow { id: 41, equipment_token: 8, creator_link: 7, ..Default::default() },
            VariantDefinitionRow { id: 42, equipment_token: token, creator_link: 7, explicit_entity: name, ..Default::default() },
        ];
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        let input = match ordinal {
            2 => Input::Snapshot { rows: &rows[1..], partial: None, read: true, sample: Sample::FromReadBit },
            4 => Input::Snapshot { rows: &rows[1..], partial: Some(1), read: true, sample: Sample::FromReadBit },
            5 => Input::Snapshot { rows: &rows[..1], partial: Some(1), read: true, sample: Sample::FromReadBit },
            6 => Input::Unavailable { reason: "after", read: false, sample: Sample::FromReadBit },
            _ => Input::Snapshot { rows: &rows, partial: None, read: true, sample: Sample::FromReadBit },
        };
        synthetic.table(variants, input).unwrap();
    });
    let opened = Arc::new(WorldPages::over(Recording::open(bytes).unwrap(), Span::Whole).unwrap());
    let kinds = Arc::new(opened.schema().to_vec());
    let pages: Arc<dyn Pages> = opened;
    let timeline = Timeline::whole(&*pages).unwrap();
    let hour = timeline.first();
    let entry = open(Units { name: "variant views".into(), pages, timeline, kinds }, quick()).unwrap();
    let (layer, _) = super::shared_maps_tests::Reads::with_column(&[TableId::Countries, variants], Some((variants, 1)));
    let mut side = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    side.set_branch(1);
    side.views().use_columns(variants);
    let reader = side.views().handle().telling("variant models", &[super::views::Need::every(variants)]);
    let mut held = None;
    for (offset, token) in [(0, 9), (0, 9), (1, 9), (2, 10)] {
        let at = hour + offset;
        side.anchor(at, false, 0.0, held.is_none());
        side.catch_up(at).unwrap();
        let (frame, changes, _) = reader.take_keyed().unwrap();
        assert_eq!(frame.hour, at);
        let rows = frame.complete::<tables::VariantDefinitions>().unwrap();
        let row = rows.iter().find(|row| row.id == 42).unwrap();
        assert_eq!(row.equipment_token, token);
        assert_eq!(&row.explicit_entity[..], if offset == 0 { &b"before"[..] } else { &b"after"[..] });
        assert_eq!(rows.iter().any(|row| row.id == 41), offset < 2);
        if offset == 1 {
            let entity = replay_data::rows::mask::<VariantDefinitionRow>(&["explicit_entity"]);
            let changes = changes.get(&variants).and_then(Option::as_ref).expect("a rename carries its changed field");
            assert_eq!(changes.get(&42), Some(&entity));
        } else if offset == 2 {
            let changes = changes.get(&variants).expect("changed variant rows carry their invalidation");
            assert!(changes.as_ref().is_none_or(|rows| rows.contains_key(&41) && rows.contains_key(&42)));
        }
        let mirror = side.parts().unwrap().0.get(variants).unwrap();
        let slot = mirror.slot(42).unwrap();
        assert_eq!(mirror.cell(0, slot).value(), Some(token as u64));
        assert_eq!(mirror.cell(1, slot).value(), Some(7));
        assert!(!(2..variants.field_count()).any(|column| mirror.holds(column)));
        assert_eq!(mirror.slot(41).is_some(), offset < 2);
        assert_eq!(mirror.refused(), 0, "intentionally omitted columns do not count as refused writes");
        let view = frame.tables[variants.index()].clone();
        if let Some(before) = held.replace(view.clone()) {
            assert_eq!(SharedView::ptr_eq(&before, &view), offset == 0);
        }
    }
    for (offset, expected) in [(3, None), (4, Some(42)), (5, Some(41)), (6, None)] {
        let at = hour + offset;
        side.anchor(at, false, 0.0, false);
        side.catch_up(at).unwrap();
        let frame = reader.frame().unwrap();
        assert_eq!(frame.hour, at);
        if let Some(key) = expected {
            let TableView::Truncated { rows, missing: 1 } = frame.tables[variants.index()].held() else { panic!("the hour should be truncated") };
            assert_eq!(rows.len(), 1);
            assert!(rows.contains_key(key));
        }
    }
    let frame = reader.frame().unwrap();
    assert!(matches!(frame.tables[variants.index()].held(), TableView::Unavailable { reason } if reason == "after"));
    assert_eq!(side.parts().unwrap().0.get(variants).unwrap().reason(), Some("after"));
}

#[test]
fn variant_views_keep_recorded_fields_before_design_was_appended() {
    use super::{script_values::ExactColumns, views::tests::{Put, STRINGS, record_with_kinds, write_rows}};
    use playhead::pages::TableWritten;
    use replay_data::rows::{compatibility::DESIGN_STATE_FORMAT, tables};

    let table = TableId::VariantDefinitions;
    let format = DESIGN_STATE_FORMAT - 1;
    let mut kinds = replay_recording::schema::schema_at(format).unwrap();
    kinds[table.index()].truncate(table.last_change().framed_fields(table, format));
    let units = record_with_kinds("variants before design", 1, Writing::at(3), kinds, |store, _| {
        let cells = [Cell::U32(9), Cell::U32(0), Cell::U32(0), Cell::U8(0), Cell::U8(0), Cell::U64(0), Cell::U32(0),
            Cell::Bytes(0), Cell::Bytes(0), Cell::Bytes(1), Cell::U32(0), Cell::U8(0), Cell::U32(0), Cell::Bytes(0), Cell::U8(0)];
        store.status(table.index(), Status::Complete, true, Some(0), true).unwrap();
        write_rows(store, table, &[(42, cells.into_iter().map(Put::Cell).collect())]);
    });
    let hour = units.timeline.first();
    let entry = open(units, quick()).unwrap();
    let columns = ExactColumns::new(&entry, table, TableWritten::Written);
    let (view, made) = columns.decoded(hour, table.full_mask(), None, &mut Vec::new());
    assert!(made.failure.is_none(), "{:?}", made.failure);
    let replay_frame::frame::TableView::Complete(rows) = view else { panic!("the recorded variant must be available") };
    let [row] = rows.rows::<tables::VariantDefinitions>().unwrap() else { panic!("expected one recorded variant") };
    assert_eq!(row.equipment_token, 9);
    assert_eq!(row.explicit_entity, STRINGS[1]);
    assert!(matches!(row.design, replay_data::rows::Recorded::NotRecorded));
}

#[test]
fn a_gap_step_shows_every_table_not_recorded_until_the_next_piece() {
    let (recording, _) = record(2, BLOCK, busy);
    let pages: Arc<dyn Pages> = Arc::clone(&recording) as Arc<dyn Pages>;
    let timeline = Timeline::new(&*pages, vec![Piece { run: 0, first: 0, last: 99 }, Piece { run: 0, first: 200, last: 399 }]).unwrap();
    let units = Units { name: "gap".into(), pages, timeline, kinds: Arc::new(recording.kinds.clone()) };
    let entry = open(units, quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(60);
    let (mut gap, mut after) = (0, 0);
    frames_until(&mut side, |side, frame| {
        if let (Some(hour), Some((mirrors, _))) = (frame.applied, side.parts()) {
            let mirror = mirrors.get(table(0)).unwrap();
            if (100..200).contains(&hour) {
                gap += 1;
                assert_eq!(mirror.status(), None, "hour {hour} is in the gap");
                assert!(!mirror.complete() && mirror.not_recorded().is_some_and(|why| why.contains("gap")));
            } else if hour >= 200 {
                after += 1;
                assert_eq!(mirror.status(), Some(Status::Complete), "hour {hour} is in the second piece");
            } else {
                assert!(mirror.complete());
            }
        }
        after >= 3
    });
    assert!(gap > 0, "a gap step was taken");
}

#[test]
fn a_dependent_layer_follows_its_upstream_and_equals_composed() {
    struct Twice {
        column: Option<Num<i32>>,
        values: Vec<Option<i64>>,
        dirty: Dirty,
        changed: crate::layers::Changed,
        frame: u64,
    }
    struct Plus {
        values: Vec<Option<i64>>,
        dirty: Dirty,
        settled_from_half: u64,
        frame: u64,
    }
    impl Layer for Twice {
        fn name(&self) -> &'static str {
            "twice"
        }
        fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
            inputs.table(table(0), false)?;
            inputs.index(table(0), 0)?;
            self.column = Some(Num::of(table(0), 0));
            Ok(())
        }
        fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
            let mirror = mirrors.get(table(0)).unwrap();
            let column = self.column.unwrap();
            self.values = (0..mirror.slots() as u32).map(|slot| mirror.live(slot).then(|| mirror.number(column, slot).value().map(|value| 2 * i64::from(value))).flatten()).collect();
            self.dirty.clear();
            Ok(())
        }
        fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
            if let Some(changes) = step.table(0) {
                for written in changes.columns().iter().filter(|written| written.column() == 0) {
                    for &slot in written.slots() {
                        self.dirty.mark(slot, self.frame);
                    }
                }
            }
        }
        fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
            self.frame += 1;
            let mirror = mirrors.get(table(0)).unwrap();
            let column = self.column.unwrap();
            let (values, changed) = (&mut self.values, &mut self.changed);
            self.dirty.settle(|slot| {
                let value = mirror.live(slot).then(|| mirror.number(column, slot).value().map(|value| 2 * i64::from(value))).flatten();
                if values.len() <= slot as usize {
                    values.resize(slot as usize + 1, None);
                }
                if values[slot as usize] != value {
                    values[slot as usize] = value;
                    changed.mark("row", u64::from(slot));
                }
                Ok(())
            })
        }
        fn carried(&self) -> Carried {
            Carried { entities: self.dirty.len() }
        }
        fn held(&self) -> usize {
            0
        }
        fn any(&self) -> &dyn std::any::Any {
            self
        }
        fn changed(&self) -> Option<&crate::layers::Changed> {
            Some(&self.changed)
        }
        fn clear_changed(&mut self) {
            self.changed.clear();
        }
    }
    impl Plus {
        fn from(upstream: &Twice) -> Vec<Option<i64>> {
            upstream.values.iter().map(|value| value.map(|value| value + 1)).collect()
        }
    }
    impl Layer for Plus {
        fn name(&self) -> &'static str {
            "plus"
        }
        fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
            inputs.after("twice");
            Ok(())
        }
        fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
            unreachable!("a dependent composes with its upstream")
        }
        fn compose_with(&mut self, _mirrors: &Mirrors, upstream: &super::Upstream<'_>) -> io::Result<()> {
            self.values = Self::from(upstream.get::<Twice>().expect("registered before"));
            self.dirty.clear();
            Ok(())
        }
        fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}
        fn apply_with(&mut self, _step: &Step, _mirrors: &Mirrors, upstream: &super::Upstream<'_>) {
            if let crate::layers::Change::Marked(marked) = upstream.changed::<Twice>() {
                for slot in marked.of("row").iter().map(|&id| id as u32) {
                    self.dirty.mark(slot, self.frame);
                }
            }
        }
        fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
            unreachable!("a dependent settles with its upstream")
        }
        fn settle_with(&mut self, _mirrors: &Mirrors, upstream: &super::Upstream<'_>, _settling: Settling) -> io::Result<()> {
            self.frame += 1;
            let twice = upstream.get::<Twice>().unwrap();
            if twice.dirty.len() > 0 {
                self.settled_from_half += 1;
            }
            let values = &mut self.values;
            self.dirty.settle(|slot| {
                if values.len() <= slot as usize {
                    values.resize(slot as usize + 1, None);
                }
                values[slot as usize] = twice.values.get(slot as usize).copied().flatten().map(|value| value + 1);
                Ok(())
            })
        }
        fn carried(&self) -> Carried {
            Carried { entities: self.dirty.len() }
        }
        fn held(&self) -> usize {
            0
        }
        fn any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "dependent"), quick()).unwrap();
    let twice = Twice { column: None, values: Vec::new(), dirty: Dirty::default(), changed: crate::layers::Changed::default(), frame: 0 };
    let plus = Plus { values: Vec::new(), dirty: Dirty::default(), settled_from_half: 0, frame: 0 };
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(twice), Box::new(plus)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let check = |side: &MapSide| {
        let layers = side.parts().unwrap().1;
        let (twice, plus) = (layers[0].any().downcast_ref::<Twice>().unwrap(), layers[1].any().downcast_ref::<Plus>().unwrap());
        assert_eq!(plus.settled_from_half, 0, "the dependent never settles from a half-settled upstream");
        (twice.dirty.len() == 0 && plus.dirty.len() == 0).then(|| assert_eq!(plus.values, Plus::from(twice), "stepwise equals composed"))
    };
    for _ in 0..12 {
        frames_until(&mut side, |side, frame| frame.steps > 0 && !side.composing());
        assert!(check(&side).is_some(), "the frame that applied a step left both layers whole");
    }
}

#[test]
fn a_slower_page_anchor_publishes_fine_steps_from_the_current_head() {
    use dashboard_follow::registry::Anchor;
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "slow-page"), quick()).unwrap();
    let (tx, rx) = mpsc::channel();
    entry.hooks().add(Box::new(move |s, _| { let _ = tx.send((s.reset(), s.last())); }));
    let anchor = Anchor { hour: 0, position: 0., timestamps: None, playing: true, speed: 1440., frame_rate: 60., seek: 1, sought: true };
    entry.follow_anchor(anchor);
    while rx.recv().unwrap().1 < 24 {}
    let epoch = entry.playhead().epoch();
    entry.follow_anchor(Anchor { hour: 5, position: 5., speed: 60., sought: false, ..anchor });
    let slower = entry.playhead().epoch();
    assert!(slower > epoch, "a slower pace is a seek at the current head");
    entry.follow_anchor(Anchor { hour: 5, position: 5., speed: 60., sought: false, ..anchor });
    assert_eq!(entry.playhead().epoch(), slower, "the same pace again seeks nothing");
    while rx.recv().unwrap() != (true, 5) {}
    assert_eq!(rx.recv().unwrap(), (false, 6));
    entry.follow_anchor(Anchor { position: 7., hour: 7, sought: false, ..anchor });
    entry.follow_anchor(Anchor { position: 8., hour: 8, speed: 60., sought: false, ..anchor });
    assert!(entry.playhead().epoch() > slower);
    while rx.recv().unwrap() != (true, 8) {}
    assert_eq!(rx.recv().unwrap(), (false, 9));
}

#[test]
fn two_anchors_of_the_same_hour_make_one_seek() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "anchors"), quick()).unwrap();
    entry.anchor(100, true, 4.0, true);
    let first = entry.playhead().holding().expect("held at the seek");
    entry.anchor(100, true, 4.0, true);
    assert_eq!(entry.playhead().holding(), Some(first), "the second anchor of hour 100 is dropped");
    entry.anchor(200, true, 4.0, true);
    let second = entry.playhead().holding().expect("held at the new seek");
    assert_ne!(second, first, "another hour seeks");
    entry.anchor(200, false, 4.0, false);
    entry.anchor(200, false, 4.0, true);
    assert_eq!(entry.playhead().holding(), Some(second), "paused at the hour it is seeking to: dropped");
    assert!(entry.paused(), "the pause is the clock's the moment it is anchored, with no step taken");
    entry.anchor(200, true, 1440.0, false);
    assert!(!entry.paused(), "and so is playing on at the top speed");
}

#[test]
fn a_layer_refused_and_accepted_again_composes_instead_of_resuming() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "refused"), quick()).unwrap();
    let (kept, refusing) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
    let mut second = Sum::new(&refusing);
    second.refuse_high = true;
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&kept)), Box::new(second)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side.set_view(View { camera_height: 2.0, ..View::default() }).unwrap();
    assert_eq!(side.refusals().count(), 1);
    let mut stepped = 0;
    frames_until(&mut side, |_, frame| {
        stepped += frame.steps;
        stepped >= 5
    });
    side.set_view(View::default()).unwrap();
    frames_until(&mut side, |side, frame| frame.steps > 0 && !side.composing());
    side.settle_all().unwrap();
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
    assert_eq!(sum_layer(&side, 1).total(), sum_of(mirror, Num::of(table(0), 0)), "accepted again, its arrays equal composed");
    assert_eq!(*refusing.lock().unwrap(), 2, "it composed again when accepted");
}

#[test]
fn a_camera_move_reaches_the_layers_without_a_redeclare_or_a_compose() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "camera"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    for height in [3.0, 5.0] {
        side.set_view(View { camera_height: height, ..View::default() }).unwrap();
        frames_until(&mut side, |_, frame| frame.steps > 0);
    }
    assert_eq!(sum_layer(&side, 0).viewed, vec![3.0, 5.0], "each camera move reaches the layer once");
    assert_eq!(*composes.lock().unwrap(), 1, "no compose for a move that changes no input");
}

#[test]
fn two_anchors_of_one_hour_on_one_playhead_make_one_seek() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "anchored twice"), quick()).unwrap();
    assert!(!entry.drawn(), "no map draws yet");
    let composes = Arc::new(Mutex::new(0));
    let side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    assert!(entry.drawn(), "a map draws");
    side.anchor(300, true, 4.0, true);
    let epoch = entry.playhead().holding().expect("held at the seek");
    entry.anchor(300, true, 4.0, true);
    assert_eq!(entry.playhead().holding(), Some(epoch), "a second anchor of the same hour makes no second seek");
    drop(side);
    assert!(!entry.drawn(), "the map gone, none draws");
}

#[test]
fn a_layer_shown_again_never_reads_a_mirror_its_tables_left() {
    let (recording, _) = record(1, 4 * BLOCK, busy);
    let entry = open(units(recording, "hidden"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut layer = Sum::new(&composes);
    layer.hide_high = true;
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side.set_view(View { camera_height: 2.0, ..View::default() }).unwrap();
    let mut stepped = 0;
    frames_until(&mut side, |_, frame| {
        stepped += frame.steps;
        stepped >= 6
    });
    let shown = entry.playhead().newest().unwrap();
    let before = sum_layer(&side, 0).read.len();
    side.set_view(View::default()).unwrap();
    let last = 4 * BLOCK as i32 - 1;
    frames_until(&mut side, |side, frame| (frame.steps > 0 && !side.composing() && sum_layer(side, 0).read.len() > before + 2) || frame.applied == Some(last));
    let read = &sum_layer(&side, 0).read[before..];
    assert!(read.iter().all(|&head| head.is_some_and(|head| head >= shown)), "shown again, it read only mirrors at or past hour {shown}: {read:?}");
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap_or_else(|| panic!("shown again at hour {shown}, its table came back as a reset before hour {:?}", side.applied()));
    assert!(mirror.head().is_some_and(|head| head >= shown), "the table came back as a reset, not as the mirror it left");
    let back = read.iter().flatten().next().copied();
    assert!(back.is_some_and(|hour| hour < shown + 48), "it came back within the ring and the read-ahead of hour {shown}: first read at {back:?}");
    side.settle_all().unwrap();
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
    assert_eq!(sum_layer(&side, 0).total(), sum_of(mirror, Num::of(table(0), 0)), "its arrays equal composed");
}

#[test]
fn a_table_two_layers_share_stays_current_when_one_stops_declaring_it() {
    let (recording, states) = record(1, BLOCK, busy);
    let entry = open(units(recording, "shared"), quick()).unwrap();
    let (first, second) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
    let mut hiding = Sum::new(&second);
    hiding.hide_high = true;
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&first)), Box::new(hiding)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    side.set_view(View { camera_height: 2.0, ..View::default() }).unwrap();
    let mut stepped = 0;
    frames_until(&mut side, |_, frame| {
        stepped += frame.steps;
        stepped >= 4
    });
    let mirror = side.parts().unwrap().0.get(table(0)).expect("the other layer keeps the table");
    let head = mirror.head().unwrap();
    assert_eq!(differ(mirror, &states[0][head as usize], &[0]), None, "the shared table stays current");
}

#[test]
fn stepped_frames_reach_every_hour_across_a_block_end() {
    let (recording, states) = record(1, 2 * BLOCK, busy);
    let entry = open(units(recording, "stepped"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.anchor(0, false, 0.0, true);
    let hours = (0..4).chain((5..BLOCK + 40).step_by(7)).chain(BLOCK + 41..BLOCK + 44);
    for hour in hours {
        side.catch_up(hour as i32).unwrap_or_else(|error| panic!("hour {hour}: {error}"));
        let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
        assert_eq!(mirror.head(), Some(hour as i32));
        assert_eq!(differ(mirror, &states[0][hour as usize], &[0]), None, "at hour {hour}");
        assert_eq!(sum_layer(&side, 0).total(), sum_of(mirror, Num::of(table(0), 0)), "the layer at hour {hour}");
    }
}

#[test]
fn a_held_dependent_sees_every_change_published_while_it_was_held() {
    struct Publisher {
        frame: u64,
        changed: crate::layers::Changed,
    }
    struct Reader {
        seen: Vec<u64>,
    }
    impl Layer for Publisher {
        fn name(&self) -> &'static str {
            "publisher"
        }
        fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
            inputs.table(table(0), false)
        }
        fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
            Ok(())
        }
        fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}
        fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
            self.frame += 1;
            self.changed.mark("frame", self.frame);
            Ok(())
        }
        fn carried(&self) -> Carried {
            Carried { entities: usize::from(self.frame < 5) }
        }
        fn held(&self) -> usize {
            0
        }
        fn any(&self) -> &dyn std::any::Any {
            self
        }
        fn changed(&self) -> Option<&crate::layers::Changed> {
            Some(&self.changed)
        }
        fn clear_changed(&mut self) {
            self.changed.clear();
        }
    }
    impl Layer for Reader {
        fn name(&self) -> &'static str {
            "reader"
        }
        fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
            inputs.after("publisher");
            Ok(())
        }
        fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
            Ok(())
        }
        fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}
        fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
            Ok(())
        }
        fn settle_with(&mut self, _mirrors: &Mirrors, upstream: &super::Upstream<'_>, _settling: Settling) -> io::Result<()> {
            if let crate::layers::Change::Marked(marked) = upstream.changed::<Publisher>() {
                self.seen.extend_from_slice(marked.of("frame"));
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
    }
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "held"), quick()).unwrap();
    let publisher = Publisher { frame: 0, changed: crate::layers::Changed::default() };
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(publisher), Box::new(Reader { seen: Vec::new() })], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    frames_until(&mut side, |side, _| side.parts().is_some_and(|(_, layers)| layers[0].any().downcast_ref::<Publisher>().unwrap().frame >= 8));
    let layers = side.parts().unwrap().1;
    let published = layers[0].any().downcast_ref::<Publisher>().unwrap().frame;
    let seen = &layers[1].any().downcast_ref::<Reader>().unwrap().seen;
    assert_eq!(seen, &(1..=published).collect::<Vec<u64>>(), "every id published, the held frames' included");
}

fn frames_to_exact(side: &mut MapSide, head: i32, states: &[TableState]) -> usize {
    let before = side.parts().unwrap().0.get(table(0)).and_then(|mirror| mirror.head()).unwrap();
    let mut frames = 0;
    crate::test_support::recorded::frames_at(side, head, &format!("exact hour {head}"), |side, _| {
        frames += 1;
        if let Some((mirrors, _)) = side.parts() {
            let at = mirrors.get(table(0)).and_then(|mirror| mirror.head());
            assert!(at.is_some_and(|at| at <= head.max(before)), "frame {frames} at {head} showed hour {at:?}, after {before}");
            assert!(side.draws("sum"), "the old picture stays while the exact one is made");
            if at == Some(head) && !side.composing() {
                return true;
            }
        }
        false
    });
    side.settle_all().unwrap();
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
    assert_eq!(differ(mirror, &states[head as usize], &[0]), None, "the mirror at hour {head}");
    assert_eq!(sum_layer(side, 0).total(), sum_of(mirror, Num::of(table(0), 0)), "the layer's arrays at hour {head}");
    frames
}

#[test]
fn a_pause_inside_a_step_and_single_hours_while_paused_show_exactly_that_hour() {
    let (recording, states) = record(1, 2 * BLOCK, busy);
    let entry = open(units(recording, "paused"), Settings { speed: 1440.0, rate: 60.0, ..quick() }).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.anchor(0, true, 1440.0, true);
    frames_until(&mut side, |side, frame| frame.applied.is_some_and(|hour| hour >= 100) && !side.composing());
    let applied = side.parts().unwrap().0.get(table(0)).unwrap().head().unwrap();
    let paused = applied + 5;
    entry.playhead().wait_newest(paused + 1).unwrap();
    assert!(entry.playhead().newest().unwrap() > paused, "the playhead built past the paused hour");
    entry.anchor(paused, false, 1440.0, false);
    let _ = frames_to_exact(&mut side, paused, &states[0]);
    entry.anchor(paused + 1, false, 1440.0, true);
    frames_to_exact(&mut side, paused + 1, &states[0]);
    entry.anchor(paused, false, 1440.0, true);
    frames_to_exact(&mut side, paused, &states[0]);
}

#[test]
fn a_reopened_map_seeks_a_paused_hour_older_than_the_ring() {
    let (recording, _) = record(1, 49, busy);
    let entry = open(units(recording, "reopen paused"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let map = || MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    let mut first = map();
    entry.anchor(48, false, 4.0, true);
    crate::test_support::recorded::frames_at(&mut first, 48, "the first map's last hour", |_, frame| frame.drawn == Some(48));
    let epoch = entry.playhead().epoch();
    let paused = 3;
    entry.anchor(paused, false, 4.0, false);

    let (entered, waiting) = mpsc::channel();
    let (release, resume) = mpsc::channel::<()>();
    let mut gate = Some((entered, resume));
    let hook = entry.hooks().add(Box::new(move |_, _| {
        if let Some((entered, resume)) = gate.take() {
            let _ = entered.send(());
            let _ = resume.recv();
        }
    }));
    let mut reopened = map();
    waiting.recv().unwrap();
    drop(first);
    assert_eq!(entry.playhead().epoch(), epoch);
    assert_eq!(entry.playhead().newest(), Some(48));
    assert!(!reopened.reaches(paused));
    let frame = reopened.frame_still(paused).unwrap();
    drop(release);
    entry.hooks().remove(hook);

    assert!(frame.exact, "the reopened map must request a reset at the paused hour");
    assert_eq!(frame.drawn, None);
    crate::test_support::recorded::frames_at(&mut reopened, paused, "the reopened map's first frame", |_, frame| frame.drawn == Some(paused));
}

fn real_key_shapes() -> Vec<u128> {
    let mut keys: Vec<u128> = (1..=13_000).collect();
    keys.extend((0..1_500u128).flat_map(|country| (0..40u128).map(move |region| country << 32 | region)));
    keys.extend((0..2_000u128).flat_map(|combat| (0..2u128).flat_map(move |side| (0..24u128).map(move |ordinal| combat << 33 | side << 32 | ordinal))));
    keys.extend((0..400u128).flat_map(|group| (0..50u128).map(move |wing| (0x0000_7ff6_0000_0000 + group * 0x1000) << 64 | (0x0001_0000_0000 + wing * 0x40))));
    keys.extend((0..300u128).flat_map(|country| (0..20u128).flat_map(move |region| (0..4u128).map(move |level| country << 72 | region << 40 | level << 32 | 7))));
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn fixed_state_colliding_keys(count: usize, bucket_mask: u64) -> Vec<u128> {
    use std::hash::BuildHasher;
    let fixed = foldhash::fast::FixedState::default();
    let mut keys = Vec::with_capacity(count);
    let mut candidate = 1u128;
    while keys.len() < count {
        if fixed.hash_one(candidate) & bucket_mask == 0 {
            keys.push(candidate);
        }
        candidate += 1;
    }
    keys
}

#[test]
fn mirror_key_hasher_randomizes_instances_and_bounds_adversarial_keys() {
    use std::hash::BuildHasher;
    let _: std::collections::HashMap<u128, u32, foldhash::fast::RandomState> = std::collections::HashMap::with_hasher(super::mirror::Keys::default());

    let count = 512usize;
    let buckets = count.next_power_of_two();
    let adversarial = fixed_state_colliding_keys(count, (buckets - 1) as u64);
    let fixed = foldhash::fast::FixedState::default();
    assert!(adversarial.iter().all(|&key| fixed.hash_one(key) as usize & (buckets - 1) == 0));

    let alternate = foldhash::fast::FixedState::with_seed(0x9e37_79b9_7f4a_7c15);
    let mut load = vec![0u32; buckets];
    for &key in &adversarial {
        load[alternate.hash_one(key) as usize & (buckets - 1)] += 1;
    }
    let longest = *load.iter().max().unwrap();
    assert!(longest <= 12, "{longest} adversarial FixedState keys shared a bucket of {buckets}");

    let settings = Writing { restore_blocks: 1, whole_every: 1, pack_margin: 8, ..Writing::at(3) };
    let held = adversarial.clone();
    let (recording, _) = record_with(settings, 1, 2 * BLOCK, move |at, table, store| {
        store.status(table, Status::Complete, true, Some(at as i32), true).unwrap();
        let mut changes = store.table(table).unwrap();
        if at == 0 {
            for (slot, &key) in held.iter().take(256).enumerate() {
                changes.insert_at(slot as u32, key).unwrap();
                changes.write(slot as u32, 0, Cell::I32(slot as i32)).unwrap();
            }
        } else if at == 10 {
            for slot in 0..200u32 {
                changes.delete(slot).unwrap();
            }
        } else if at == 20 {
            for (offset, &key) in held.iter().skip(256).take(64).enumerate() {
                let slot = 256 + offset as u32;
                changes.insert_at(slot, key).unwrap();
                changes.write(slot, 0, Cell::I32(slot as i32)).unwrap();
            }
        }
    });
    assert!(recording.runs[0].held_pages().unwrap()[0].tables[0].packed, "the adversarial fixture packs its first block's end");
    let pages: Arc<dyn Pages> = recording;
    let columns = [0];
    let mut lane = Lane::new(Arc::clone(&pages), 0, 0, &columns, true).unwrap();
    lane.seek(0).unwrap();
    let mut mirror = Mirror::rebuild(&lane, &columns);
    let mut unindexed = mirror.clone();
    let before_lookup = mirror.bytes();
    for (slot, &key) in adversarial.iter().take(256).enumerate() {
        assert_eq!(mirror.slot(key), Some(slot as u32));
        assert_eq!(mirror.key(slot as u32), Some(key));
    }
    assert!(mirror.bytes() > before_lookup, "walking row keys needs no reverse index before the first lookup");
    assert_eq!(mirror.slot(adversarial[256]), None);
    let mut changes = Changes::default();
    let (mut to, mut renumbered) = (99, 0);
    while to < 2 * BLOCK as i32 {
        lane.fetch_through(to).unwrap();
        let head = lane.step(to, &mut changes).unwrap();
        mirror.apply(&changes);
        unindexed.apply(&changes);
        assert_eq!(differ(&mirror, lane.cells(), &columns), None, "after step to hour {head}");
        if changes.renumber().is_some() {
            renumbered += 1;
            assert_eq!(unindexed.slot(adversarial[0]), None, "first lookup sees the deletion before packing");
            assert_eq!(unindexed.slot(adversarial[200]), Some(0), "first lookup uses the new slot order");
            let live: Vec<u32> = mirror.rows().collect();
            assert_eq!(live, (0..120u32).collect::<Vec<_>>());
            for &key in &adversarial[..200] {
                assert_eq!(mirror.slot(key), None);
            }
            for (new_slot, &key) in adversarial[200..320].iter().enumerate() {
                assert_eq!(mirror.slot(key), Some(new_slot as u32));
                assert_eq!(mirror.key(new_slot as u32), Some(key));
            }
        }
        if head == to {
            to += 100;
        }
    }
    assert_eq!(renumbered, 1);
}

struct Tableless {
    composed: u32,
}

impl Layer for Tableless {
    fn name(&self) -> &'static str {
        "tableless"
    }
    fn inputs(&mut self, _view: &View, _inputs: &mut Inputs<'_>) -> io::Result<()> {
        Ok(())
    }
    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        self.composed += 1;
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

#[test]
fn a_layer_without_tables_on_a_map_joining_an_anchored_playhead_is_drawn() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "tableless join"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut first = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut first, |side, frame| frame.swapped.is_some() && !side.composing());
    let mut second = MapSide::new(Arc::clone(&entry), vec![Box::new(Tableless { composed: 0 })], View::default()).unwrap();
    let last = entry.playhead().timeline().last();
    loop {
        crate::test_support::recorded::reads_landed(&second);
        let before = entry.playhead().step_state();
        let head = before.0.unwrap_or(0);
        first.frame(head).unwrap();
        let frame = if second.composing() { second.frame_waiting(head) } else { second.frame(head) }.unwrap();
        if second.draws("tableless") {
            return;
        }
        if second.composing() || frame.steps > 0 || frame.swapped.is_some() {
            continue;
        }
        assert!(second.wait_step_after(before, last).unwrap(), "the joining map's layer without tables was never drawn");
    }
}

#[test]
fn a_map_composing_does_not_hold_the_steps_of_another_map() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "composing holds"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let mut first = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&composes))], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut first, |side, frame| frame.swapped.is_some() && !side.composing());
    let (gate, held) = mpsc::channel();
    let mut gated = Sum::new(&Arc::new(Mutex::new(0)));
    gated.gate = Some(Arc::new(Mutex::new(held)));
    let mut second = MapSide::new(Arc::clone(&entry), vec![Box::new(gated)], View::default()).unwrap();
    let last = entry.playhead().timeline().last();
    loop {
        crate::test_support::recorded::reads_landed(&second);
        let before = entry.playhead().step_state();
        let head = before.0.unwrap_or(0);
        first.frame(head).unwrap();
        let frame = second.frame(head).unwrap();
        if second.composing() {
            break;
        }
        if frame.steps > 0 || frame.swapped.is_some() {
            continue;
        }
        assert!(second.wait_step_after(before, last).unwrap(), "the second map never started composing");
    }
    assert!(second.composing(), "the second map composes, held by the test");
    let from = first.applied().unwrap();
    let target = from + 100;
    let mut reached = from;
    while reached < target {
        let before = entry.playhead().step_state();
        let head = before.0.unwrap_or(0);
        let frame = first.frame(head).unwrap();
        second.frame(head).unwrap();
        reached = first.applied().unwrap();
        if reached >= target {
            break;
        }
        if frame.steps > 0 || frame.swapped.is_some() {
            continue;
        }
        assert!(first.wait_step_after(before, target).unwrap(), "the first map stepped from {from} only to {reached} while the second composed");
    }
    gate.send(()).unwrap();
    assert!(reached >= target, "the first map stepped from {from} only to {reached} while the second composed");
}

struct Worked {
    next: Option<i32>,
    sent: Option<i32>,
    published: Option<i32>,
}

impl Layer for Worked {
    fn name(&self) -> &'static str {
        "worked"
    }
    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(table(0), false)?;
        inputs.index(table(0), 0)
    }
    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        (self.next, self.sent, self.published) = (None, None, mirrors.get(table(0)).and_then(|mirror| mirror.head()));
        Ok(())
    }
    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.next = Some(step.last());
    }
    fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        if let Some(sent) = self.sent.take() {
            self.published = Some(sent);
        }
        self.sent = self.next.take();
        Ok(())
    }
    fn carried(&self) -> Carried {
        Carried::default()
    }
    fn published(&self) -> crate::layers::Published {
        use crate::layers::Published;
        match self.sent {
            Some(_) => self.published.map_or(Published::Nothing, Published::At),
            None => Published::Current,
        }
    }
    fn held(&self) -> usize {
        0
    }
    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

fn worked(side: &MapSide) -> &Worked {
    side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<Worked>()).unwrap()
}

#[test]
fn a_map_with_a_worker_layer_is_drawn_at_its_shown_hour_and_keeps_up_while_playing() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "worked playing"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sum::new(&composes)), Box::new(Worked { next: None, sent: None, published: None })];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let first = side.last_frame().drawn;
    for _ in 0..20 {
        let head = side.applied().unwrap() + 4;
        entry.playhead().wait_newest(head).unwrap();
        let frame = side.frame_synced(head).unwrap();
        assert_eq!(frame.applied, Some(head), "one step a frame");
        assert_eq!(frame.drawn, Some(head - 4), "the worker holds the step: drawn at its product's hour, not the {head} applied");
        let frame = side.frame(head).unwrap();
        assert_eq!(frame.drawn, Some(head), "the product is back: drawn at the head");
    }
    assert!(side.last_frame().drawn > first, "the drawn hour advances");
}

struct Made {
    applied: Option<i32>,
    behind: Arc<std::sync::atomic::AtomicI32>,
}

impl Layer for Made {
    fn name(&self) -> &'static str {
        "made"
    }
    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(table(0), false)?;
        inputs.index(table(0), 0)
    }
    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.applied = mirrors.get(table(0)).and_then(|mirror| mirror.head());
        Ok(())
    }
    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.applied = Some(step.last());
    }
    fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        Ok(())
    }
    fn carried(&self) -> Carried {
        Carried::default()
    }
    fn published(&self) -> crate::layers::Published {
        self.applied.map_or(crate::layers::Published::Nothing, |hour| crate::layers::Published::Cadence(hour - self.behind.load(Ordering::Acquire)))
    }
    fn held(&self) -> usize {
        0
    }
    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn the_oldest_hour_on_screen_counts_a_layer_of_rows_at_the_head_while_it_is_behind() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "made behind"), quick()).unwrap();
    let behind = Arc::new(std::sync::atomic::AtomicI32::new(0));
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sum::new(&Arc::new(Mutex::new(0)))), Box::new(Made { applied: None, behind: Arc::clone(&behind) })];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    for lag in [3, 0, 5] {
        behind.store(lag, Ordering::Release);
        let head = side.applied().unwrap() + 4;
        entry.playhead().wait_newest(head).unwrap();
        let frame = side.frame_synced(head).unwrap();
        assert_eq!(frame.applied, Some(head), "one step a frame");
        assert_eq!(frame.drawn, Some(head), "the window clock is not held by a layer of rows at the head");
        assert_eq!(frame.oldest, Some(head - lag), "the oldest hour on screen counts it {lag} hours behind");
    }
}

struct Joined {
    applied: Option<i32>,
    made: Option<i32>,
    ran_out: u32,
}

impl Layer for Joined {
    fn name(&self) -> &'static str {
        "joined"
    }
    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(table(0), false)?;
        inputs.index(table(0), 0)
    }
    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.applied = mirrors.get(table(0)).and_then(|mirror| mirror.head());
        self.made = self.applied;
        Ok(())
    }
    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.applied = Some(step.last());
    }
    fn settle(&mut self, _mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        if settling.waiting(std::time::Duration::from_secs(60)).is_zero() {
            self.ran_out += 1;
        } else {
            self.made = self.applied;
        }
        Ok(())
    }
    fn carried(&self) -> Carried {
        Carried::default()
    }
    fn published(&self) -> crate::layers::Published {
        self.made.map_or(crate::layers::Published::Nothing, crate::layers::Published::Cadence)
    }
    fn held(&self) -> usize {
        0
    }
    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

fn joined(side: &MapSide) -> &Joined {
    side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<Joined>()).unwrap()
}

#[test]
fn a_step_joins_the_products_made_from_rows_at_the_head_and_stands_whole_at_its_hour() {
    let made = |name: &str| {
        let (recording, _) = record(1, BLOCK, busy);
        let entry = open(units(recording, name), quick()).unwrap();
        let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sum::new(&Arc::new(Mutex::new(0)))), Box::new(Joined { applied: None, made: None, ran_out: 0 })];
        let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
        entry.playhead().play(4.0);
        entry.playhead().seek(0);
        frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
        (side, entry)
    };
    let taking = |side: &mut MapSide, head: replay_data::Hour| {
        loop {
            let before = side.entry().playhead().step_state();
            let frame = side.frame_still(head).unwrap();
            if frame.steps > 0 {
                return frame;
            }
            assert!(side.wait_step_after(before, head).unwrap(), "no frame took the steps built to hour {head}");
        }
    };
    let (mut side, entry) = made("joined one by one");
    let ran_out = joined(&side).ran_out;
    let mut one_by_one = Vec::new();
    for _ in 0..8 {
        let head = side.applied().unwrap() + 4;
        entry.playhead().wait_newest(head).unwrap();
        let frame = taking(&mut side, head);
        assert_eq!((frame.applied, frame.steps), (Some(head), 1), "one step a frame");
        assert_eq!(frame.oldest, frame.applied, "the step's picture is whole at the hour applied");
        one_by_one.push((frame.drawn, frame.oldest, sum_of(side.parts().unwrap().0.get(table(0)).unwrap(), Num::of(table(0), 0))));
    }
    assert_eq!(joined(&side).ran_out, ran_out, "every step the head stood still for waited for the product of its hour");

    let (mut merged, entry) = made("joined merged");
    let head = merged.applied().unwrap() + 12;
    entry.playhead().wait_newest(head).unwrap();
    let frame = taking(&mut merged, head);
    assert_eq!((frame.applied, frame.steps), (Some(head), 3), "the steps the head reached are merged into one");
    let last = one_by_one[2];
    assert_eq!((frame.drawn, frame.oldest), (last.0, last.1), "merged steps leave the picture the steps one by one leave");
    assert_eq!(sum_of(merged.parts().unwrap().0.get(table(0)).unwrap(), Num::of(table(0), 0)), last.2, "and the same mirror");
}

#[test]
fn a_dependent_settles_every_frame_while_its_upstream_worker_holds_work() {
    struct Follower {
        settles: u64,
    }
    impl Layer for Follower {
        fn name(&self) -> &'static str {
            "follower"
        }
        fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
            inputs.after("worked");
            Ok(())
        }
        fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
            Ok(())
        }
        fn apply(&mut self, _step: &Step, _mirrors: &Mirrors) {}
        fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
            self.settles += 1;
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
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "follower"), quick()).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Worked { next: None, sent: None, published: None }), Box::new(Follower { settles: 0 })], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let follower = |side: &MapSide| side.parts().unwrap().1[1].any().downcast_ref::<Follower>().unwrap().settles;
    for _ in 0..20 {
        let before = follower(&side);
        let mut frames = 0;
        frames_until(&mut side, |_, frame| {
            frames += 1;
            frame.steps > 0
        });
        assert!(worked(&side).sent.is_some(), "the upstream's worker holds work");
        assert_eq!(follower(&side) - before, frames, "the dependent settled in every frame, the one that applied the step included");
    }
}

#[test]
fn held_the_map_is_drawn_at_the_head_only_once_its_worker_layer_shows_it() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "worked held"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Sum::new(&composes)), Box::new(Worked { next: None, sent: None, published: None })];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let head = side.applied().unwrap() + 4;
    entry.playhead().wait_newest(head).unwrap();
    let frame = side.frame_synced(head).unwrap();
    assert_eq!(frame.applied, Some(head), "the step to the held hour is applied");
    assert_ne!(frame.drawn, Some(head), "the worker has not shown it yet");
    let frame = side.frame(head).unwrap();
    assert_eq!(frame.drawn, Some(head), "drawn at the head once the worker shows it");
}

struct Meeting {
    name: &'static str,
    after: Option<&'static str>,
    meet: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    composed: Arc<Mutex<Vec<&'static str>>>,
}

impl Meeting {
    fn boxed(name: &'static str, after: Option<&'static str>, meet: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>, composed: &Arc<Mutex<Vec<&'static str>>>) -> Box<dyn Layer> {
        Box::new(Self { name, after, meet, composed: Arc::clone(composed) })
    }
}

impl Layer for Meeting {
    fn name(&self) -> &'static str {
        self.name
    }
    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if let Some(upstream) = self.after {
            inputs.after(upstream);
        }
        Ok(())
    }
    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        if let Some(upstream) = self.after
            && !self.composed.lock().unwrap().contains(&upstream)
        {
            return Err(io::Error::other(format!("{} composed before the {upstream} layer it reads", self.name)));
        }
        if let Some((started, other)) = &self.meet {
            let _ = started.send(());
            other.recv().map_err(|_| io::Error::other(format!("{} composed alone: the layer it meets stopped before starting", self.name)))?;
        }
        self.composed.lock().unwrap().push(self.name);
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

#[test]
fn layers_that_read_nothing_of_one_another_compose_at_once_and_a_dependent_after_its_upstream() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "composed at once"), quick()).unwrap();
    let composed = Arc::new(Mutex::new(Vec::new()));
    let ((to_u, from_a), (to_a, from_u)) = (mpsc::channel(), mpsc::channel());
    let layers = vec![
        Box::new(Sum::new(&Arc::new(Mutex::new(0)))) as Box<dyn Layer>,
        Meeting::boxed("a", None, Some((to_u, from_u)), &composed),
        Meeting::boxed("u", None, Some((to_a, from_a)), &composed),
        Meeting::boxed("d", Some("u"), None, &composed),
    ];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let refused: Vec<String> = side.refusals().map(|(name, why)| format!("{name}: {why}")).collect();
    assert!(refused.is_empty(), "refused: {refused:?}");
    assert!(["a", "u", "d"].iter().all(|name| side.draws(name)));
    let composed = composed.lock().unwrap();
    let at = |name: &str| composed.iter().position(|held| *held == name);
    assert!(at("u") < at("d"), "the dependent composed after its upstream: {composed:?}");
}

#[test]
fn a_second_seek_during_a_compose_discards_the_first() {
    let (recording, states) = record(1, BLOCK + 200, busy);
    let entry = open(units(recording, "superseded"), quick()).unwrap();
    let composes = Arc::new(Mutex::new(0));
    let (mut layer, release) = Sum::gated(&composes);
    let (entered, inside) = mpsc::channel();
    layer.entered = Some(entered);
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    frames_until(&mut side, |_, frame| frame.resets > 0);
    assert!(side.composing());
    inside.recv().expect("the first seek's compose began");
    entry.playhead().seek(300);
    release.send(()).unwrap();
    let mut drawn = Vec::new();
    let mut discarded = false;
    frames_until(&mut side, |_, frame| {
        assert!(frame.swapped.is_none(), "the first seek's compose is never swapped in");
        discarded |= frame.superseded;
        drawn.extend(frame.drawn);
        frame.resets > 0
    });
    assert!(discarded, "the first seek's compose came back and was discarded");
    inside.recv().expect("the newer seek's compose began");
    release.send(()).unwrap();
    frames_until(&mut side, |side, frame| {
        drawn.extend(frame.drawn);
        frame.swapped.is_some() && !side.composing()
    });
    assert!(drawn.iter().all(|&hour| hour >= 300), "an hour of the first seek was shown: {drawn:?}");
    assert_eq!(*composes.lock().unwrap(), 2);
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
    let head = mirror.head().unwrap();
    assert!(head >= 300, "the mirrors are at the newer seek: {head}");
    assert_eq!(differ(mirror, &states[0][head as usize], &[0]), None);
}

fn bare_consumer(entry: &super::Entry) -> playhead::Consumer {
    let reader = entry.playhead().declare(Vec::new(), None).unwrap();
    entry.playhead().consumer(reader)
}

fn take_reset(entry: &super::Entry, consumer: &mut playhead::Consumer, head: i32) {
    let mut taken: Vec<Arc<Step>> = Vec::new();
    loop {
        let before = entry.playhead().step_state();
        consumer.take(head, &mut taken);
        if taken.iter().any(|step| step.reset()) {
            return;
        }
        taken.clear();
        assert!(consumer.wait_step_after(before, head).unwrap(), "no reset reached hour {head}");
    }
}

fn runs_from(entry: &super::Entry, hour: i32, held_by: &str) {
    assert_eq!(entry.playhead().holding(), None, "the clock is held at the seek by {held_by}");
    let later = Instant::now() + Duration::from_secs(10);
    assert!(entry.playhead().head(later) > hour, "the head stays at {hour}, held by {held_by}");
}

#[test]
fn a_map_that_never_uploads_holds_no_clock_while_it_composes() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "never uploads"), quick()).unwrap();
    let (layer, release) = Sum::gated(&Arc::new(Mutex::new(0)));
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    frames_until(&mut side, |side, frame| frame.resets > 0 && side.composing());
    runs_from(&entry, 100, "a map composing its reset");
    release.send(()).unwrap();
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    assert_eq!(side.applied(), Some(100), "the swap shows the picture composed for the hour sought");
}

#[test]
fn a_hidden_map_holds_no_clock_for_a_reader_beside_it() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "hidden map"), quick()).unwrap();
    let hidden = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&Arc::new(Mutex::new(0))))], View::default()).unwrap();
    let mut rows = bare_consumer(&entry);
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    take_reset(&entry, &mut rows, 100);
    runs_from(&entry, 100, "a map never drawn");
    drop(hidden);
}

#[test]
fn a_map_with_every_layer_refused_holds_no_clock() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "every layer refused"), quick()).unwrap();
    let mut refused = Sum::new(&Arc::new(Mutex::new(0)));
    refused.refuse_high = true;
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(refused)], View { camera_height: 2.0, ..View::default() }).unwrap();
    assert_eq!(side.refusals().count(), 1);
    let mut rows = bare_consumer(&entry);
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    take_reset(&entry, &mut rows, 100);
    runs_from(&entry, 100, "a map with every layer refused");
    frames_until(&mut side, |side, _| side.applied().is_some_and(|hour| hour >= 100));
    assert!(!side.composing(), "nothing to compose");
}

#[test]
fn a_map_composing_slowly_holds_neither_the_clock_nor_another_map_on_its_playhead() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "slow second map"), quick()).unwrap();
    let mut first = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&Arc::new(Mutex::new(0))))], View::default()).unwrap();
    let (slow, release) = Sum::gated(&Arc::new(Mutex::new(0)));
    let mut second = MapSide::new(Arc::clone(&entry), vec![Box::new(slow)], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(100);
    let (mut one_reset, mut other_reset) = (false, false);
    loop {
        crate::test_support::recorded::reads_landed(&first);
        let before = entry.playhead().step_state();
        let head = before.0.unwrap_or(0);
        if !one_reset {
            one_reset = first.frame_synced(head).unwrap().resets > 0;
        }
        if !other_reset {
            other_reset = second.frame_synced(head).unwrap().resets > 0;
        }
        if one_reset && other_reset {
            break;
        }
        let waiter = if !one_reset { &first } else { &second };
        assert!(waiter.wait_step_after(before, 100).unwrap(), "the maps did not take the reset at hour 100");
    }
    runs_from(&entry, 100, "the maps' composes");
    let target = 100 + 3 * 4;
    loop {
        let before = entry.playhead().step_state();
        let head = before.0.unwrap_or(0);
        let frame = if first.composing() { first.frame_waiting(head) } else { first.frame(head) }.unwrap();
        second.frame(head).unwrap();
        if !first.composing() && first.applied().is_some_and(|hour| hour >= target) {
            break;
        }
        if first.composing() || frame.steps > 0 || frame.swapped.is_some() {
            continue;
        }
        assert!(first.wait_step_after(before, target).unwrap(), "the first map did not play past the hour sought while the second composed");
    }
    assert!(second.composing(), "the second map still composes, its old picture on screen");
    release.send(()).unwrap();
}

#[test]
fn a_follow_host_is_held_neither_by_a_map_beside_it_nor_by_its_frames() {
    let (recording, states) = record(1, 2 * BLOCK, busy);
    let settings = Settings { rate: 60.0, ..quick() };
    let entry = open(units(recording, "follow beside a map"), settings).unwrap();
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(Sum::new(&Arc::new(Mutex::new(0))))], View::default()).unwrap();
    let held = dashboard_follow::host::held_tables(&**entry.pages(), entry.kinds().len());
    let anchored = |entry: &super::Entry, anchor: dashboard_follow::registry::Anchor| entry.anchor(anchor.hour, anchor.playing, anchor.speed, anchor.sought);
    let host = dashboard_follow::host::PlayheadHost::new(Arc::clone(&entry), super::Entry::playhead, anchored, dashboard_follow::Follow::new(0), held).unwrap();
    entry.playhead().play(240.0);
    entry.playhead().seek(100);
    entry.playhead().wait_for(|| entry.playhead().holding().is_none()).unwrap();
    runs_from(&entry, 100, "the map beside the follow host");
    frames_until(&mut side, |side, frame| frame.applied.is_some_and(|hour| hour >= 100) && !side.composing());
    let last = side.last_frame().head;
    let past = last + 4 * (settings.ring as i32 + 2);
    entry.playhead().wait_newest(past).unwrap();
    frames_until(&mut side, |side, frame| frame.applied.is_some_and(|hour| hour >= past) && !side.composing());
    side.settle_all().unwrap();
    let mirror = side.parts().unwrap().0.get(table(0)).unwrap();
    let hour = mirror.head().unwrap();
    assert_eq!(differ(mirror, &states[0][hour as usize], &[0]), None, "the map let go, at hour {hour}");
    assert_eq!(sum_layer(&side, 0).total(), sum_of(mirror, Num::of(table(0), 0)), "the layer's arrays at hour {hour}");
    drop(host);
    drop(side);
}

type Permits = Arc<(Mutex<Option<u32>>, std::sync::Condvar)>;
type AheadDone = Arc<(Mutex<(u64, Option<i32>, bool)>, std::sync::Condvar)>;

struct AheadStop(AheadDone);

impl Drop for AheadStop {
    fn drop(&mut self) {
        let (held, wake) = &*self.0;
        let mut done = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        done.2 = true;
        wake.notify_all();
    }
}

enum Fold {
    Reset { epoch: u64, values: Vec<Option<i32>> },
    Step { epoch: u64, step: Arc<Step> },
}

type Summed = (u64, Option<i32>, i64);

struct Ahead {
    fed: crate::layers::Fed<Summed>,
    epoch: u64,
    jobs: Option<mpsc::Sender<Fold>>,
    made: Option<viewer_runtime::landed::Receiver<Summed>>,
    permits: Permits,
    done: AheadDone,
    shown: Option<Summed>,
    handed_through: Option<i32>,
    made_through: Option<i32>,
    changed: crate::layers::Changed,
}

impl Ahead {
    fn new(permits: Option<u32>) -> (Self, Permits) {
        let permits: Permits = Arc::new((Mutex::new(permits), std::sync::Condvar::new()));
        let done: AheadDone = Arc::new((Mutex::new((0, None, false)), std::sync::Condvar::new()));
        (Self { fed: crate::layers::Fed::default(), epoch: 0, jobs: None, made: None, permits: Arc::clone(&permits), done, shown: None, handed_through: None, made_through: None, changed: crate::layers::Changed::default() }, permits)
    }

    fn wait_made(&self, hour: i32) {
        let (held, wake) = &*self.done;
        let mut done = held.lock().unwrap();
        while !(done.0 == self.epoch && done.1 >= Some(hour)) {
            assert!(!done.2, "the ahead worker stopped before producing hour {hour}");
            done = wake.wait(done).unwrap();
        }
    }

    fn run(jobs: mpsc::Receiver<Fold>, made: viewer_runtime::landed::Sender<Summed>, permits: Permits, state: AheadDone) {
        let _stop = AheadStop(Arc::clone(&state));
        let (mut values, mut epoch, mut stamp): (Vec<Option<i32>>, u64, Option<i32>) = (Vec::new(), 0, None);
        for job in jobs {
            {
                let (held, wake) = &*permits;
                let mut left = held.lock().unwrap();
                while *left == Some(0) {
                    left = wake.wait(left).unwrap();
                }
                if let Some(left) = left.as_mut() {
                    *left -= 1;
                }
            }
            match job {
                Fold::Reset { epoch: next, values: whole } => (epoch, values, stamp) = (next, whole, None),
                Fold::Step { epoch: of, step } if of == epoch => {
                    if let Some(changes) = step.table(0) {
                        for &slot in changes.deleted().slots() {
                            values[slot as usize] = None;
                        }
                        for written in changes.columns().iter().filter(|written| written.column() == 0) {
                            for (slot, value) in written.values() {
                                if values.len() <= slot as usize {
                                    values.resize(slot as usize + 1, None);
                                }
                                values[slot as usize] = match value {
                                    Value::Number(bits) => Some(bits as i32),
                                    _ => None,
                                };
                            }
                        }
                    }
                    stamp = stamp.max(Some(step.last()));
                }
                Fold::Step { .. } => continue,
            }
            if made.send((epoch, stamp, values.iter().flatten().copied().map(i64::from).sum())).is_err() {
                return;
            }
            let (held, wake) = &*state;
            let mut done = held.lock().unwrap();
            if epoch > done.0 {
                *done = (epoch, stamp, false);
            } else if epoch == done.0 {
                done.1 = done.1.max(stamp);
            }
            wake.notify_all();
        }
    }

    fn exchange(&mut self, wait: bool) {
        if let Some(receive) = &self.made {
            loop {
                let made = if wait && !self.fed.arrived() { receive.recv().ok() } else { receive.try_recv().ok() };
                let Some(made) = made else { break };
                if made.0 == self.epoch {
                    self.made_through = self.made_through.max(made.1);
                    self.fed.receive(made.1, made);
                }
            }
        }
        let (mut shown, mut changed) = (self.shown, Vec::new());
        self.fed.take(|made| {
            let made = made.into_inner();
            shown = Some(made);
            changed.extend(made.1);
        });
        self.shown = shown;
        for hour in changed {
            self.changed.mark("hour", hour as u64);
        }
    }
}

fn permit(permits: &Permits, more: Option<u32>) {
    let (held, wake) = &**permits;
    let mut left = held.lock().unwrap();
    *left = more.map(|more| left.unwrap_or(0) + more);
    wake.notify_all();
}

impl Layer for Ahead {
    fn name(&self) -> &'static str {
        "ahead"
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.table(table(0), true)?;
        inputs.index(table(0), 0)
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = viewer_runtime::landed::channel();
            let permits = Arc::clone(&self.permits);
            let done = Arc::clone(&self.done);
            std::thread::Builder::new().name("ahead test fold".into()).spawn(move || Ahead::run(receive, send, permits, done))?;
            (self.jobs, self.made) = (Some(jobs), Some(made));
        }
        let mirror = mirrors.get(table(0)).unwrap();
        let column = Num::of(table(0), 0);
        let values = (0..mirror.slots() as u32).map(|slot| if mirror.live(slot) { mirror.number(column, slot).value() } else { None }).collect();
        self.epoch += 1;
        (self.shown, self.handed_through, self.made_through) = (None, None, None);
        self.fed.compose(mirror.head());
        self.jobs.as_ref().unwrap().send(Fold::Reset { epoch: self.epoch, values }).unwrap();
        self.exchange(true);
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.fed.applied(step);
    }

    fn wants_ahead(&self) -> bool {
        true
    }

    fn ahead(&mut self, steps: &[Arc<Step>]) {
        let jobs = self.jobs.as_ref().unwrap();
        for step in self.fed.hand(steps) {
            self.handed_through = self.handed_through.max(Some(step.last()));
            jobs.send(Fold::Step { epoch: self.epoch, step: Arc::clone(step) }).unwrap();
        }
    }

    fn settle(&mut self, _mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.exchange(settling.strict());
        Ok(())
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> crate::layers::Published {
        self.fed.published()
    }

    fn held(&self) -> usize {
        0
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    fn changed(&self) -> Option<&crate::layers::Changed> {
        Some(&self.changed)
    }

    fn clear_changed(&mut self) {
        self.changed.clear();
    }
}

struct Hears {
    stepped: Option<i32>,
    heard: Vec<(Option<i32>, Vec<u64>)>,
}

impl Layer for Hears {
    fn name(&self) -> &'static str {
        "hears"
    }
    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        inputs.after("ahead");
        Ok(())
    }
    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        Ok(())
    }
    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.stepped = Some(step.last());
    }
    fn settle(&mut self, _mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        Ok(())
    }
    fn settle_with(&mut self, _mirrors: &Mirrors, upstream: &super::Upstream<'_>, _settling: Settling) -> io::Result<()> {
        let named = match upstream.changed::<Ahead>() {
            crate::layers::Change::Whole => Vec::new(),
            crate::layers::Change::Marked(marked) => marked.of("hour").to_vec(),
        };
        self.heard.push((self.stepped.take(), named));
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

fn ahead(side: &MapSide) -> &Ahead {
    side.parts().unwrap().1.iter().find_map(|layer| layer.any().downcast_ref::<Ahead>()).unwrap()
}

fn ahead_side(name: &str, permits: Option<u32>) -> (MapSide, Arc<super::Entry>, Permits) {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, name), quick()).unwrap();
    let (layer, permits) = Ahead::new(permits);
    let mut side = MapSide::new(Arc::clone(&entry), vec![Box::new(layer), Box::new(Hears { stepped: None, heard: Vec::new() })], View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    (side, entry, permits)
}

fn made_ahead(side: &mut MapSide, hour: i32) {
    side.entry().playhead().wait_newest(hour).unwrap();
    let applied = side.applied().unwrap();
    side.frame_synced(applied).unwrap();
    assert!(ahead(side).handed_through >= Some(hour), "the ring never handed hour {hour} to the ahead worker");
    ahead(side).wait_made(hour);
    side.frame_synced(applied).unwrap();
    assert!(ahead(side).made_through >= Some(hour), "the worker never made the product of hour {hour}");
}

fn until_current(side: &mut MapSide, hour: i32) {
    ahead(side).wait_made(hour);
    side.frame_synced(hour).unwrap();
    assert_eq!(ahead(side).published(), crate::layers::Published::Current, "the layer never showed hour {hour}");
}

fn mirrored(side: &MapSide) -> i64 {
    sum_of(side.parts().unwrap().0.get(table(0)).unwrap(), Num::of(table(0), 0))
}

#[test]
fn a_step_made_ahead_is_shown_current_in_the_frame_that_applies_it() {
    let (mut side, _entry, _permits) = ahead_side("ahead current", None);
    for _ in 0..10 {
        let next = side.applied().unwrap() + 4;
        made_ahead(&mut side, next);
        let frame = step_frame(&mut side, next);
        assert_eq!((frame.applied, frame.steps), (Some(next), 1), "one step a frame");
        assert_eq!(frame.layers[0].ready_at_hour, Some(true), "the product of hour {next} was ready in the frame that applied it");
        assert_eq!(ahead(&side).published(), crate::layers::Published::Current);
        assert_eq!(frame.drawn, Some(next), "the map is drawn at the hour applied");
        assert_eq!(ahead(&side).shown.map(|(_, hour, sum)| (hour, sum)), Some((Some(next), mirrored(&side))), "the product shown is the mirror's at {next}");
    }
}

#[test]
fn a_held_worker_leaves_the_layer_at_the_newest_hour_it_made_and_never_shows_one_ahead() {
    let (mut side, entry, permits) = ahead_side("ahead held", Some(1));
    let from = side.applied().unwrap();
    entry.playhead().wait_newest(from + 12).unwrap();
    permit(&permits, Some(2));
    made_ahead(&mut side, from + 8);
    let frame = step_frame(&mut side, from + 4);
    assert_eq!(frame.applied, Some(from + 4));
    assert_eq!(ahead(&side).shown.map(|(_, hour, _)| hour), Some(Some(from + 4)), "the product of {} is made; not shown before its hour", from + 8);
    assert_eq!(frame.drawn, Some(from + 4));
    let frame = step_frame(&mut side, from + 8);
    assert_eq!((frame.drawn, frame.layers[0].ready_at_hour), (Some(from + 8), Some(true)));
    let frame = step_frame(&mut side, from + 12);
    assert_eq!(frame.applied, Some(from + 12));
    assert_eq!(frame.layers[0].ready_at_hour, Some(false), "the step to {} is not folded", from + 12);
    assert_eq!(ahead(&side).published(), crate::layers::Published::At(from + 8));
    assert_eq!(frame.drawn, Some(from + 8), "the map stays at the newest hour the worker made");
    let shown = ahead(&side).shown.unwrap();
    assert_eq!(shown.1, Some(from + 8));
    permit(&permits, None);
    until_current(&mut side, from + 12);
    assert_eq!(ahead(&side).shown.map(|(_, hour, sum)| (hour, sum)), Some((Some(from + 12), mirrored(&side))));
}

#[test]
fn a_layer_whose_product_is_older_than_the_hour_is_named_behind_for_a_sample() {
    let (mut side, entry, permits) = ahead_side("ahead behind", Some(1));
    let from = side.applied().unwrap();
    entry.playhead().wait_newest(from + 12).unwrap();
    permit(&permits, Some(2));
    made_ahead(&mut side, from + 8);
    step_frame(&mut side, from + 8);
    let frame = step_frame(&mut side, from + 12);
    assert_eq!(frame.applied, Some(from + 12));
    assert_eq!(ahead(&side).published(), crate::layers::Published::At(from + 8));
    assert_eq!(
        crate::comparison::behind_at(&side, from + 12),
        vec![format!("ahead publishes At({})", from + 8)],
        "the layer is a step behind the hour the sample asks for"
    );
    assert!(crate::comparison::behind_at(&side, from + 8).is_empty(), "it stands at the hour it published");
    permit(&permits, None);
    until_current(&mut side, from + 12);
    assert!(crate::comparison::behind_at(&side, from + 12).is_empty(), "its product of the hour arrived");
}

#[test]
fn two_steps_taken_in_one_frame_show_the_seconds_product() {
    let (mut side, _entry, _permits) = ahead_side("ahead two", None);
    let from = side.applied().unwrap();
    made_ahead(&mut side, from + 8);
    let frame = step_frame(&mut side, from + 8);
    assert_eq!((frame.applied, frame.steps), (Some(from + 8), 2));
    assert_eq!(frame.layers[0].ready_at_hour, Some(true));
    assert_eq!(ahead(&side).shown.map(|(_, hour, sum)| (hour, sum)), Some((Some(from + 8), mirrored(&side))));
    assert_eq!(frame.drawn, Some(from + 8));
}

#[test]
fn a_seek_shows_none_of_the_products_made_ahead_before_it() {
    let (mut side, entry, _permits) = ahead_side("ahead seek", None);
    let from = side.applied().unwrap();
    made_ahead(&mut side, from + 8);
    let before = ahead(&side).epoch;
    entry.playhead().seek(from + 2);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    let epoch = ahead(&side).epoch;
    assert!(epoch > before, "the seek composed the layer again");
    for _ in 0..6 {
        let next = side.applied().unwrap() + 4;
        made_ahead(&mut side, next);
        step_frame(&mut side, next);
        let shown = ahead(&side).shown.unwrap();
        assert_eq!(shown.0, epoch, "a product made before the seek was shown at hour {next}");
        assert_eq!((shown.1, shown.2), (Some(next), mirrored(&side)));
    }
}

#[test]
fn a_dependent_hears_a_fed_layers_change_in_the_frame_of_its_step() {
    let (mut side, _entry, _permits) = ahead_side("ahead heard", None);
    for _ in 0..5 {
        let next = side.applied().unwrap() + 4;
        made_ahead(&mut side, next);
        step_frame(&mut side, next);
        let hears = side.parts().unwrap().1[1].any().downcast_ref::<Hears>().unwrap();
        let (stepped, heard) = hears.heard.last().unwrap();
        assert_eq!(*stepped, Some(next), "the dependent settled in the frame that applied {next}");
        assert!(heard.contains(&(next as u64)), "the change of hour {next} reached it in that frame: {heard:?}");
    }
}

struct Camera {
    name: &'static str,
    takes: bool,
    declared: Arc<AtomicU32>,
}

impl Layer for Camera {
    fn name(&self) -> &'static str {
        self.name
    }
    fn placed(&mut self, view: &View) -> Option<u64> {
        self.takes.then(|| u64::from(view.camera_height < 100.0))
    }
    fn inputs(&mut self, _view: &View, _inputs: &mut Inputs<'_>) -> io::Result<()> {
        self.declared.fetch_add(1, Ordering::Relaxed);
        Ok(())
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

#[test]
fn a_moved_camera_declares_again_only_what_the_camera_decides() {
    let (recording, _) = record(1, BLOCK, busy);
    let entry = open(units(recording, "camera placed"), quick()).unwrap();
    let (taker, reader) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let layers: Vec<Box<dyn Layer>> = vec![
        Box::new(Camera { name: "taker", takes: true, declared: Arc::clone(&taker) }),
        Box::new(Camera { name: "reader", takes: false, declared: Arc::clone(&reader) }),
    ];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View { camera_height: 10.0, ..View::default() }).unwrap();
    let counts = || (taker.load(Ordering::Relaxed), reader.load(Ordering::Relaxed));
    assert_eq!(counts(), (1, 1));
    side.set_view(View { camera_height: 50.0, viewport: [800, 600], ..View::default() }).unwrap();
    assert_eq!(counts(), (1, 2), "a moved camera: the taker takes it, the reader declares again alone");
    side.set_view(View { camera_height: 150.0, viewport: [800, 600], ..View::default() }).unwrap();
    assert_eq!(counts(), (2, 3), "the camera crossed what the taker's declaration decides: the view is declared whole");
    side.set_view(View { camera_height: 150.0, viewport: [800, 600], fog: !View::default().fog, ..View::default() }).unwrap();
    assert_eq!(counts(), (3, 4), "another change of the view declares it whole");
}

struct Maker {
    name: &'static str,
    column: Option<usize>,
    moved: Option<i32>,
    jobs: mpsc::Sender<i32>,
    made: mpsc::Receiver<i32>,
    asks: Arc<AtomicU32>,
    out: usize,
    product: Option<i32>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Maker {
    fn new(name: &'static str, together: &Arc<std::sync::Barrier>) -> Self {
        let (jobs, taken) = mpsc::channel::<i32>();
        let (answers, made) = mpsc::channel::<i32>();
        let together = Arc::clone(together);
        let worker = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let mut first = true;
                while let Ok(hour) = taken.recv() {
                    if std::mem::take(&mut first) {
                        together.wait();
                    }
                    if answers.send(hour).is_err() {
                        return;
                    }
                }
            })
            .expect("a worker thread");
        Self { name, column: None, moved: None, jobs, made, asks: Arc::new(AtomicU32::new(0)), out: 0, product: None, worker: Some(worker) }
    }
}

impl Drop for Maker {
    fn drop(&mut self) {
        let (jobs, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Layer for Maker {
    fn name(&self) -> &'static str {
        self.name
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let at = usize::from(self.name == "second maker");
        inputs.table(table(at), true)?;
        inputs.index(table(at), 0)?;
        self.column = Some(at);
        Ok(())
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        let read = self.column.expect("a composed maker reads its column");
        if step.tables().iter().any(|changes| changes.table() == read && changes.columns().iter().any(|written| written.column() == 0 && !written.is_empty())) {
            self.moved = Some(step.last());
        }
    }

    fn ask(&mut self, _mirrors: &Mirrors, _upstream: &crate::layers::Upstream<'_>) -> io::Result<bool> {
        if let Some(hour) = self.moved.take() {
            self.asks.fetch_add(1, Ordering::SeqCst);
            self.out += 1;
            let _ = self.jobs.send(hour);
        }
        Ok(true)
    }

    fn join(&mut self, _mirrors: &Mirrors, _upstream: &crate::layers::Upstream<'_>, _settling: Settling) -> io::Result<()> {
        while self.out > 0 {
            let hour = self.made.recv().map_err(|_| io::Error::other(format!("{} worker stopped before answering", self.name)))?;
            (self.product, self.out) = (Some(hour), self.out - 1);
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

fn maker_of(side: &MapSide, name: &str) -> (u32, Option<i32>) {
    let layer = side.parts().unwrap().1.iter().filter_map(|layer| layer.any().downcast_ref::<Maker>()).find(|maker| maker.name == name).expect("the maker layer");
    (layer.asks.load(Ordering::SeqCst), layer.product)
}

#[test]
fn makers_asked_at_the_applies_build_side_by_side_within_one_step() {
    let (recording, _) = record(2, BLOCK, busy);
    let entry = open(units(recording, "makers side by side"), quick()).unwrap();
    let together = Arc::new(std::sync::Barrier::new(2));
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Maker::new("first maker", &together)), Box::new(Maker::new("second maker", &together))];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    frames_until(&mut side, |side, _| maker_of(side, "first maker").1.is_some() && maker_of(side, "second maker").1.is_some());
    let (first, second) = (maker_of(&side, "first maker"), maker_of(&side, "second maker"));
    assert!(first.1.is_some() && second.1.is_some(), "each maker's product joined a step: {first:?}, {second:?}");
}

#[test]
fn a_step_that_changes_nothing_a_maker_reads_asks_it_nothing() {
    let quiet = |at: u32, table: usize, store: &mut Store| {
        if table == 0 || at == 0 {
            busy(at, table, store);
        } else {
            store.status(table, Status::Complete, true, Some(at as i32), true).unwrap();
        }
    };
    let (recording, _) = record(2, BLOCK, quiet);
    let entry = open(units(recording, "a quiet maker"), quick()).unwrap();
    let together = Arc::new(std::sync::Barrier::new(1));
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Maker::new("first maker", &together)), Box::new(Maker::new("second maker", &together))];
    let mut side = MapSide::new(Arc::clone(&entry), layers, View::default()).unwrap();
    entry.playhead().play(4.0);
    entry.playhead().seek(0);
    frames_until(&mut side, |side, frame| frame.swapped.is_some() && !side.composing());
    frames_until(&mut side, |side, _| maker_of(side, "first maker").0 >= 4);
    let (asked, product) = maker_of(&side, "second maker");
    assert_eq!(asked, 0, "the second maker was asked nothing: its column moved in no step after the reset, it holds {product:?}");
}

#[test]
fn apart_drive_stops_when_a_read_fails() {
    struct FailingExtents;
    impl replay_columnar::unit::reader::Extents for FailingExtents {
        fn read_at(&self, _offset: u64, _length: usize, _out: &mut Vec<u8>) -> std::io::Result<()> {
            Err(std::io::Error::other("injected page read failure"))
        }
    }
    struct FailingPages {
        inner: Arc<Recording>,
    }
    impl playhead::pages::Pages for FailingPages {
        fn extents(&self, _run: usize) -> &dyn replay_columnar::unit::reader::Extents {
            &FailingExtents
        }
        fn runs(&self) -> &[playhead::pages::Run] {
            &self.inner.runs
        }
        fn kinds(&self, table: usize) -> Option<&[replay_columnar::state::Kind]> {
            self.inner.kinds.get(table).map(Vec::as_slice)
        }
    }
    let (recording, _) = record(1, 8, busy);
    let kinds = Arc::new(recording.kinds.clone());
    let pages: Arc<dyn playhead::pages::Pages> = Arc::new(FailingPages { inner: recording });
    let timeline = playhead::Timeline::whole(&*pages).unwrap();
    let entry = open(
        crate::playhead_host::units::Units {
            name: "apart read failure".into(),
            pages,
            timeline,
            kinds,
        },
        quick(),
    )
    .unwrap();
    let mut apart = crate::playhead_host::views::apart::Apart::new(&entry, 1).unwrap();
    let _reads = apart.store().reader("reads", &[crate::playhead_host::views::Need::every(table(0))]);
    entry.playhead().seek(4);
    assert!(!apart.drive(4), "drive must return false instead of spinning after a read error");
    assert!(
        entry.playhead().error().is_some_and(|why| why.contains("injected page read failure")),
        "the playhead reports the read error: {:?}",
        entry.playhead().error()
    );
}

