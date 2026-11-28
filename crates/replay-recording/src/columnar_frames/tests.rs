use crate::schema::{LAST_COMPLETE, columns_at, schema, withheld_at};
use super::test_support::read_on;
use replay_columnar::state::Kind as Column;
use replay_journal::fixtures::{self, every, recording};
use super::*;
use replay_data::rows::TableRows;
use replay_columnar::{
    container::Root,
    envelope::{self, Kind},
};
use std::sync::atomic::{AtomicU64, Ordering};

fn opened(bytes: Vec<u8>) -> Recording {
    Recording::open(bytes).unwrap()
}

struct Saved(std::path::PathBuf);

impl Saved {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("columnar-frames-{}-{}.battleplan", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }

    fn open(&self) -> Recording {
        Recording::open_path(&self.0).unwrap()
    }

    fn append(&self, bytes: &[u8]) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&self.0).unwrap().write_all(bytes).unwrap();
    }
}

impl Drop for Saved {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn append_revision(bytes: &mut Vec<u8>, revision: u64) {
    let (extent, _) = envelope::last_root(bytes).unwrap();
    let stored = extent.stored_in(bytes).unwrap();
    let raw = zstd::bulk::decompress(stored, 1 << 20).unwrap();
    let mut root = Root::read(&raw).unwrap();
    root.revision = revision;
    let root = fixtures::append(bytes, Kind::Root, (0, 0, 0, 0), &root.write().unwrap());
    bytes.extend_from_slice(&envelope::footer(root.offset, root.stored, &root.digest));
}

fn same(read: &fixtures::Seen, written: &fixtures::Seen) -> bool {
    read.0.branch == written.0.branch && read.0.hour == written.0.hour && read.0.tables == written.0.tables && read.1 == written.1
}

const CHURNED: [(u64, i32, u32); 5] = [
    (1, 100, 5),
    (1, 100 + 5 * envelope::UNIT_ORDINALS as i32, 4),
    (2, 50_000, 3),
    (3, 90_000, 6),
    (2, 200_000, 2),
];

const WATCHED: [TableId; 2] = [TableId::Countries, TableId::WorldTension];

#[test]
fn frames_read_on_are_the_writer_s_tables_at_every_hour_of_every_run() {
    let (bytes, expected) = fixtures::churned(recorder_derive::writers(), &CHURNED);
    let disk = Saved::new(&bytes);
    for file in [opened(bytes), disk.open()] {
        assert_eq!(file.run_count(), 5);
        assert_eq!(file.segments().len(), 5);
        assert_eq!(file.hours(), expected.len() as u64);
        let read = read_on(&file, &WATCHED);
        assert_eq!(read.len(), expected.len());
        for (at, (read, written)) in read.iter().zip(&expected).enumerate() {
            assert!(same(read, written), "hour {at} ({}) differs", written.0.hour);
        }
    }
}

#[test]
fn a_derived_table_reads_back_as_its_writer_wrote_it_whatever_the_hours_say() {
    use replay_journal::synthetic::{Input, Sample};
    use replay_data::rows::{BalanceDropRow, CountryRow, CountryStabilityInputRow, CountryStabilityRow, Recorded};
    let derived = TableId::BalanceDrops;
    assert!(recorder_derive::writers().tables().contains(&derived.index()));
    const FIRST: i32 = 100;
    let power = |ordinal: u32| i64::from(ordinal % 5) * 1_000;
    let inputs = CountryStabilityInputRow {
        country: 9,
        stability_raw: 50_000,
        war_support_raw: 20_000,
        max_stability: 100_000,
        max_war_support: 100_000,
        base_max_command_power: 30_000_000,
        stability_factor: Some(0),
        party_popularity_stability_factor: Some(0),
        war_support_factor: Some(0),
        keys: vec![1, 2, u32::MAX, u32::MAX, u32::MAX, 6, u32::MAX, u32::MAX, u32::MAX, u32::MAX],
        ..Default::default()
    };
    let streams = [TableId::Countries, TableId::CountryStabilityInputs, derived, TableId::CountryStability];
    let (bytes, _) = fixtures::written(recorder_derive::writers(), b"derived", &[(1, FIRST, 1)], &streams, |synthetic, _, ordinal| {
        let rows = [CountryRow { id: 9, tag: b"GER".to_vec(), political_power_raw: Some(power(ordinal)), ..Default::default() }];
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &rows, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        let read = std::slice::from_ref(&inputs);
        synthetic.table(TableId::CountryStabilityInputs, Input::Snapshot { rows: read, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        synthetic.table::<BalanceDropRow>(derived, Input::Unavailable { reason: "the hours' own word", read: false, sample: Sample::FromReadBit }).unwrap();
    });
    let recorded = |value: Option<i64>| value.map_or(Recorded::NotRecorded, Recorded::Value);
    let stability = CountryStabilityRow {
        country: 9,
        stability: recorded(replay_data::country_stability::stability(&inputs).value),
        war_support: recorded(replay_data::country_stability::war_support(&inputs).value),
        max_command_power: Recorded::NotRecorded,
    };
    assert!(matches!((stability.stability, stability.war_support), (Recorded::Value(_), Recorded::Value(_))), "the fixture's stability and war support are recorded");
    let read = read_on(&opened(bytes), &[derived, TableId::CountryStability]);
    assert_eq!(read.len(), envelope::UNIT_ORDINALS as usize);
    let mut drops = 0;
    for (ordinal, (frame, _)) in (0..).zip(&read) {
        let fell = ordinal > 0 && power(ordinal) < power(ordinal - 1);
        let rows: Vec<BalanceDropRow> = fell
            .then(|| BalanceDropRow { kind: 0, country: 9, hour: FIRST + ordinal as i32, before: power(ordinal - 1), after: power(ordinal) })
            .into_iter()
            .collect();
        drops += rows.len();
        assert_eq!(*frame.tables[derived.index()], TableView::Complete(TableRows::BalanceDrops(rows)), "hour {ordinal}");
        assert_eq!(*frame.tables[TableId::CountryStability.index()], TableView::Complete(TableRows::CountryStability(vec![stability.clone()])), "hour {ordinal}");
    }
    assert_eq!(drops, (envelope::UNIT_ORDINALS as usize - 1) / 5);
}

#[test]
fn a_table_whose_row_cannot_say_not_recorded_is_withheld_by_name_not_the_hour() {
    use replay_journal::synthetic::{Input, Sample};
    use replay_data::rows::{CountryExperienceUnsayableRow, CountryRow, Recorded};
    let withheld = TableId::CountryExperience;
    let countries = [CountryRow { id: 9, tag: b"GER".to_vec(), ..Default::default() }];
    let (bytes, _) = fixtures::written(recorder_derive::writers(), b"withheld", &[(1, 100, 1)], &[TableId::Countries], |synthetic, _, ordinal| {
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        let owned = if ordinal % 2 == 0 { Recorded::NotRecorded } else { Recorded::Value(5) };
        let rows = [CountryExperienceUnsayableRow { country: 9, army_raw: owned, ..Default::default() }];
        synthetic.table(withheld, Input::Snapshot { rows: &rows, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
    });
    let read = read_on(&opened(bytes), &[TableId::Countries, withheld]);
    assert_eq!(read.len(), envelope::UNIT_ORDINALS as usize);
    for (ordinal, (frame, _)) in (0..).zip(&read) {
        assert_eq!(*frame.tables[TableId::Countries.index()], TableView::Complete(TableRows::Countries(countries.to_vec())), "hour {ordinal}");
        let view = &*frame.tables[withheld.index()];
        if ordinal % 2 == 0 {
            let reason = "CountryExperience.army_raw is not recorded and its row cannot say so";
            assert_eq!(*view, TableView::Unavailable { reason: reason.into() }, "hour {ordinal}");
        } else {
            let TableView::Complete(TableRows::CountryExperience(rows)) = view else { panic!("hour {ordinal}: {view:?}") };
            assert_eq!((rows.len(), rows[0].country, rows[0].army_raw), (1, 9, 5), "hour {ordinal}");
        }
    }
}

#[test]
fn a_frame_sought_at_any_hour_is_the_one_reading_on_reaches() {
    let (bytes, expected) = fixtures::churned(recorder_derive::writers(), &CHURNED);
    let file = opened(bytes);
    let mut starts = Vec::new();
    for run in 0..file.run_count() {
        let held = file.run(run).unwrap();
        for ordinal in 0..held.ordinals {
            starts.push((run, ordinal, held.branch));
        }
    }
    let mut frames = Frames::new(&file, &WATCHED).unwrap();
    for (at, &(run, ordinal, branch)) in starts.iter().enumerate().rev().step_by(37) {
        frames.seek(run, ordinal).unwrap();
        let frame = frames.frame(branch).unwrap();
        let sampled = frame.tables.iter().map(SharedView::sampled_at).collect();
        assert!(same(&(frame, sampled), &expected[at]), "run {run} ordinal {ordinal} differs");
    }
}

#[test]
fn frames_moved_forward_by_days_go_on_and_are_the_ones_sought() {
    let (bytes, _) = fixtures::churned(recorder_derive::writers(), &[(1, 0, 7)]);
    let file = opened(bytes);
    let ordinals = file.run(0).unwrap().ordinals;
    let block = envelope::UNIT_ORDINALS;
    let restored = |table: TableId, at: u32| file.table_entry(0, at, table.index()).unwrap().unit.restore.is_some();
    let restores = |frames: &Frames<&Recording>| WATCHED.map(|table| frames.reader(table).map_or(0, TableReader::restores));
    let sampled = |frame: &DecodedFrame| frame.tables.iter().map(SharedView::sampled_at).collect::<Vec<_>>();
    let mut frames = Frames::new(&file, &WATCHED).unwrap();
    let mut crossed = [false; 2];
    let mut previous: Option<u32> = None;
    for ordinal in (5..ordinals).step_by(24) {
        let before = restores(&frames);
        frames.go_to(0, ordinal).unwrap();
        let frame = frames.frame(1).unwrap();
        let mut fresh = Frames::new(&file, &WATCHED).unwrap();
        fresh.seek(0, ordinal).unwrap();
        let sought = fresh.frame(1).unwrap();
        assert_eq!(frame.tables, sought.tables, "ordinal {ordinal}");
        assert_eq!(sampled(&frame), sampled(&sought), "ordinal {ordinal}");
        let Some(at) = previous else {
            previous = Some(ordinal);
            continue;
        };
        let after = restores(&frames);
        for (index, table) in WATCHED.into_iter().enumerate() {
            let across = (at / block..ordinal / block).any(|end| restored(table, end));
            if at / block != ordinal / block {
                crossed[usize::from(across)] = true;
            }
            assert_eq!(after[index] - before[index], u32::from(across), "{table:?} from ordinal {at} to {ordinal}");
        }
        previous = Some(ordinal);
    }
    assert_eq!(crossed, [true, true], "the walk crosses a block end with no restore and one with");
}

fn total(frames: &Frames<&Recording>) -> Spent {
    frames.spent().fold(Spent::default(), |sum, (_, spent)| Spent {
        patched: sum.patched + spent.patched,
        in_place: sum.in_place + spent.in_place,
        rebuilt: sum.rebuilt + spent.rebuilt,
        snapshots_patched: sum.snapshots_patched + spent.snapshots_patched,
        move_ns: sum.move_ns + spent.move_ns,
        view_ns: sum.view_ns + spent.view_ns,
        update_ns: sum.update_ns + spent.update_ns,
    })
}

fn three_ways(file: &Recording, tables: &[TableId], mut seen: impl FnMut(&Frames<&Recording>, bool)) -> [Spent; 3] {
    let mut rebuilt = Frames::new(file, tables).unwrap().rebuilding();
    let mut held = Frames::new(file, tables).unwrap();
    let mut dropped = Frames::new(file, tables).unwrap();
    let mut kept = None;
    for run in 0..file.run_count() {
        let at = file.run(run).unwrap();
        for ordinal in 0..at.ordinals {
            seen(&dropped, false);
            for frames in [&mut rebuilt, &mut held, &mut dropped] {
                frames.go_to(run, ordinal).unwrap();
                if ordinal % envelope::UNIT_ORDINALS == 0 {
                    frames.keyframe();
                }
            }
            seen(&dropped, true);
            let (want, want_facts) = rebuilt.frame_recorded(at.branch).unwrap();
            let (shared, shared_facts) = held.frame_recorded(at.branch).unwrap();
            let (alone, alone_facts) = dropped.frame_recorded(at.branch).unwrap();
            for (how, frame, facts) in [("held", &shared, &shared_facts), ("dropped", &alone, &alone_facts)] {
                assert_eq!(facts.keyframe, want_facts.keyframe, "run {run} ordinal {ordinal}, the hour before {how}: keyframe");
                for &table in tables {
                    let index = table.index();
                    assert_eq!(frame.tables[index], want.tables[index], "{table:?} at run {run} ordinal {ordinal}, the hour before {how}: the patched view is not the view built from every cell");
                    assert_eq!(frame.tables[index].sampled_at(), want.tables[index].sampled_at(), "{table:?} at run {run} ordinal {ordinal}, the hour before {how}: sample hour");
                    assert_eq!(facts.tables[index], want_facts.tables[index], "{table:?} at run {run} ordinal {ordinal}, the hour before {how}: update");
                }
            }
            kept = Some((shared, shared_facts));
        }
    }
    drop(kept);
    [total(&rebuilt), total(&held), total(&dropped)]
}

fn took_each_path([rebuilt, held, dropped]: [Spent; 3]) {
    assert_eq!(rebuilt.patched, 0, "the reference patched: {rebuilt:?}");
    assert!(held.patched > 0 && held.in_place == 0 && held.snapshots_patched == 0, "a frame held copies: {held:?}");
    assert!(dropped.patched > 0 && dropped.in_place == dropped.patched && dropped.snapshots_patched > 0, "a frame dropped is patched in place: {dropped:?}");
}

#[test]
fn a_view_patched_at_every_hour_is_the_view_built_from_every_cell_held_or_dropped() {
    let (bytes, _) = fixtures::churned(recorder_derive::writers(), &CHURNED);
    took_each_path(three_ways(&opened(bytes), &WATCHED, |_, _| {}));
}

#[test]
fn a_view_patched_through_inserts_deletes_packs_and_status_changes_is_the_view_built_from_every_cell() {
    let (bytes, _) = fixtures::edited(recorder_derive::writers(), 3);
    let countries = TableId::Countries;
    let (mut packs, mut taken, mut returned, mut statuses) = (0_u32, 0_u32, 0_u32, 0_u32);
    let (mut gone, mut status) = (Vec::new(), None);
    let spent = three_ways(&opened(bytes), &[countries], |frames, moved| {
        let Some(reader) = frames.reader(countries) else { return };
        if !moved {
            packs += u32::from(reader.packs_on_step());
            return;
        }
        let changes = reader.changes();
        taken += u32::from(changes.deleted.iter().any(|(slot, _)| changes.inserted.contains(slot)));
        let inserted: Vec<u128> = changes.inserted.iter().filter_map(|&slot| reader.cells().key(slot)).collect();
        returned += u32::from(inserted.iter().any(|key| gone.contains(key)));
        gone = changes.deleted.iter().map(|&(_, key)| key).collect();
        statuses += u32::from(status.is_some_and(|before| before != reader.status()));
        status = Some(reader.status());
    });
    assert!(packs > 0, "a block's end packs the table");
    assert!(taken > 0, "a row inserted takes the slot of a row deleted that hour");
    assert!(returned > 0, "a row deleted comes back the next hour");
    assert!(statuses > 0, "the table's status changes");
    took_each_path(spent);
}

fn keyed_edits<T: replay_data::rows::RowCodec>(now: &[T], before: &[T]) -> (Vec<u128>, Vec<u128>) {
    let at = |rows: &[T], key: u128| rows.binary_search_by_key(&key, T::key).ok();
    let changed = now.iter().filter(|row| at(before, row.key()).is_none_or(|old| before[old] != **row)).map(T::key).collect();
    let deleted = before.iter().map(T::key).filter(|&key| at(now, key).is_none()).collect();
    (changed, deleted)
}

fn watched_edits(now: &TableRows, before: &TableRows) -> (Vec<u128>, Vec<u128>) {
    match (now, before) {
        (TableRows::Countries(now), TableRows::Countries(before)) => keyed_edits(now, before),
        (TableRows::WorldTension(now), TableRows::WorldTension(before)) => keyed_edits(now, before),
        _ => panic!("not a watched table's rows"),
    }
}

#[test]
fn an_hour_stepped_to_names_what_it_changed_and_no_other_move_does() {
    use replay_frame::frame::{ChangeDiscontinuity, HourInput, TableChanges};
    let runs = [(1, 0, 3), (2, 50_000, 1)];
    let (bytes, seen) = fixtures::churned(recorder_derive::writers(), &runs);
    let file = opened(bytes);
    let countries = TableId::Countries;
    let unwritten = 1_u64 << (0..).find(|&field| countries.field_name(field) == Some("political_power_raw")).unwrap();
    let mut frames = Frames::new(&file, &WATCHED).unwrap().with_change_facts();
    let mut previous: Option<DecodedFrame> = None;
    let (mut at, mut named, mut rebuilt, mut membership, mut quiet) = (0, [0_u32; 2], [0_u32; 2], 0, 0);
    for run in 0..file.run_count() {
        let held = file.run(run).unwrap();
        for ordinal in 0..held.ordinals {
            frames.go_to(run, ordinal).unwrap();
            let frame = frames.frame(held.branch).unwrap();
            for (index, table) in WATCHED.into_iter().enumerate() {
                let Some(before) = &previous else { continue };
                let (old, new) = (&before.tables[table.index()], &frame.tables[table.index()]);
                let identity = old.identity();
                let since = HourInput::new(&frame).changes().since(table, Some(&identity), u64::MAX);
                if SharedView::ptr_eq(old, new) {
                    assert!(matches!(since, TableChanges::Unchanged), "{table:?} at run {run} ordinal {ordinal}: a view kept is unchanged");
                    continue;
                }
                let complete = matches!((&**old, &**new), (TableView::Complete(_), TableView::Complete(_)));
                if ordinal == 0 || !complete {
                    assert!(new.changes_since(&identity).is_none(), "{table:?} at run {run} ordinal {ordinal} names changes");
                    assert!(matches!(since, TableChanges::Discontinuity(_)), "{table:?} at run {run} ordinal {ordinal}: {since:?}");
                    continue;
                }
                let facts = new.changes_since(&identity).unwrap_or_else(|| panic!("{table:?} at run {run} ordinal {ordinal} names no changes"));
                let (TableView::Complete(written), TableView::Complete(written_before)) = (&*seen[at].0.tables[table.index()], &*seen[at - 1].0.tables[table.index()]) else {
                    panic!("the writer's {table:?} at hour {at} is not complete where the read's is");
                };
                let (changed, deleted) = watched_edits(written, written_before);
                assert_eq!(facts.changed().iter().map(|row| row.key).collect::<Vec<_>>(), changed, "{table:?} at run {run} ordinal {ordinal}: changed keys");
                assert_eq!(facts.deleted(), deleted.as_slice(), "{table:?} at run {run} ordinal {ordinal}: deleted keys");
                if changed.is_empty() && deleted.is_empty() {
                    assert!(matches!(since, TableChanges::Unchanged), "{table:?} at run {run} ordinal {ordinal}: an hour that wrote no row is unchanged: {since:?}");
                    rebuilt[index] += 1;
                    continue;
                }
                assert!(matches!(since, TableChanges::Rows(rows) if std::ptr::eq(rows, facts)), "{table:?} at run {run} ordinal {ordinal}: {since:?}");
                for row in facts.changed() {
                    assert_eq!(row.inserted, !written_before.contains_key(row.key), "{table:?} at run {run} ordinal {ordinal}: key {}", row.key);
                    assert_ne!(row.fields, 0, "{table:?} at run {run} ordinal {ordinal}: key {} names no field", row.key);
                }
                named[index] += 1;
                if table == countries {
                    let moved = facts.changed().iter().any(|row| row.inserted) || !facts.deleted().is_empty();
                    let unwritten_since = HourInput::new(&frame).changes().since(countries, Some(&identity), unwritten);
                    if moved {
                        membership += 1;
                        assert!(matches!(unwritten_since, TableChanges::Rows(_)), "ordinal {ordinal}: rows came or went");
                    } else {
                        quiet += 1;
                        assert!(matches!(unwritten_since, TableChanges::Unchanged), "ordinal {ordinal}: no hour writes political power");
                    }
                }
            }
            previous = Some(frame);
            at += 1;
        }
    }
    assert_eq!(at, seen.len());
    let stepped = (seen.len() - runs.len()) as u32;
    assert_eq!(named[0], stepped, "every hour stepped to changes a country");
    assert!(rebuilt[1] > 0, "world tension rebuilt for its sample hour alone, its row unwritten, is unchanged");
    assert!(membership > 0 && quiet > 0, "hours with and without rows coming and going");

    fn since<'a>(frame: &'a DecodedFrame, before: &DecodedFrame) -> TableChanges<'a> {
        let countries = TableId::Countries;
        HourInput::new(frame).changes().since(countries, Some(&before.tables[countries.index()].identity()), u64::MAX)
    }
    let discontinuous = |change: TableChanges<'_>| matches!(change, TableChanges::Discontinuity(ChangeDiscontinuity::SnapshotSeekOrSkippedEdit));
    let mut frames = Frames::new(&file, &WATCHED).unwrap().with_change_facts();
    frames.go_to(0, 40).unwrap();
    let built = frames.frame(1).unwrap();
    frames.seek(0, 39).unwrap();
    assert!(discontinuous(since(&frames.frame(1).unwrap(), &built)), "a seek back");
    frames.go_to(0, 50).unwrap();
    let built = frames.frame(1).unwrap();
    frames.go_to(0, 52).unwrap();
    assert!(discontinuous(since(&frames.frame(1).unwrap(), &built)), "a move of two hours");
    frames.go_to(0, 60).unwrap();
    let built = frames.frame(1).unwrap();
    assert!(frames.step().unwrap() && frames.step().unwrap());
    assert!(discontinuous(since(&frames.frame(1).unwrap(), &built)), "two steps, no frame between");
    frames.go_to(0, 3 * envelope::UNIT_ORDINALS - 1).unwrap();
    let built = frames.frame(1).unwrap();
    frames.go_to(1, 0).unwrap();
    assert!(discontinuous(since(&frames.frame(2).unwrap(), &built)), "a run's first hour");
    frames.go_to(0, 70).unwrap();
    let built = frames.frame(1).unwrap();
    frames.go_to(0, 71).unwrap();
    assert!(matches!(since(&frames.frame(1).unwrap(), &built), TableChanges::Rows(_)), "the next hour after a seek");

    let mut plain = Frames::new(&file, &WATCHED).unwrap();
    plain.go_to(0, 10).unwrap();
    let built = plain.frame(1).unwrap();
    plain.go_to(0, 11).unwrap();
    let next = plain.frame(1).unwrap();
    assert!(next.tables[countries.index()].changes_since(&built.tables[countries.index()].identity()).is_none(), "frames without change facts name none");
    assert!(discontinuous(since(&next, &built)));
}

#[test]
fn strings_that_return_on_later_blocks_are_read_after_a_seek() {
    let (bytes, _) = fixtures::churned(recorder_derive::writers(), &[(1, 0, 6)]);
    let file = opened(bytes);
    let mut frames = Frames::new(&file, &WATCHED).unwrap();
    let mut tags = |hour: u32| {
        frames.seek(0, hour).unwrap();
        let frame = frames.frame(1).unwrap();
        let TableView::Complete(TableRows::Countries(rows)) = &*frame.tables[TableId::Countries.index()] else {
            panic!("hour {hour}'s countries are not complete");
        };
        rows.iter().map(|row| (row.id, String::from_utf8(row.tag.clone()).unwrap())).collect::<Vec<_>>()
    };
    let named = |rows: &[(u32, &str)]| rows.iter().map(|&(id, tag)| (id, tag.to_owned())).collect::<Vec<_>>();
    assert_eq!(tags(800), named(&[(1, "old string"), (2, "new string 800"), (3, "cycle 16"), (4, "returning")]));
    assert_eq!(tags(2000), named(&[(1, "old string"), (2, "new string 2000"), (3, "cycle 0"), (4, "returning")]));
    assert_eq!(tags(4319), named(&[(1, "old string"), (2, "new string 4319"), (3, "cycle 6")]));
    let tag = dashboard_declare::field(TableId::Countries, "tag").unwrap();
    for (ordinal, expected) in [(2000, named(&[(1, "old string"), (2, "new string 2000"), (3, "cycle 0"), (4, "returning")])), (4319, named(&[(1, "old string"), (2, "new string 4319"), (3, "cycle 6")]))] {
        let (read, counts) = crate::cells::count_reads(|| {
            let mut reader = crate::cells::TableReader::new(&file, 0, TableId::Countries, None).unwrap();
            reader.seek(&file, ordinal).unwrap();
            let view = reader.view();
            let mut read: Vec<(u32, String)> = view.rows().filter_map(|slot| Some((view.key(slot)? as u32, String::from_utf8(view.string(view.text(tag, slot)?)?.to_vec()).unwrap()))).collect();
            read.sort();
            read
        });
        assert_eq!(read, expected, "a reader sought to {ordinal} names every row's text from the restore's own string table");
        let stats = counts[&TableId::Countries];
        assert!(stats.restores == 1 && stats.pinned == 0, "a restore keeps its strings where they were read instead of copying each one: {stats:?}");
    }
    let mut frames = Frames::new(&file, &WATCHED).unwrap();
    frames.seek(0, 2100).unwrap();
    assert_eq!(*frames.frame(1).unwrap().tables[TableId::WorldTension.index()], TableView::Unavailable { reason: "refused 0".into() });
}

#[test]
fn a_damaged_part_refuses_its_table_by_name_and_no_other() {
    let (bytes, expected) = fixtures::churned(recorder_derive::writers(), &[(1, 0, 4)]);
    let clean = opened(bytes.clone());
    let entry = clean.table_entry(0, 1, TableId::Countries.index()).unwrap();
    let frame = &entry.unit.parts[0].frames[0];
    let mut damaged = bytes;
    damaged[(frame.offset + u64::from(frame.stored) / 2) as usize] ^= 0x40;
    let file = opened(damaged);
    let read = read_on(&file, &WATCHED);
    let block = envelope::UNIT_ORDINALS as usize;
    for (at, (read, written)) in read.iter().zip(&expected).enumerate() {
        let tension = TableId::WorldTension.index();
        assert_eq!(read.0.tables[tension], written.0.tables[tension], "hour {at}'s world tension differs");
        let countries = &*read.0.tables[TableId::Countries.index()];
        if at < block {
            assert_eq!(countries, &*written.0.tables[TableId::Countries.index()], "hour {at}");
        } else {
            let TableView::Unavailable { reason } = countries else { panic!("hour {at}'s countries read past their damage") };
            assert!(reason.contains("Countries of run 0 in block 1"), "{reason}");
        }
    }
    let mut frames = Frames::new(&file, &WATCHED).unwrap();
    frames.seek(0, 300).unwrap();
    assert_eq!(frames.frame(1).unwrap().tables[TableId::Countries.index()], expected[300].0.tables[TableId::Countries.index()]);
    frames.seek(0, 2 * block as u32 + 5).unwrap();
    let frame = frames.frame(1).unwrap();
    assert!(clean.table_entry(0, 1, TableId::Countries.index()).unwrap().unit.restore.is_none(), "the fixture restores no table at block 1's end");
    let TableView::Unavailable { reason } = &*frame.tables[TableId::Countries.index()] else { panic!("a seek past the damage read it") };
    assert!(reason.contains("Countries of run 0 in block 1"), "{reason}");
    assert_eq!(frame.tables[TableId::WorldTension.index()], expected[2 * block + 5].0.tables[TableId::WorldTension.index()]);
    frames.seek(0, 300).unwrap();
    assert_eq!(frames.frame(1).unwrap().tables[TableId::Countries.index()], expected[300].0.tables[TableId::Countries.index()], "a seek back before the damage reads it again");

    rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(|| {
        let mut parked = Frames::new(&file, &WATCHED).unwrap();
        let (first, bytes) = parked.frame_with_parked_readers(1, 0, 299, 1024 * 1024).unwrap();
        assert!(WATCHED.iter().all(|&table| parked.reader(table).is_some()));
        let mut constrained = Frames::new(&file, &WATCHED).unwrap();
        let (same_hour, _) = constrained.frame_with_parked_readers(1, 0, 299, bytes.unwrap() - 1).unwrap();
        assert_eq!(first.tables, same_hour.tables);
        assert_eq!(WATCHED.iter().filter(|&&table| constrained.reader(table).is_some()).count(), 1);
        let target = block as u32;
        let (resumed, reads) = crate::cells::count_reads(|| {
            parked.frame_with_parked_readers(1, 0, target, 1024 * 1024).unwrap().0
        });
        let mut exact = Frames::new(&file, &WATCHED).unwrap();
        exact.seek(0, target).unwrap();
        assert_eq!(resumed.tables, exact.frame(1).unwrap().tables);
        assert_eq!(reads[&TableId::Countries].opened, 1);
    });
}

#[test]
fn torn_tails_choose_the_last_committed_root_for_both_inputs() {
    let runs = [(1, 0, 10)];
    let (mut bytes, _) = recording(recorder_derive::writers(), &runs, every(&runs), |_| {});
    let original = bytes.len();
    append_revision(&mut bytes, 2);
    let committed = bytes.len();
    append_revision(&mut bytes, 3);
    bytes.truncate(bytes.len() - 13);
    let disk = Saved::new(&bytes);
    for reader in [opened(bytes.clone()), disk.open()] {
        assert_eq!(reader.len(), committed as u64);
        assert_eq!(reader.hours(), 10);
    }
    let (root, _) = envelope::last_root(&bytes).unwrap();
    bytes[root.offset as usize] ^= 1;
    assert_eq!(opened(bytes).len(), original as u64);
}

#[test]
fn an_open_recording_keeps_its_revision_while_its_file_grows() {
    let runs = [(1, 0, 10)];
    let (mut bytes, _) = recording(recorder_derive::writers(), &runs, every(&runs), |_| {});
    let disk = Saved::new(&bytes);
    let pinned = disk.open();
    let before = bytes.len();
    append_revision(&mut bytes, 2);
    disk.append(&bytes[before..]);
    assert_eq!(pinned.len(), before as u64);
    assert_eq!(read_on(&pinned, &[TableId::Countries]).len(), 10);
    assert_eq!(disk.open().len(), bytes.len() as u64);
}

#[test]
fn root_and_run_lists_hold_many_short_runs() {
    for count in [4, 256] {
        let runs: Vec<_> = (0..count).map(|run| (run as u64, 0, 1)).collect();
        let file = opened(recording(recorder_derive::writers(), &runs, every(&runs), |_| {}).0);
        assert_eq!(file.hours(), count as u64);
        assert_eq!(file.run_count(), count as u32);
        assert_eq!(file.segments().len(), count);
    }
}

#[test]
fn format_136_columns_are_this_builds_but_the_appended() {
    use replay_data::rows::compatibility::AIR_HISTORY_SLOTS_FORMAT;
    let schema = schema().unwrap();
    let columns = columns_at(AIR_HISTORY_SLOTS_FORMAT).unwrap();
    let mut changed = [TableId::DivisionMovement, TableId::DivisionPanelStats, TableId::DivisionStatus, TableId::LandCombatDivisions, TableId::LandCombatDivisionModifiers, TableId::LandUnitOrder, TableId::StrategicRegionWeights, TableId::StateBuildingInputs, TableId::PowerSpendDays, TableId::CombatEngagementRefreshes, TableId::LandCombatEdges, TableId::FuelAwardRecipients, TableId::ScriptVariableNames, TableId::ScriptVariableValues, TableId::LandCombatResultObservers, TableId::LandDivisionDamage, TableId::DivisionKills, TableId::PowerSpendPayments, TableId::PowerSpendGains, TableId::VariantStats, TableId::VariantChanges, TableId::RaidSpeedInputs, TableId::RocketProductionOutput].map(TableId::index).to_vec();
    changed.sort_unstable();
    let named = |format: u16| {
        withheld_at(format)
            .unwrap()
            .into_iter()
            .map(|(table, reason)| {
                assert!(reason.contains(&format!("{:?}", TableId::ALL[table])), "{reason}");
                table
            })
            .collect::<Vec<usize>>()
    };
    assert_eq!(named(AIR_HISTORY_SLOTS_FORMAT), changed);
    use replay_data::rows::compatibility::EXACT_NAVAL_CONDITION_FORMAT;
    let mut expected = changed.clone();
    expected.extend([TableId::AirNavalPatrolHistories, TableId::AirMissionInputs].map(TableId::index));
    expected.sort_unstable();
    assert_eq!(named(EXACT_NAVAL_CONDITION_FORMAT), expected);
    let earlier = columns_at(EXACT_NAVAL_CONDITION_FORMAT).unwrap();
    assert_eq!(earlier[TableId::AirNavalPatrolHistories.index()], 3);
    assert_eq!(earlier[TableId::Cryptology.index()], 4);
    assert_eq!(earlier[TableId::RaidAirScores.index()], 6);
    for held in [&earlier, &columns] {
        assert_eq!(held.len(), TABLE_COUNT - 14);
        assert_eq!(replay_columnar::envelope::schema_digest_prefix(&schema[..held.len()], held), None);
    }
    let current = columns_at(replay_data::FORMAT_VERSION).unwrap();
    assert_eq!(
        replay_columnar::envelope::schema_digest_prefix(&schema, &current),
        Some(replay_columnar::envelope::schema_digest(&schema))
    );
    assert_ne!(
        replay_columnar::envelope::schema_digest_prefix(&schema, &columns),
        Some(replay_columnar::envelope::schema_digest(&schema))
    );
}

fn naming_schema(mut bytes: Vec<u8>, theirs: [u8; 32]) -> Vec<u8> {
    let (extent, _) = envelope::last_root(&bytes).unwrap();
    let raw = zstd::bulk::decompress(extent.stored_in(&bytes).unwrap(), 1 << 20).unwrap();
    let mut root = Root::read(&raw).unwrap();
    root.identity.schema = theirs;
    let root = fixtures::append(&mut bytes, Kind::Root, (0, 0, 0, 0), &root.write().unwrap());
    bytes.extend_from_slice(&envelope::footer(root.offset, root.stored, &root.digest));
    bytes
}

fn refused_naming_both(bytes: Vec<u8>, theirs: &[u8; 32]) {
    let hex = |digest: &[u8; 32]| digest[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let error = Recording::open(bytes).unwrap_err().to_string();
    let ours = hex(&envelope::schema_digest(&schema().unwrap()));
    assert!(error.contains(&format!("This recording was written with another schema (its digest {}, this build's {ours})", hex(theirs))), "{error}");
}

#[test]
fn another_schema_is_refused_naming_both_digests() {
    let mut before_logs = schema().unwrap();
    before_logs.insert(TableId::DivisionModelInputs.index() + 1, vec![Column::I32]);
    for theirs in [[0xab; 32], envelope::schema_digest(&before_logs)] {
        let runs = [(1, 0, 3)];
        let (bytes, _) = recording(recorder_derive::writers(), &runs, every(&runs), |_| {});
        refused_naming_both(naming_schema(bytes, theirs), &theirs);
    }
}

#[test]
fn a_recording_of_an_earlier_format_opens_and_reads_what_it_held() {
    use replay_journal::synthetic::{Input, Sample};
    use replay_data::rows::{CountryRow, DivisionRow, DivisionRowAt141, Recorded, StateRow, StateRowAt141};
    use replay_data::rows::compatibility::CAPTURE_ADDITIONS_FORMAT;
    let format = CAPTURE_ADDITIONS_FORMAT - 1;
    let added: Vec<TableId> = TableId::ALL.into_iter().filter(|table| table.last_change().introduced > format).collect();
    assert_eq!(added, [TableId::LandUnitOrder, TableId::StrategicRegionWeights, TableId::StateBuildingInputs, TableId::PowerSpendDays, TableId::CombatEngagementRefreshes, TableId::LandCombatEdges, TableId::FuelAwardRecipients, TableId::ScriptVariableNames, TableId::ScriptVariableValues, TableId::LandCombatResultObservers, TableId::LandDivisionDamage, TableId::DivisionKills, TableId::PowerSpendPayments, TableId::PowerSpendGains]);
    assert!(LAST_COMPLETE.contains(&TableId::Divisions));
    let states = [StateRowAt141 { id: 4, owner: 7, controller: 9, custom_name: b"Bohemia".to_vec(), continent: Some(2).into(), ..Default::default() }];
    let countries = [CountryRow { id: 7, tag: b"GER".to_vec(), stability_raw: 5, ..Default::default() }];
    let divisions = [
        DivisionRowAt141 { id: (3_u64 << 32) | 1, roster_owner: 7, province: 11, hit_points: 500_000, organisation: 250_000, ..Default::default() },
        DivisionRowAt141 { id: (4_u64 << 32) | 1, roster_owner: 7, province: 12, ..Default::default() },
    ];
    let partial = |ordinal: u32| ordinal % envelope::UNIT_ORDINALS >= envelope::UNIT_ORDINALS - 10;
    let runs = [(1, 100, envelope::SPAN_BLOCKS + 1)];
    let (bytes, _) = fixtures::written_at(recorder_derive::writers_within, format, b"format 141", &runs, &[TableId::States, TableId::Countries], |synthetic, _, ordinal| {
        synthetic.table(TableId::States, Input::Snapshot { rows: &states, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        synthetic.table(TableId::Countries, Input::Snapshot { rows: &countries, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
        let input = match partial(ordinal) {
            true => Input::Snapshot { rows: &divisions[..1], partial: Some(1), read: true, sample: Sample::FromReadBit },
            false => Input::Snapshot { rows: &divisions, partial: None, read: true, sample: Sample::FromReadBit },
        };
        synthetic.table(TableId::Divisions, input).unwrap();
    });
    let recording = opened(bytes.clone());
    assert_eq!(recording.tables(), TABLE_COUNT - added.len());
    assert_eq!(recording.columns().len(), TABLE_COUNT);
    let state_columns = TableId::States.last_change().fields(TableId::States, format);
    assert_eq!(recording.columns()[TableId::States.index()], state_columns);
    assert_eq!(TableId::States.field_name(state_columns), Some("marked_owners"));
    assert_eq!(state_columns, TableId::States.field_count() - 1);
    let mode = replay_data::scope::CaptureMode::from_byte(recording.root_identity().capture_mode).unwrap();
    let block = envelope::UNIT_ORDINALS;
    for table in &added {
        assert_eq!(recording.columns()[table.index()], 0);
        let gap = table.last_change().table_gap(*table, format).unwrap();
        assert_eq!(recording.predates(table.index()), Some(gap.as_str()));
        assert_eq!(recording.withheld_reason(table.index()), Some(gap.as_str()));
        for error in [recording.table_entry(0, 0, table.index()).map(|_| ()).unwrap_err(), recording.span_summaries(0, 0, table.index()).map(|_| ()).unwrap_err()] {
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(&gap), "{error}");
        }
        let mut reader = crate::cells::TableReader::new(&recording, 0, *table, Some(&[0])).unwrap();
        reader.keep_tracks();
        let not_recorded = |reader: &crate::cells::TableReader, at: &str| {
            assert_eq!(reader.status(), Status::NotRecorded, "{table:?} {at}");
            assert!(!reader.was_read() && reader.sampled_at().is_none() && reader.cells().rows().next().is_none(), "{table:?} {at}");
        };
        reader.seek(&recording, block + 3).unwrap();
        not_recorded(&reader, "sought");
        assert!(reader.step(&recording).unwrap());
        not_recorded(&reader, "stepped");
        assert!(reader.changes().quiet() && !reader.changes().status_changed, "{table:?}: a step changes nothing");
        reader.go_to(&recording, 7 * block + 1).unwrap();
        not_recorded(&reader, "advanced");
        let stretch = reader.read_stretch(&recording, 7 * block + 9).unwrap();
        assert_eq!(stretch.statuses, vec![Status::NotRecorded; 8], "{table:?}");
        assert!(stretch.events.is_empty() && stretch.start.is_empty(), "{table:?}");
        assert_eq!(reader.hour(), Some(runs[0].1 + 7 * block as i32 + 9));
    }
    assert_eq!(recording.columns()[TableId::Divisions.index()], TableId::Divisions.field_count() - 1);
    let mut tables = vec![TableId::States, TableId::Countries, TableId::Divisions];
    tables.extend(&added);
    let read = read_on(&recording, &tables);
    assert_eq!(read.len(), runs[0].2 as usize * envelope::UNIT_ORDINALS as usize);
    let state = StateRow {
        id: 4,
        owner: 7,
        controller: 9,
        custom_name: b"Bohemia".to_vec(),
        continent: Some(2).into(),
        marked_owners: Recorded::NotRecorded,
        ..Default::default()
    };
    let division = |at: usize| {
        let held = &divisions[at];
        DivisionRow { id: held.id, roster_owner: held.roster_owner, province: held.province, hit_points: held.hit_points, organisation: held.organisation, theatre: Recorded::NotRecorded, ..Default::default() }
    };
    for (ordinal, (frame, _)) in (0..).zip(&read) {
        assert_eq!(*frame.tables[TableId::States.index()], TableView::Complete(TableRows::States(vec![state.clone()])), "hour {ordinal}");
        assert_eq!(*frame.tables[TableId::Countries.index()], TableView::Complete(TableRows::Countries(countries.to_vec())), "hour {ordinal}");
        let expected = match partial(ordinal) {
            true => TableView::Truncated { rows: TableRows::Divisions(vec![division(0)]), missing: 1 },
            false => TableView::Complete(TableRows::Divisions(vec![division(0), division(1)])),
        };
        assert_eq!(*frame.tables[TableId::Divisions.index()], expected, "hour {ordinal}");
        for table in &added {
            assert_eq!(*frame.tables[table.index()], TableView::NotRecorded { mode }, "hour {ordinal}: {table:?}");
        }
    }
    for ordinal in [block, 6 * block + 3, 12 * block + 1, envelope::SPAN_BLOCKS * block + 2] {
        let mut frames = Frames::new(&recording, &tables).unwrap();
        frames.seek(0, ordinal).unwrap();
        let sought = frames.frame(runs[0].0).unwrap();
        assert_eq!(sought.tables, read[ordinal as usize].0.tables, "sought to {ordinal}");
    }
    let schema = schema().unwrap();
    let mut columns = columns_at(format).unwrap();
    let every_table: Vec<usize> = columns.iter().copied().chain(added.iter().map(|_| 0)).collect();
    columns[TableId::Countries.index()] -= 1;
    let narrower = envelope::schema_digest_prefix(&schema[..columns.len()], &columns).unwrap();
    let padded = envelope::schema_digest_prefix(&schema, &every_table).unwrap();
    for theirs in [narrower, padded] {
        refused_naming_both(naming_schema(bytes.clone(), theirs), &theirs);
    }
}

#[test]
fn a_log_reads_at_an_hour_as_the_rows_it_lists() {
    use replay_journal::synthetic::{Input, Sample};
    use replay_data::rows::NuclearStrikeRow;
    const PROVINCE: usize = 3;
    let listed = |ordinal: u32| -> Vec<(u128, i64)> {
        (0..480_u32).filter(|&id| 3 * id <= ordinal && ordinal < 3 * id + id % 40 + 1).map(|id| (u128::from(id), i64::from(id * 7))).collect()
    };
    let (bytes, _) = fixtures::written(recorder_derive::writers(), b"log at an hour", &[(1, 0, 2)], &[], |synthetic, _, ordinal| {
        let rows: Vec<NuclearStrikeRow> = listed(ordinal)
            .into_iter()
            .map(|(id, province)| NuclearStrikeRow { id: id as u64, hour: id as i32 * 3, province: province as u32, nuke_type: id as u32 % 3, ..Default::default() })
            .collect();
        synthetic.table(TableId::NuclearStrikes, Input::Snapshot { rows: &rows, partial: None, read: true, sample: Sample::FromReadBit }).unwrap();
    });
    let recording = opened(bytes);
    let mut reader = crate::cells::TableReader::new(&recording, 0, TableId::NuclearStrikes, None).unwrap();
    let held = |reader: &crate::cells::TableReader| -> Vec<(u128, i64)> {
        let view = reader.view();
        let mut rows: Vec<(u128, i64)> = view.rows().map(|slot| (view.key(slot).unwrap(), view.number(PROVINCE, slot).unwrap())).collect();
        rows.sort_unstable();
        rows
    };
    for ordinal in [0, 5, 719, 720, 1000, 1439] {
        reader.seek(&recording, ordinal).unwrap();
        assert_eq!(held(&reader), listed(ordinal), "sought to {ordinal}");
        assert!(matches!(reader.status(), replay_columnar::manifest::Status::Complete));
    }
    reader.seek(&recording, 700).unwrap();
    for ordinal in 701..760 {
        let before: Vec<u128> = held(&reader).into_iter().map(|(key, _)| key).collect();
        assert!(reader.step(&recording).unwrap());
        let after: Vec<u128> = held(&reader).into_iter().map(|(key, _)| key).collect();
        assert_eq!(held(&reader), listed(ordinal), "stepped to {ordinal}");
        let view = reader.view();
        let mut inserted: Vec<u128> = reader.changes().inserted.iter().map(|&slot| view.key(slot).unwrap()).collect();
        inserted.sort_unstable();
        let mut deleted: Vec<u128> = reader.changes().deleted.iter().map(|&(_, key)| key).collect();
        deleted.sort_unstable();
        assert_eq!(inserted, after.iter().copied().filter(|key| !before.contains(key)).collect::<Vec<_>>(), "appended at {ordinal}");
        assert_eq!(deleted, before.iter().copied().filter(|key| !after.contains(key)).collect::<Vec<_>>(), "forgotten at {ordinal}");
    }
    reader.go_to(&recording, 1300).unwrap();
    assert_eq!(held(&reader), listed(1300), "advanced to 1300");
    let mut tracked = crate::cells::TableReader::new(&recording, 0, TableId::NuclearStrikes, None).unwrap();
    tracked.keep_tracks();
    tracked.seek(&recording, 10).unwrap();
    let refused = tracked.read_stretch(&recording, 20).unwrap_err().to_string();
    assert!(refused.contains("NuclearStrikes is a log"), "{refused}");
}

#[test]
fn a_log_read_in_part_lists_its_rows() {
    use replay_frame::frame::{FrameInput, Metrics, TableInput, TableView};
    use replay_data::rows::{NuclearStrikeRow, TABLE_COUNT};
    const PROVINCE: usize = 3;
    let strike = |id: u64| NuclearStrikeRow { id, hour: id as i32, province: id as u32 * 7, ..Default::default() };
    let bytes = fixtures::recorded(recorder_derive::writers(), b"log read in part", 3, |ordinal| {
        let mut tables: [TableInput; TABLE_COUNT] = std::array::from_fn(|_| TableInput::Sampled(TableView::Unavailable { reason: "outside the fixture".into() }));
        tables[TableId::NuclearStrikes.index()] = TableInput::Sampled(match ordinal {
            0 => TableView::Complete(TableRows::NuclearStrikes(vec![strike(1)])),
            1 => TableView::Truncated { rows: TableRows::NuclearStrikes(vec![strike(2)]), missing: 1 },
            _ => TableView::Complete(TableRows::NuclearStrikes(vec![strike(2)])),
        });
        FrameInput { branch: 1, hour: 10 + ordinal as i32, tables, metrics: Metrics::default() }
    });
    let recording = opened(bytes);
    let read = |reader: &crate::cells::TableReader| -> Vec<i64> {
        let view = reader.view();
        let mut provinces: Vec<i64> = reader.read_rows().unwrap().iter().map(|&slot| view.number(PROVINCE, slot).unwrap()).collect();
        provinces.sort_unstable();
        provinces
    };
    let mut sought = crate::cells::TableReader::new(&recording, 0, TableId::NuclearStrikes, None).unwrap();
    sought.seek(&recording, 1).unwrap();
    assert!(matches!(sought.status(), replay_columnar::manifest::Status::Truncated { missing: 1 }));
    assert_eq!(read(&sought), [7, 14]);
    let mut stepped = crate::cells::TableReader::new(&recording, 0, TableId::NuclearStrikes, None).unwrap();
    stepped.seek(&recording, 0).unwrap();
    assert!(stepped.read_rows().is_none());
    assert!(stepped.step(&recording).unwrap());
    assert_eq!(read(&stepped), [7, 14]);
    assert!(stepped.step(&recording).unwrap());
    assert!(stepped.read_rows().is_none());
}

#[test]
fn a_pool_let_go_last_on_its_own_thread_ends() {
    use std::sync::mpsc;
    let pool = Arc::new(Pool::new(2, "pool self drop", || ()).unwrap());
    let (release, released) = mpsc::channel::<()>();
    let (ended, end) = mpsc::channel();
    let held = Arc::clone(&pool);
    pool.install(move || {
        rayon::spawn(move || {
            released.recv().unwrap();
            drop(held);
            ended.send(()).unwrap();
        })
    });
    drop(pool);
    release.send(()).unwrap();
    end.recv().unwrap();
}

#[test]
fn file_opens_share_one_page_owner_and_keep_distinct_live_pages() {
    let (bytes, _) = fixtures::churned(recorder_derive::writers(), &[(1, 100, envelope::SPAN_BLOCKS + 1), (2, 20_000, 1)]);
    let disk = Saved::new(&bytes);
    let first = disk.open();
    let second = disk.open();
    let selected = second.selecting(&TableId::ALL).unwrap();
    assert_eq!(first.pages_id(), second.pages_id(), "two opens of one file share one page owner");
    assert_eq!(first.pages_id(), selected.pages_id(), "selecting shares the same page owner");
    assert_eq!(first.live_page_counts(), crate::columnar_frames::PageCounts::default());

    let closed_a = first.table_entry(0, 0, TableId::Countries.index()).unwrap();
    let closed_b = second.table_entry(0, 5, TableId::Countries.index()).unwrap();
    let closed_c = selected.table_entry(0, 10, TableId::Countries.index()).unwrap();
    assert_eq!(first.live_page_counts().spans, 1, "one span directory loaded across all openers");
    assert_eq!(first.live_page_counts().span_rows, 1, "one span row shared across all three handles");

    let open_a = first.table_entry(0, envelope::SPAN_BLOCKS, TableId::Countries.index()).unwrap();
    let open_b = second.table_entry(0, envelope::SPAN_BLOCKS, TableId::WorldTension.index()).unwrap();
    let open_c = selected.table_entry(0, envelope::SPAN_BLOCKS, TableId::Countries.index()).unwrap();
    assert_eq!(first.live_page_counts().open_blocks, 1, "one live open block shared across all three handles");

    let open_r1 = first.table_entry(1, 0, TableId::Countries.index()).unwrap();
    assert_eq!(first.live_page_counts().open_blocks, 2, "two distinct open blocks held across openers");
    drop((closed_a, closed_b, closed_c, open_a, open_b, open_c, open_r1));

    let clean = opened(bytes.clone());
    let mut damaged_bytes = bytes;
    let extent = clean.run(1).unwrap().directory().open[0];
    let byte = (extent.offset + extent.stored / 2) as usize;
    damaged_bytes[byte] ^= 0xff;
    let damaged = opened(damaged_bytes);
    assert_ne!(clean.pages_id(), damaged.pages_id(), "in-memory opens do not share a page owner");
    let _ok = clean.table_entry(1, 0, TableId::Countries.index()).unwrap();
    let error = damaged.table_entry(1, 0, TableId::Countries.index()).unwrap_err();
    assert_eq!(Damage::of(&error).unwrap().scope, DamageScope::Extent(extent));
}

#[test]
fn plain_frame_builds_no_recorded_update_copy_and_alternates_cleanly_with_frame_recorded() {
    let (bytes, _) = fixtures::churned(recorder_derive::writers(), &[(1, 100, 2)]);
    let file = opened(bytes);
    let assert_same_frame = |got: &DecodedFrame, want: &DecodedFrame, context: &str| {
        assert_eq!(got.tables, want.tables, "{context}: tables");
        let got_sampled: Vec<_> = got.tables.iter().map(SharedView::sampled_at).collect();
        let want_sampled: Vec<_> = want.tables.iter().map(SharedView::sampled_at).collect();
        assert_eq!(got_sampled, want_sampled, "{context}: sampled_at");
    };
    let assert_same_recorded = |got: &RecordedFrame, want: &RecordedFrame, context: &str| {
        assert_eq!(got.keyframe, want.keyframe, "{context}: keyframe");
        assert_eq!(
            got.tables.iter().collect::<Vec<_>>(),
            want.tables.iter().collect::<Vec<_>>(),
            "{context}: recorded updates",
        );
    };
    let mut plain = Frames::new(&file, &WATCHED).unwrap();
    let mut recorded = Frames::new(&file, &WATCHED).unwrap();
    for ordinal in 0..10 {
        plain.go_to(0, ordinal).unwrap();
        recorded.go_to(0, ordinal).unwrap();
        let got = plain.frame(1).unwrap();
        let (want, want_recorded) = recorded.frame_recorded(1).unwrap();
        assert_same_frame(&got, &want, &format!("ordinal {ordinal}"));
        assert!(plain.slots.iter().all(|slot| slot.update.is_none()), "ordinal {ordinal}: plain frame retains no update");
        if ordinal == 0 {
            let bytes: usize = want_recorded.tables.iter().map(replay_frame::frame::recorded::update_bytes).sum();
            assert!(bytes > 0, "frame_recorded holds snapshot update bytes at keyframe");
        }
    }
    assert!(total(&plain).patched > 0 && total(&plain).in_place == total(&plain).patched && total(&plain).snapshots_patched == 0);

    plain.go_to(0, 10).unwrap();
    recorded.go_to(0, 10).unwrap();
    let got = plain.frame(1).unwrap();
    let (want, _) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "same-ordinal first plain");
    assert!(plain.slots.iter().all(|slot| slot.update.is_none()));

    let (got, got_recorded) = plain.frame_recorded(1).unwrap();
    let (want, want_recorded) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "same-ordinal switch to recorded");
    assert_same_recorded(&got_recorded, &want_recorded, "same-ordinal switch to recorded");
    assert!(plain.slots.iter().all(|slot| slot.update.is_some()));

    let got = plain.frame(1).unwrap();
    let (want, _) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "same-ordinal return to plain");
    assert!(plain.slots.iter().all(|slot| slot.update.is_none()), "returning to plain frame drops retained updates");

    plain.keyframe();
    recorded.keyframe();
    let got = plain.frame(1).unwrap();
    let (want, _) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "same-ordinal keyframe plain");
    assert!(plain.slots.iter().all(|slot| slot.update.is_none()));

    let (got, got_recorded) = plain.frame_recorded(1).unwrap();
    let (want, want_recorded) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "same-ordinal after keyframe recorded");
    assert_same_recorded(&got_recorded, &want_recorded, "same-ordinal after keyframe recorded");
    assert!(plain.slots.iter().all(|slot| slot.update.is_some()));

    plain.go_to(0, 11).unwrap();
    recorded.go_to(0, 11).unwrap();
    let (got, got_recorded) = plain.frame_recorded(1).unwrap();
    let (want, want_recorded) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "next ordinal recorded");
    assert_same_recorded(&got_recorded, &want_recorded, "next ordinal recorded");

    plain.go_to(0, 12).unwrap();
    recorded.go_to(0, 12).unwrap();
    let got = plain.frame(1).unwrap();
    let (want, _) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "advancing return to plain");
    assert!(plain.slots.iter().all(|slot| slot.update.is_none()), "advancing to plain frame drops retained updates");

    plain.go_to(0, 13).unwrap();
    recorded.go_to(0, 13).unwrap();
    plain.keyframe();
    recorded.keyframe();
    let (got, got_recorded) = plain.frame_recorded(1).unwrap();
    let (want, want_recorded) = recorded.frame_recorded(1).unwrap();
    assert_same_frame(&got, &want, "advancing plain to recorded keyframe");
    assert_same_recorded(&got_recorded, &want_recorded, "advancing plain to recorded keyframe");
    assert!(got_recorded.keyframe);
    assert!(plain.slots.iter().all(|slot| slot.update.is_some()));
}
