use super::views;
use crate::layers::{Retirement, RetirementFlag, Settling};
use replay_data::{BranchId, Hour, rows::TableId};
use replay_frame::frame::DecodedFrame;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use viewer_runtime::{landed::Landed, reads::Reads};
use viewer_engagements::engagement_history::History;

const WHOLE: Duration = Duration::from_secs(120);

const LOOK: Duration = Duration::from_millis(2);

pub trait Asking: Clone + Send + 'static {
    type Key: Clone + PartialEq + std::fmt::Debug + Send + 'static;
    type Made: Send + 'static;
    fn key(&self) -> Self::Key;
    fn read(&self, branch: BranchId, hour: Hour, seek: u64, rows: Arc<DecodedFrame>, engaged: Option<Arc<History>>) -> Self::Made;
}

#[derive(Clone)]
pub enum NoCombat {}

impl Asking for NoCombat {
    type Key = std::convert::Infallible;
    type Made = std::convert::Infallible;

    fn key(&self) -> Self::Key {
        match *self {}
    }

    fn read(&self, _: BranchId, _: Hour, _: u64, _: Arc<DecodedFrame>, _: Option<Arc<History>>) -> Self::Made {
        match *self {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Joined {
    pub answered: bool,
    pub made: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Key<K> {
    generation: u64,
    branch: BranchId,
    hour: Hour,
    covered: bool,
    read: K,
}

#[derive(Debug, PartialEq, Eq)]
enum RowsError {
    Passed(String),
    TimedOut(String),
}

struct Maker<A: Asking> {
    reads: Reads<Key<A::Key>, Result<A::Made, RowsError>>,
    rows: Arc<views::Reader>,
    declared: Vec<TableId>,
    last: Option<Key<A::Key>>,
    newest: Arc<AtomicU64>,
}

pub struct Combat<A: Asking> {
    retiring: RetirementFlag,
    asking: Option<A>,
    maker: Option<Maker<A>>,
    learned: Vec<TableId>,
    generation: u64,
    asked: Option<Key<A::Key>>,
    handed: Option<Key<A::Key>>,
    made: Option<Landed<A::Made>>,
    loading: Vec<String>,
}

impl<A: Asking> Default for Combat<A> {
    fn default() -> Self {
        Self { retiring: RetirementFlag::default(), asking: None, maker: None, learned: Vec::new(), generation: 0, asked: None, handed: None, made: None, loading: Vec::new() }
    }
}

impl<A: Asking> Combat<A> {
    pub fn retirement(&self) -> Retirement {
        self.retiring.handle()
    }

    pub fn ask(&mut self, asking: Option<A>) {
        if asking.is_none() {
            (self.maker, self.asked, self.handed, self.made) = (None, None, None, None);
            self.loading.clear();
        }
        self.asking = asking;
    }

    pub fn reset(&mut self) {
        self.generation += 1;
        (self.asked, self.handed, self.made) = (None, None, None);
    }

    pub fn ask_step(&mut self, store: &views::Store, applied: Option<Hour>, branch: Option<BranchId>, epoch: u64, engaged: Option<Arc<History>>) -> bool {
        self.asked = None;
        let (Some(asking), Some(hour), Some(branch)) = (self.asking.clone(), applied, branch) else { return false };
        let covered = engaged.as_deref().is_some_and(|index| index.covers(branch, hour));
        let key = Key { generation: self.generation, branch, hour, covered, read: asking.key() };
        if self.handed.as_ref() == Some(&key) {
            return false;
        }
        self.loading.clear();
        let learned = &self.learned;
        let maker = self.maker.get_or_insert_with(|| Maker::open(store, learned));
        if maker.last.as_ref() != Some(&key) {
            maker.last = Some(key.clone());
            maker.newest.fetch_add(1, Ordering::AcqRel);
        }
        let (rows, newest) = (Arc::clone(&maker.rows), Arc::clone(&maker.newest));
        let stamp = newest.load(Ordering::Acquire);
        maker.reads.ask(
            key.clone(),
            move || rows_at(&rows, hour, epoch, &newest, stamp).map(|frame| asking.read(branch, hour, epoch, frame, engaged)),
            || {},
        );
        self.asked = Some(key);
        true
    }

    pub fn join(&mut self, settling: Settling) -> Joined {
        let (Some(key), Some(maker)) = (self.asked.take(), self.maker.as_mut()) else { return Joined { answered: true, made: false } };
        if settling.waits() {
            let until = Instant::now() + WHOLE;
            while !self.retiring.requested() && maker.reads.busy() {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() || maker.reads.wait(|done| *done == key, left.min(RetirementFlag::POLL)) {
                    break;
                }
            }
        }
        let missed = maker.reads.take_missed();
        let answered = missed.is_empty();
        if !answered {
            self.loading.push(format!("{missed:?}"));
            self.learned.extend(missed);
            self.learned.sort_unstable_by_key(|table| table.index());
            self.learned.dedup();
            maker.declare(&self.learned);
        }
        let mut made = false;
        match maker.reads.take(|done| *done == key).map(|(_, read)| read.transpose()) {
            Some(Ok(read)) => {
                (self.made, self.handed) = (Some(read), Some(key));
                made = true;
            }
            Some(Err(RowsError::Passed(why))) => {
                self.loading.push(why);
                self.handed = Some(key);
            }
            Some(Err(RowsError::TimedOut(why))) => self.loading.push(why),
            None if answered => self.loading.push(format!("its read of hour {}", key.hour)),
            None => {}
        }
        Joined { answered, made }
    }

    pub fn take(&mut self) -> Option<Landed<A::Made>> {
        self.made.take()
    }

    pub fn loading(&self) -> &[String] {
        &self.loading
    }

    pub fn open(&self) -> bool {
        self.asking.is_some()
    }

    pub fn learned(&self) -> usize {
        self.learned.len()
    }
}

impl<A: Asking> Maker<A> {
    fn open(store: &views::Store, learned: &[TableId]) -> Self {
        let declared = Self::tables(learned);
        let rows = Arc::new(store.reader("combat window", &declared.iter().copied().map(views::Need::every).collect::<Vec<_>>()));
        Self { reads: Reads::new("combat window reads"), rows, declared, last: None, newest: Arc::default() }
    }

    fn tables(learned: &[TableId]) -> Vec<TableId> {
        let mut tables: Vec<TableId> = viewer_foundation::perspective::VISION_TABLES.iter().chain(learned).copied().collect();
        tables.sort_unstable_by_key(|table| table.index());
        tables.dedup();
        tables
    }

    fn declare(&mut self, learned: &[TableId]) {
        let tables = Self::tables(learned);
        if tables != self.declared {
            self.rows.declare_only(&tables);
            self.declared = tables;
        }
    }
}

impl<A: Asking> Drop for Maker<A> {
    fn drop(&mut self) {
        self.newest.fetch_add(1, Ordering::AcqRel);
    }
}

fn rows_at(rows: &views::Reader, hour: Hour, epoch: u64, newest: &AtomicU64, stamp: u64) -> Result<Arc<DecodedFrame>, RowsError> {
    rows_at_within(rows, hour, epoch, newest, stamp, WHOLE)
}

fn rows_at_within(rows: &views::Reader, hour: Hour, epoch: u64, newest: &AtomicU64, stamp: u64, limit: Duration) -> Result<Arc<DecodedFrame>, RowsError> {
    let until = Instant::now() + limit;
    loop {
        let seen = rows.published();
        if let Some((frame, _, seek)) = rows.newest_seek()
            && seek == epoch
        {
            if frame.hour == hour {
                return Ok(frame);
            }
            if frame.hour > hour {
                return Err(RowsError::Passed(format!("its rows of hour {hour}, which the side's steps have passed")));
            }
        }
        if newest.load(Ordering::Acquire) != stamp {
            return Err(RowsError::TimedOut(format!("its rows of hour {hour}, passed over for a newer hour")));
        }
        if Instant::now() >= until {
            return Err(RowsError::TimedOut(format!("its rows of hour {hour}, which did not come (waiting for {:?})", rows.arriving())));
        }
        rows.wait_newer(seen, LOOK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playhead_host::{
        open,
        views::{
            apart::Apart,
            tests::{RECORDING, hourly_steps, record},
        },
    };
    use replay_data::scope::CaptureMode;
    use std::sync::{Mutex, mpsc};

    #[derive(Clone)]
    struct Held {
        started: Arc<Mutex<mpsc::Sender<Hour>>>,
        finish: Arc<Mutex<mpsc::Receiver<()>>>,
    }

    impl Asking for Held {
        type Key = ();
        type Made = Hour;

        fn key(&self) {}

        fn read(&self, _: BranchId, hour: Hour, _: u64, rows: Arc<DecodedFrame>, _: Option<Arc<History>>) -> Hour {
            assert_eq!(rows.hour, hour, "read over the rows of its own hour");
            let _ = self.started.lock().unwrap().send(hour);
            self.finish.lock().unwrap().recv_timeout(WHOLE).expect("the test lets the read finish");
            rows.hour
        }
    }

    #[test]
    fn the_read_handed_with_a_publication_is_of_its_hour_and_a_late_one_never_is() {
        let entry = open(record("combat window publication", 8), hourly_steps()).unwrap();
        entry.set_capture(CaptureMode::Full);
        entry.playhead().seek(1);
        let mut fed = Apart::new(&entry, RECORDING.branch).unwrap();
        let (started, reads) = mpsc::channel();
        let (finish, finishing) = mpsc::channel();
        let mut combat: Combat<Held> = Combat::default();
        combat.ask(Some(Held { started: Arc::new(Mutex::new(started)), finish: Arc::new(Mutex::new(finishing)) }));
        let branch = Some(RECORDING.branch);
        let epoch = entry.playhead().epoch();
        let read = || reads.recv_timeout(WHOLE).expect("a read starts");

        assert!(combat.ask_step(fed.store(), Some(1), branch, epoch, None));
        assert!(fed.drive(1));
        assert_eq!(read(), 1);
        finish.send(()).unwrap();
        assert!(combat.join(Settling::still()).answered);
        assert_eq!(combat.take().map(Landed::into_inner), Some(1), "the head standing still waits for its hour's read");
        assert!(!combat.ask_step(fed.store(), Some(1), branch, epoch, None), "the frame holds hour 1's read: nothing is read again");

        assert!(combat.ask_step(fed.store(), Some(2), branch, epoch, None));
        assert!(fed.drive(2));
        assert_eq!(read(), 2);
        assert!(combat.join(Settling::step()).answered);
        assert_eq!(combat.take(), None, "playing, the step does not wait for the pending read");
        assert_eq!(combat.loading(), ["its read of hour 2"], "and says what the window waits for");

        assert!(combat.ask_step(fed.store(), Some(3), branch, epoch, None));
        assert!(fed.drive(3));
        finish.send(()).unwrap();
        assert_eq!(read(), 3, "hour 3's read follows hour 2's");
        finish.send(()).unwrap();
        assert!(combat.join(Settling::still()).answered);
        assert_eq!(combat.take().map(Landed::into_inner), Some(3), "hour 3's publication hands hour 3's read, never hour 2's");
    }

    #[test]
    fn a_join_reports_only_the_read_it_took() {
        let entry = open(record("combat window join", 8), hourly_steps()).unwrap();
        entry.set_capture(CaptureMode::Full);
        entry.playhead().seek(1);
        let mut fed = Apart::new(&entry, RECORDING.branch).unwrap();
        let (started, reads) = mpsc::channel();
        let (finish, finishing) = mpsc::channel();
        let mut combat: Combat<Held> = Combat::default();
        combat.ask(Some(Held { started: Arc::new(Mutex::new(started)), finish: Arc::new(Mutex::new(finishing)) }));
        let (branch, epoch) = (Some(RECORDING.branch), entry.playhead().epoch());
        assert!(combat.ask_step(fed.store(), Some(1), branch, epoch, None));
        assert!(fed.drive(1));
        assert_eq!(reads.recv_timeout(WHOLE), Ok(1));
        finish.send(()).unwrap();
        assert_eq!(combat.join(Settling::still()), Joined { answered: true, made: true });
        assert!(combat.ask_step(fed.store(), Some(0), branch, epoch, None), "hour 1's read is not taken yet: hour 0 is asked");
        assert_eq!(combat.join(Settling::still()), Joined { answered: true, made: false }, "hour 0's rows are passed: this join took nothing");
        assert!(!combat.ask_step(fed.store(), Some(0), branch, epoch, None), "a passed hour is handed so a paused turn does not retry it forever");
        assert_eq!(combat.take().map(Landed::into_inner), Some(1), "hour 1's read is still the frame's to take");
    }

    #[test]
    fn rows_of_the_seek_before_never_answer_a_read_of_the_same_hour() {
        let entry = open(record("combat window seek", 8), hourly_steps()).unwrap();
        entry.set_capture(CaptureMode::Full);
        entry.playhead().seek(1);
        let mut fed = Apart::new(&entry, RECORDING.branch).unwrap();
        let maker = Maker::<Held>::open(fed.store(), &[]);
        let rows = Arc::clone(&maker.rows);
        assert!(fed.drive(1));
        let before = entry.playhead().epoch();
        let (asked, newer) = (AtomicU64::new(0), AtomicU64::new(1));
        assert_eq!(rows_at(&rows, 1, before, &asked, 0).map(|frame| frame.hour), Ok(1), "the rows of hour 1 at the seek they were posted at");
        assert!(matches!(rows_at(&rows, 1, before + 1, &newer, 0), Err(RowsError::TimedOut(_))), "a read asked after the seek waits for its own rows, never taking these");
        assert!(matches!(rows_at_within(&rows, 2, before, &asked, 0, Duration::ZERO), Err(RowsError::TimedOut(_))), "a timeout stays retryable rather than handed");
        assert!(matches!(rows_at_within(&rows, 0, before, &asked, 0, Duration::ZERO), Err(RowsError::Passed(_))), "a passed hour is distinguished from a timeout");
    }
}
