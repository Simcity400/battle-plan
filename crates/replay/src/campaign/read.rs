
use super::{BlockIndex, Replay, SeekResult};
use replay_data::{BranchId, Hour, codec::invalid_static, rows::TableId};
use replay_recording::columnar_frames::{Frames, Pool};
use replay_frame::frame::{DecodedFrame, RecordedFrame};
use std::{io, sync::Arc};

impl Replay {
    fn ordinal(&self, block: &BlockIndex, hour: Hour) -> io::Result<u32> {
        self.recording.run(block.run)?.ordinal(hour).ok_or_else(|| invalid_static("a block's hour lies outside its run"))
    }

    pub fn visit_blocks(
        &self,
        blocks: &[&BlockIndex],
        tables: &[TableId],
        mut visit: impl FnMut(&BlockIndex, &RecordedFrame, &DecodedFrame) -> io::Result<bool>,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let pool = self.pool.as_deref();
        let mut frames = Frames::new(&self.recording, tables)?;
        for block in blocks {
            check()?;
            let first = self.ordinal(block, block.first_hour)?;
            let last = self.ordinal(block, block.last_hour)?;
            Pool::on(pool, || frames.go_to(block.run, first))?;
            frames.keyframe();
            for ordinal in first..=last {
                if ordinal > first {
                    check()?;
                    Pool::on(pool, || frames.go_to(block.run, ordinal))?;
                }
                let (frame, recorded) = Pool::on(pool, || frames.frame_recorded(block.branch))?;
                if !visit(block, &recorded, &frame)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub fn read_block_alone(
        &self,
        block: &BlockIndex,
        tables: &[TableId],
        mut visit: impl FnMut(&RecordedFrame, &DecodedFrame) -> io::Result<()>,
        check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        self.visit_blocks(&[block], tables, |_, recorded, frame| visit(recorded, frame).map(|()| true), check)
    }

    pub fn block_at(&self, branch: BranchId, hour: Hour) -> Option<&BlockIndex> {
        self.index.iter().find(|block| block.branch == branch && block.first_hour <= hour && hour <= block.last_hour)
    }

    pub fn seek(&self, branch: BranchId, hour: Hour) -> io::Result<SeekResult> {
        self.seek_selected(branch, hour, None)
    }

    pub fn seek_selected(&self, branch: BranchId, hour: Hour, tables: Option<&[TableId]>) -> io::Result<SeekResult> {
        self.seek_selected_with(branch, hour, tables, || Ok(()))
    }

    pub fn seek_selected_with(&self, branch: BranchId, hour: Hour, tables: Option<&[TableId]>, mut check: impl FnMut() -> io::Result<()>) -> io::Result<SeekResult> {
        let Some(block) = self.block_at(branch, hour).copied() else {
            return Ok(SeekResult::Gap { branch, hour, reason: self.gap_reason(branch, hour)? });
        };
        check()?;
        let ordinal = self.ordinal(&block, hour)?;
        let mut frames = Frames::new(&self.recording, tables.unwrap_or(&TableId::ALL))?;
        let frame = Pool::on(self.pool.as_deref(), || frames.seek(block.run, ordinal).and_then(|()| frames.frame(branch)))?;
        Ok(SeekResult::Frame(engine_allocation::boxed(frame)?))
    }
}

pub struct HourCursor {
    replay: Arc<Replay>,
    tables: Vec<TableId>,
    changes: bool,
    reader_budget: Option<usize>,
    last: Option<(BranchId, Hour, u32)>,
    frames: Option<Frames<Held>>,
}

struct Held(Arc<Replay>);

impl std::ops::Deref for Held {
    type Target = replay_recording::columnar_frames::Recording;

    fn deref(&self) -> &Self::Target {
        &self.0.recording
    }
}

impl HourCursor {
    pub fn new(replay: Arc<Replay>, tables: &[TableId], changes: bool) -> Self {
        Self { replay, tables: tables.to_vec(), changes, reader_budget: None, last: None, frames: None }
    }

    pub fn bounded(replay: Arc<Replay>, tables: &[TableId], reader_budget: usize) -> Self {
        let mut cursor = Self::new(replay, tables, false);
        let fixed = size_of::<Self>() + cursor.tables.capacity() * size_of::<TableId>();
        cursor.reader_budget = Some(reader_budget.saturating_sub(fixed));
        cursor
    }

    pub fn replay(&self) -> &Arc<Replay> {
        &self.replay
    }

    pub fn tables(&self) -> &[TableId] { &self.tables }

    pub fn last(&self) -> Option<(BranchId, Hour)> {
        self.last.map(|(branch, hour, _)| (branch, hour))
    }

    pub fn continues(&self, branch: BranchId, hour: Hour) -> bool {
        self.last.is_some_and(|(held, last, run)| held == branch && hour >= last && self.replay.block_at(branch, hour).is_some_and(|block| block.run == run))
    }

    pub fn release(&mut self) {
        self.frames = None;
        self.last = None;
    }

    pub fn seek(&mut self, branch: BranchId, hour: Hour) -> io::Result<SeekResult> {
        let Some(block) = self.replay.block_at(branch, hour).copied() else {
            if self.reader_budget.is_some() { self.release(); }
            return Ok(SeekResult::Gap { branch, hour, reason: self.replay.gap_reason(branch, hour)? });
        };
        let ordinal = self.replay.ordinal(&block, hour)?;
        if self.reader_budget.is_some() && !self.last.is_some_and(|(prior_branch, prior_hour, _)| {
            prior_branch == branch && prior_hour <= hour
                && self.replay.block_at(prior_branch, prior_hour).is_some_and(|prior| prior.key() == block.key())
        }) {
            self.release();
        }
        if self.frames.is_none() {
            let frames = Frames::new(Held(Arc::clone(&self.replay)), &self.tables)?;
            self.frames = Some(if self.changes { frames.with_change_facts() } else { frames });
        }
        let frames = self.frames.as_mut().expect("made");
        let budget = self.reader_budget;
        let read = Pool::on(self.replay.pool.as_deref(), || {
            if let Some(budget) = budget {
                frames.frame_with_parked_readers(branch, block.run, ordinal, budget).map(|(frame, bytes)| (frame, bytes.is_some()))
            } else {
                frames.go_to(block.run, ordinal)?;
                frames.frame(branch).map(|frame| (frame, true))
            }
        });
        match read.and_then(|(frame, retained)| Ok((engine_allocation::boxed(frame)?, retained))) {
            Ok((frame, retained)) => {
                if retained { self.last = Some((branch, hour, block.run)); } else { self.release(); }
                Ok(SeekResult::Frame(frame))
            }
            Err(error) => {
                self.release();
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replay_frame::{branches::{BranchReason, BranchStart, Parentage}, frame::{Frame, Metrics, SharedView, TableUpdate, TableView}};
    use replay_recording::cells::TableReader;
    use replay_data::{identity::{CaptureMode, Identity, SampleBoundary, SessionStart, Verification}, rows::{CampaignRow, ClockRow, TableRows}};
    use replay_capture_spool::spool::Record;
    use replay_columnar::envelope::UNIT_ORDINALS;

    const CAMPAIGN: &[u8] = b"hour cursor";
    const BLOCK: Hour = UNIT_ORDINALS as Hour;
    const TABLES: [TableId; 2] = [TableId::Clock, TableId::Campaign];
    const FIRST: std::ops::Range<Hour> = 10..10 + 6 * BLOCK + 40;
    const LATER: Hour = 10 + 7 * BLOCK;

    fn frame(hour: Hour) -> Frame {
        Frame {
            branch: 1,
            hour,
            keyframe: true,
            metrics: Metrics::default(),
            tables: std::array::from_fn(|index| {
                let rows = match TableId::ALL[index] {
                    TableId::Clock => TableRows::Clock(vec![ClockRow { hour, ..Default::default() }]),
                    TableId::Campaign => TableRows::Campaign(vec![CampaignRow { id: CAMPAIGN.to_vec() }]),
                    table => TableRows::empty(table),
                };
                match replay_frame::frame::current_absence(rows.table()) {
                    Some(TableView::NotRecorded { mode }) => TableUpdate::NotRecorded { mode },
                    Some(TableView::Unavailable { reason }) => TableUpdate::Unavailable { reason },
                    None => TableUpdate::Snapshot(rows),
                    _ => unreachable!("absence is an omission or a named refusal"),
                }
            }),
        }
    }

    fn replay() -> Replay {
        Replay::from_records(recorder_derive::writers(), CAMPAIGN.to_vec(), &[records(frame)]).unwrap()
    }

    fn records(frame: impl Fn(Hour) -> Frame) -> Vec<Record> {
        let session = SessionStart {
            id: 1,
            process_id: 1,
            process_start_time: 1,
            qpc_frequency: 100,
            sample_boundary: SampleBoundary::EndOfHour,
            capture_mode: CaptureMode::Full,
            identity: Identity {
                game_version: "synthetic".into(),
                exe_hash: [1; 32],
                exe_size: 1,
                vanilla_hash: [2; 32],
                dlc: Vec::new(),
                mods: Vec::new(),
                coverage_policy: 1,
                language: "english".into(),
                recorder_version: "test".into(),
                collector_hash: [3; 32],
                verification: Verification::Unchecked,
            },
        };
        let branch = BranchStart { id: 1, session: 1, loaded_hour: Some(FIRST.start - 1), first_hour: FIRST.start, parent: Parentage::Unknown, reason: BranchReason::SessionStart };
        let mut records = vec![Record::SessionStart(session), Record::BranchStart(branch)];
        records.extend(FIRST.chain(LATER..LATER + 30).map(|hour| Record::Frame(frame(hour))));
        records
    }

    #[test]
    fn a_campaign_recording_of_an_earlier_format_opens_and_plays() {
        use replay_data::rows::compatibility::CAPTURE_ADDITIONS_FORMAT;
        let format = CAPTURE_ADDITIONS_FORMAT - 1;
        let added: Vec<TableId> = TableId::ALL.into_iter().filter(|table| !table.last_change().present(format) && table.last_change().retired.is_none()).collect();
        assert_eq!(added, [TableId::LandUnitOrder, TableId::StrategicRegionWeights, TableId::StateBuildingInputs, TableId::PowerSpendDays, TableId::CombatEngagementRefreshes, TableId::LandCombatEdges, TableId::FuelAwardRecipients, TableId::ScriptVariableNames, TableId::ScriptVariableValues, TableId::LandCombatResultObservers, TableId::LandDivisionDamage, TableId::DivisionKills, TableId::PowerSpendPayments, TableId::PowerSpendGains]);
        let bytes = crate::campaign::fixtures::encode_records_at(recorder_derive::writers_within, format, CAMPAIGN.to_vec(), &[records(|hour| frame_at(format, hour))]).unwrap();
        let replay = Replay::open(bytes).unwrap();
        assert_eq!((replay.sessions.len(), replay.branches.len()), (1, 1));
        assert_eq!(replay.recording.tables(), TableId::ALL.len() - added.len());
        assert_eq!(replay.format.version(), format);
        let gaps = replay.compatibility_gaps();
        for table in &added {
            let gap = table.last_change().table_gap(*table, format).unwrap().to_string();
            assert!(gaps.iter().any(|reason| reason.contains(&gap)), "{gap}: {gaps:?}");
        }
        let mut tables = TABLES.to_vec();
        tables.extend(&added);
        for hour in [FIRST.start, FIRST.start + BLOCK + 7, FIRST.end - 1, LATER + 29] {
            let SeekResult::Frame(frame) = replay.seek_selected(1, hour, Some(&tables)).unwrap() else { panic!("hour {hour} is a gap") };
            assert_eq!(*frame.tables[TableId::Clock.index()], TableView::Complete(TableRows::Clock(vec![ClockRow { hour, ..Default::default() }])), "hour {hour}");
            for table in &added {
                assert_eq!(*frame.tables[table.index()], TableView::NotRecorded { mode: replay.capture_mode }, "hour {hour}: {table:?}");
                assert!(!frame.recorded(*table), "hour {hour}: {table:?}");
            }
        }
    }

    fn frame_at(format: u16, hour: Hour) -> Frame {
        let mut frame = frame(hour);
        for table in TableId::ALL.into_iter().filter(|table| !table.last_change().present(format) && table.last_change().retired.is_none()) {
            frame.tables[table.index()] = TableUpdate::NotRecorded { mode: CaptureMode::Full };
        }
        frame
    }

    #[test]
    fn records_added_to_a_recording_of_an_earlier_format_are_recorded_in_its_format() {
        use replay_frame::branches::Gap;
        use replay_recording::{records::Records, columnar_frames::{Open, Recording}, control::ControlRecord};
        use crate::campaign::writer::{self, Addition};
        use replay_data::rows::compatibility::{CAPTURE_ADDITIONS_FORMAT, EVENT_LOSS_FORMAT};
        let format = CAPTURE_ADDITIONS_FORMAT - 1;
        let bytes = crate::campaign::fixtures::encode_records_at(recorder_derive::writers_within, format, CAMPAIGN.to_vec(), &[records(|hour| frame_at(format, hour))]).unwrap();
        let path = std::env::temp_dir().join(format!("hoi4-revise-earlier-format-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let opened = Recording::open_path_as(&path, Open::Newest).unwrap();
        assert_eq!(opened.format(), format);
        let gap = Gap { branch: 1, first_hour: LATER + 30, last_hour: LATER + 40, reason: "recovered".into() };
        let partial = ControlRecord::Partial { session: 1, reason: "the capture failed".into(), failed_hour: Some(LATER + 41) };
        let added = Records { gaps: vec![gap.clone()], controls: vec![partial.clone()], ..Records::default() };
        let closed = replay_settings::exit::Exit::new();
        closed.seal();
        let refused = writer::revise_under(&closed, &path, &opened, &Addition { records: Some(&added), ..Addition::default() }, false, 3).unwrap_err();
        assert_eq!(refused.to_string(), replay_settings::exit::CLOSED, "an addition asked for once BattlePlan closes is refused, never cut off before its footer");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        writer::revise(&path, &opened, &Addition { records: Some(&added), ..Addition::default() }, false, 3).unwrap();
        drop(opened);
        let reopened = Replay::open_path_as(&path, Open::Newest).unwrap();
        assert_eq!(reopened.format.version(), format);
        assert_eq!(reopened.recording.format(), format);
        assert!(reopened.gaps.contains(&gap), "{:?}", reopened.gaps);
        assert!(reopened.controls.contains(&partial), "{:?}", reopened.controls);
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        let loss = Records { controls: vec![ControlRecord::EventLoss { session: 1, branch: 1, after_hour: 1, hour: 2 }], ..Records::default() };
        let error = loss.encode(replay_data::format::FormatVersion::read(EVENT_LOSS_FORMAT - 1).unwrap()).unwrap_err();
        assert!(error.to_string().contains("EventLoss layout changed"), "{error}");
    }

    #[test]
    fn a_recording_of_a_newer_format_is_refused_by_name_however_it_is_opened() {
        let newer = replay_data::format::FormatVersion::CURRENT.version() + 1;
        let bytes = crate::campaign::fixtures::encode_records_naming(recorder_derive::writers(), newer, CAMPAIGN.to_vec(), &[records(frame)]).unwrap();
        for error in [replay_recording::columnar_frames::Recording::open(bytes.clone()).map(drop).unwrap_err(), Replay::open(bytes).map(drop).unwrap_err()] {
            assert!(error.to_string().contains(&format!("recording format {newer} is newer")), "{error}");
        }
    }

    fn restores(cursor: &HourCursor) -> Vec<u32> {
        TABLES.iter().map(|&table| cursor.frames.as_ref().and_then(|frames| frames.reader(table)).map_or(0, TableReader::restores)).collect()
    }

    #[test]
    fn bounded_readers_reuse_a_block_and_drop_on_seeks_or_budget_refusal() {
        let replay = Arc::new(replay());
        let first = FIRST.start;
        let mut cursor = HourCursor::bounded(Arc::clone(&replay), &TABLES, 1024 * 1024);
        for (hour, opens) in [(first, 2), (first + 1, 0), (first + 25, 0), (first, 2), (first + BLOCK, 2), (LATER, 2)] {
            let (got, counts) = replay_recording::cells::count_reads(|| cursor.seek(1, hour));
            let SeekResult::Frame(got) = got.unwrap() else { panic!("expected recorded hour {hour}") };
            let SeekResult::Frame(exact) = replay.seek_selected(1, hour, Some(&TABLES)).unwrap() else { panic!("expected exact hour {hour}") };
            assert_eq!(got.tables, exact.tables);
            assert_eq!(got.metrics.table_ns, exact.metrics.table_ns);
            for table in TABLES {
                assert_eq!(got.tables[table.index()].sampled_at(), exact.tables[table.index()].sampled_at());
            }
            assert_eq!(counts.values().map(|read| read.opened).sum::<u64>(), opens, "hour {hour}");
        }
        assert!(matches!(cursor.seek(1, FIRST.end).unwrap(), SeekResult::Gap { .. }));
        assert!(cursor.frames.is_none());
        let mut tight = HourCursor::bounded(replay, &TABLES, 0);
        assert!(matches!(tight.seek(1, first).unwrap(), SeekResult::Frame(_)));
        assert!(tight.frames.is_none());
    }

    #[test]
    fn a_cursor_moved_forward_by_days_goes_on_and_builds_what_a_seek_builds() {
        let replay = Arc::new(replay());
        let run = replay.block_at(1, FIRST.start).unwrap().run;
        assert_ne!(replay.block_at(1, LATER).unwrap().run, run, "a hole starts another run");
        let block_of = |hour: Hour| ((hour - FIRST.start) / BLOCK) as u32;
        let restored = |table: TableId, block: u32| replay.recording.table_entry(run, block, table.index()).unwrap().unit.restore.is_some();
        let sampled = |frame: &DecodedFrame| frame.tables.iter().map(SharedView::sampled_at).collect::<Vec<_>>();
        let mut cursor = HourCursor::new(Arc::clone(&replay), &TABLES, false);
        let mut crossed = [false; 2];
        let mut previous: Option<Hour> = None;
        for hour in FIRST.skip(5).step_by(24) {
            if let Some(last) = previous {
                assert!(cursor.continues(1, hour), "hour {hour} goes on from {last}");
            }
            let before = restores(&cursor);
            let SeekResult::Frame(frame) = cursor.seek(1, hour).unwrap() else { panic!("hour {hour} is a gap") };
            let SeekResult::Frame(sought) = replay.seek_selected(1, hour, Some(&TABLES)).unwrap() else { panic!("hour {hour} is a gap") };
            assert_eq!(frame.tables, sought.tables, "hour {hour}");
            assert_eq!(sampled(&frame), sampled(&sought), "hour {hour}");
            let Some(last) = previous else {
                previous = Some(hour);
                continue;
            };
            let after = restores(&cursor);
            for (at, &table) in TABLES.iter().enumerate() {
                let across = (block_of(last)..block_of(hour)).any(|block| restored(table, block));
                if block_of(last) != block_of(hour) {
                    crossed[usize::from(across)] = true;
                }
                assert_eq!(after[at] - before[at], u32::from(across), "{table:?} from hour {last} to {hour}");
            }
            previous = Some(hour);
        }
        assert_eq!(crossed, [true, true], "the walk crosses a block end with no restore and one with");
        let last = previous.unwrap();
        assert!(cursor.continues(1, last));
        assert!(!cursor.continues(1, last - 1));
        assert!(!cursor.continues(1, LATER));
        assert!(!cursor.continues(2, last + 1));
    }

    #[test]
    fn a_walk_across_block_ends_hands_the_frames_each_block_read_alone_hands() {
        let replay = replay();
        let blocks: Vec<&BlockIndex> = replay.index.iter().filter(|block| block.branch == 1).collect();
        assert!(blocks.len() > 2 && blocks[0].run != blocks[blocks.len() - 1].run, "blocks of two runs");
        let mut alone = Vec::new();
        for block in &blocks {
            replay
                .read_block_alone(
                    block,
                    &TABLES,
                    |recorded, frame| {
                        alone.push((recorded.clone(), frame.clone()));
                        Ok(())
                    },
                    || Ok(()),
                )
                .unwrap();
        }
        let mut at = 0;
        replay
            .visit_blocks(
                &blocks,
                &TABLES,
                |block, recorded, frame| {
                    let (want_recorded, want) = &alone[at];
                    let hour = frame.hour;
                    assert_eq!(hour, want.hour);
                    assert_eq!(recorded.keyframe, want_recorded.keyframe, "hour {hour}");
                    assert_eq!(recorded.keyframe, hour == block.first_hour, "hour {hour}");
                    for table in TABLES {
                        let index = table.index();
                        assert_eq!(frame.tables[index], want.tables[index], "{table:?} at hour {hour}");
                        assert_eq!(frame.tables[index].sampled_at(), want.tables[index].sampled_at(), "{table:?} at hour {hour}");
                        assert_eq!(recorded.tables[index], want_recorded.tables[index], "{table:?} at hour {hour}");
                    }
                    at += 1;
                    Ok(true)
                },
                || Ok(()),
            )
            .unwrap();
        assert_eq!(at, alone.len());
    }

    #[test]
    fn a_cursor_with_change_facts_hands_them_through_its_frames() {
        use replay_frame::frame::{HourInput, TableChanges};
        let replay = Arc::new(replay());
        let at = |cursor: &mut HourCursor, hour: Hour| {
            let SeekResult::Frame(frame) = cursor.seek(1, hour).unwrap() else { panic!("hour {hour} is a gap") };
            *frame
        };
        let since = |frame: &DecodedFrame, before: &DecodedFrame, table: TableId| {
            let identity = before.tables[table.index()].identity();
            match HourInput::new(frame).changes().since(table, Some(&identity), u64::MAX) {
                TableChanges::Rows(facts) => Some((facts.changed().len(), facts.changed().iter().any(|row| row.inserted), facts.deleted().len())),
                TableChanges::Unchanged => Some((0, false, 0)),
                TableChanges::Discontinuity(_) => None,
            }
        };
        let mut cursor = HourCursor::new(Arc::clone(&replay), &TABLES, true);
        let first = at(&mut cursor, FIRST.start + 3);
        let next = at(&mut cursor, FIRST.start + 4);
        assert!(matches!(
            HourInput::new(&next).changes().since(TableId::Clock, Some(&first.tables[TableId::Clock.index()].identity()), u64::MAX),
            TableChanges::Rows(_)
        ));
        assert_eq!(since(&next, &first, TableId::Clock), Some((1, false, 0)), "the next hour writes its clock row");
        assert!(SharedView::ptr_eq(&next.tables[TableId::Campaign.index()], &first.tables[TableId::Campaign.index()]));
        assert_eq!(since(&next, &first, TableId::Campaign), Some((0, false, 0)));
        let jumped = at(&mut cursor, FIRST.start + 6);
        assert_eq!(since(&jumped, &next, TableId::Clock), None, "a jump of two hours");
        let before = at(&mut cursor, FIRST.start + BLOCK - 1);
        let after = at(&mut cursor, FIRST.start + BLOCK);
        assert_eq!(since(&after, &before, TableId::Clock), Some((1, false, 0)), "the next hour across a block end");

        let mut plain = HourCursor::new(replay, &TABLES, false);
        let first = at(&mut plain, FIRST.start + 3);
        let next = at(&mut plain, FIRST.start + 4);
        assert!(next.tables[TableId::Clock.index()].changes_since(&first.tables[TableId::Clock.index()].identity()).is_none());
        assert_eq!(since(&next, &first, TableId::Clock), None, "a cursor without change facts names none");
    }
}
