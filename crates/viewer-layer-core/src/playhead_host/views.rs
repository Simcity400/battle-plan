
pub mod apart;
#[cfg(any(test, feature = "test-support"))]
pub mod tests;
pub mod touched;
pub mod view;

use super::{
    Entry, lock,
    mirror::{Mirror, Mirrors},
    script_values::{ExactColumns, ScriptValues, TABLE as SCRIPT_VALUES},
};
use crate::rebuilt::panicked;
use playhead::changes::{Changes, Lives};
use replay_columnar::state::Kind;
use replay_data::{
    BranchId, Hour,
    rows::{TABLE_COUNT, TableId},
};
use replay_frame::frame::{DecodedFrame, Metrics, SharedView, Stamp, TableView, not_declared};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, atomic::{AtomicU64, Ordering}},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use touched::Touched;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recording {
    pub branch: BranchId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    pub table: TableId,
    pub columns: u64,
}

impl Need {
    pub const fn every(table: TableId) -> Self {
        Self { table, columns: u64::MAX }
    }

    pub const fn columns(table: TableId, mask: u64) -> Self {
        Self { table, columns: mask }
    }

    pub const fn keys(table: TableId) -> Self {
        Self { table, columns: 0 }
    }
}

pub type Told = BTreeMap<TableId, Option<BTreeMap<u128, u64>>>;

pub fn tell<'a>(into: &mut Told, table: TableId, rows: Option<impl IntoIterator<Item = (&'a u128, &'a u64)>>) {
    let held = into.entry(table).or_insert_with(|| Some(BTreeMap::new()));
    match (held, rows) {
        (Some(held), Some(rows)) => {
            for (&key, &columns) in rows {
                *held.entry(key).or_default() |= columns;
            }
        }
        (held, _) => *held = None,
    }
}

#[derive(Clone)]
pub struct Keyed {
    pub table: usize,
    pub keys: Arc<BTreeSet<u64>>,
    pub version: u64,
}

#[derive(Clone, Debug, Default)]
pub struct HeldKeys(Vec<(usize, Arc<BTreeSet<u64>>)>);

impl HeldKeys {
    pub fn of_tables(held: Vec<(usize, Arc<BTreeSet<u64>>)>) -> Self {
        Self(held)
    }

    pub fn of(&self, table: TableId) -> Option<&Arc<BTreeSet<u64>>> {
        self.0.iter().find(|(held, _)| *held == table.index()).map(|(_, keys)| keys)
    }
}

pub fn prefix(key: u128) -> u64 {
    (key >> 32) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Make {
    InPlace,
    Copied,
    Whole(Whole),
    Keyed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whole {
    First,
    Reset,
    Gap,
    Status,
    Log,
    Unkeyed,
    Untold,
    Cost,
    Refused,
}

impl Whole {
    pub fn name(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Reset => "reset",
            Self::Gap => "gap",
            Self::Status => "status",
            Self::Log => "log",
            Self::Unkeyed => "unkeyed",
            Self::Untold => "untold",
            Self::Cost => "cost",
            Self::Refused => "refused",
        }
    }
}


pub const FIRST_MAKE_NS: u64 = 500_000;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    Step,
    Ready,
    PerSecond(u32),
}

impl Pace {
    fn interval(self) -> Option<Duration> {
        match self {
            Self::Step | Self::Ready => None,
            Self::PerSecond(times) => Some(Duration::from_secs(1) / times.max(1)),
        }
    }
}


const MEASURED_FROM: usize = 32;

const COPY_UNMEASURED: f64 = 0.125;

#[derive(Clone, Copy, Debug, Default)]
pub struct Costs {
    pub whole: Option<f64>,
    pub patch: Option<f64>,
    pub copy: Option<f64>,
}

impl Costs {
    pub fn learn(held: &mut Option<f64>, took: Duration, rows: usize) {
        if rows >= MEASURED_FROM {
            let each = took.as_nanos() as f64 / rows as f64;
            *held = Some(held.map_or(each, |held| held * 0.75 + each * 0.25));
        }
    }

    pub fn relative(self) -> (f64, f64) {
        match self.whole.filter(|whole| *whole > 0.0) {
            Some(whole) => (self.patch.map_or(1.0, |patch| patch / whole), self.copy.map_or(COPY_UNMEASURED, |copy| copy / whole)),
            None => (1.0, COPY_UNMEASURED),
        }
    }

    pub fn patching(self, touched: usize, copied: usize) -> f64 {
        let (patch, copy) = self.relative();
        touched as f64 * patch + copied as f64 * copy
    }

    pub fn bound(self, rows: usize) -> usize {
        (2.0 * rows.max(1) as f64 / self.relative().0.max(f64::EPSILON)).ceil() as usize
    }
}


pub struct Set {
    pub columns: u64,
    pub made: Mutex<Columns>,
}

pub struct Columns {
    pub stamp: u64,
    pub built: Option<(u64, SharedView)>,
    pub built_failure: Option<view::HeadFailure>,
    pub rows: usize,
    pub touched: Result<Touched, Whole>,
    pub bound: usize,
    pub whole_read: bool,
    pub spare: Option<(SharedView, Vec<(u128, u64)>)>,
    pub costs: Costs,
    pub untold: bool,
    pub told: bool,
    pub last_ns: u64,
}


pub fn mine(made: &mut Mutex<Columns>) -> &mut Columns {
    made.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub type Source<'a> = (&'a Mirror, &'a [Kind], bool);

impl Columns {
    pub fn new() -> Self {
        Self { stamp: 0, built: None, built_failure: None, rows: 0, touched: Err(Whole::First), bound: 0, whole_read: false, spare: None, costs: Costs::default(), untold: false, told: false, last_ns: 0 }
    }

    pub fn arriving(&mut self) {
        (self.built, self.built_failure, self.touched) = (None, None, Err(Whole::First));
        self.spare = None;
        self.stamp += 1;
    }

    pub fn make(&mut self, table: usize, columns: u64, from: Source<'_>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> SharedView {
        self.make_from(table, columns, from, order, made, |held, previous, since, order| {
            held.remake(TableId::ALL[table], columns, previous, since, from, order)
        })
    }

    fn make_from(&mut self, table: usize, columns: u64, from: Source<'_>, order: &mut Vec<(u128, u32)>, made: &mut Made, build: impl FnOnce(&mut Self, Option<SharedView>, Result<&[(u128, u64)], Whole>, &mut Vec<(u128, u32)>) -> (TableView, view::Made, Make)) -> SharedView {
        let at = made.at();
        made.made_on();
        let started = Instant::now();
        let previous = self.built.take().map(|(_, view)| view);
        let since = self.since(from);
        if let (Some(previous), Ok(touched)) = (&previous, &since)
            && touched.is_empty()
            && holds_rows(previous)
        {
            self.spare = None;
            self.built = Some((self.stamp, previous.clone()));
            return previous.clone();
        }
        let (view, built, how) = build(self, previous, since.as_deref().map_err(|why| *why), order);
        let took = started.elapsed();
        self.last_ns = took.as_nanos() as u64;
        made.builds.push((table, took.as_nanos() as u64, built.rows, how, at));
        if let Make::Whole(_) = how {
            Costs::learn(&mut self.costs.whole, took, built.rows);
        }
        if self.told {
            made.told.push((table, columns, since.ok()));
        }
        made.untold += built.untold;
        self.untold = built.untold > 0;
        let view = held_fields(SharedView::from(view), columns, from.1.len());
        self.rows = match view.held() {
            TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.len(),
            _ => 0,
        };
        self.bound = if self.whole_read { self.costs.bound(self.rows) } else { usize::MAX };
        self.built_failure = built.failure.map(|kind| view::HeadFailure {
            table: TableId::ALL[table],
            kind,
        });
        self.built = Some((self.stamp, view.clone()));
        view
    }

    pub fn failed_make(&mut self, table: usize, payload: &(dyn std::any::Any + Send)) -> SharedView {
        let failure = view::HeadFailure {
            table: TableId::ALL[table],
            kind: view::HeadFailureKind::Panicked(panicked(payload)),
        };
        let view = SharedView::from(TableView::Unavailable {
            reason: failure.format(),
        });
        (self.touched, self.untold) = (Err(Whole::First), false);
        self.spare = None;
        self.built_failure = Some(failure);
        self.built = Some((self.stamp, view.clone()));
        view
    }

    pub fn keyed_made(&mut self, table: usize, columns: u64, from: Source<'_>, keys: &BTreeSet<u64>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> (SharedView, Option<view::HeadFailure>) {
        self.keyed_from(table, columns, from, order, made, |order| view::make_of(TableId::ALL[table], from.0, (from.1, columns), from.2, |key| keys.contains(&prefix(key)), order))
    }

    fn keyed_from(&mut self, table: usize, columns: u64, from: Source<'_>, order: &mut Vec<(u128, u32)>, made: &mut Made, decode: impl FnOnce(&mut Vec<(u128, u32)>) -> (TableView, view::Made)) -> (SharedView, Option<view::HeadFailure>) {
        let (mirror, kinds, gap) = from;
        let at = made.at();
        made.made_on();
        let started = Instant::now();
        if self.told {
            let since = if self.built.is_none() { self.since(from).ok() } else { None };
            made.told.push((table, columns, since));
        }
        let (view, built) = decode(order);
        made.builds.push((table, started.elapsed().as_nanos() as u64, built.rows, Make::Keyed, at));
        made.untold += built.untold;
        let sampled = if matches!(view, TableView::Complete(_)) && !gap { mirror.sampled() } else { None };
        let failure = built.failure.map(|kind| view::HeadFailure {
            table: TableId::ALL[table],
            kind,
        });
        (held_fields(SharedView::from(view), columns, kinds.len()).with_sampled_at(sampled), failure)
    }

    pub fn since(&mut self, (mirror, _, gap): Source<'_>) -> Result<Vec<(u128, u64)>, Whole> {
        if gap {
            self.touched = Ok(Touched::default());
            return Err(Whole::Gap);
        }
        let since = match &mut self.touched {
            Ok(touched) => touched.take(mirror).ok_or(Whole::Unkeyed),
            Err(why) => Err(*why),
        };
        if self.touched.is_err() {
            self.touched = Ok(Touched::default());
        }
        since.and_then(|rows| if self.untold { Err(Whole::Untold) } else { Ok(rows) })
    }

    pub fn remake(&mut self, table: TableId, columns: u64, previous: Option<SharedView>, since: Result<&[(u128, u64)], Whole>, from: Source<'_>, order: &mut Vec<(u128, u32)>) -> (TableView, view::Made, Make) {
        let (previous, touched) = match (previous, since) {
            (Some(previous), Ok(touched)) if holds_rows(&previous) => (previous, touched),
            (previous, since) => {
                self.spare = None;
                let why = match (previous, since) {
                    (_, Err(why)) => why,
                    (None, _) => Whole::First,
                    (Some(_), Ok(_)) => Whole::Status,
                };
                return self.whole(table, columns, why, from, order);
            }
        };
        let rows = self.rows;
        let shared = previous.is_shared();
        let on_newest = self.costs.patching(touched.len(), if shared { rows } else { 0 });
        let mut made = view::Made::default();
        if let Some((spare, since)) = self.spare.take().filter(|(spare, _)| !spare.is_shared()) {
            let anew = touched::merge(since.into_iter().chain(touched.iter().copied()).collect());
            let on_spare = self.costs.patching(anew.len(), 0);
            if on_spare < on_newest && on_spare < rows as f64 {
                let started = Instant::now();
                let patched = spare.try_unwrap().map_err(|_| io::Error::other(format!("{table:?}'s spare is held"))).and_then(|held| self.patch(table, columns, held, &anew, from, &mut made));
                if let Ok(view) = patched {
                    Costs::learn(&mut self.costs.patch, started.elapsed(), made.rows);
                    self.keep(previous, touched);
                    return (view, made, Make::InPlace);
                }
                made = view::Made::default();
            }
        }
        if on_newest >= rows as f64 {
            self.keep(previous, touched);
            return self.whole(table, columns, Whole::Cost, from, order);
        }
        let (held, previous) = if shared {
            let started = Instant::now();
            let copied = copy(table, &previous);
            Costs::learn(&mut self.costs.copy, started.elapsed(), rows);
            (copied, Some(previous))
        } else {
            (previous.try_unwrap().map_err(|_| io::Error::other(format!("{table:?}'s view is held"))), None)
        };
        let started = Instant::now();
        let patched = held.and_then(|held| self.patch(table, columns, held, touched, from, &mut made));
        if let Some(previous) = previous && patched.is_ok() {
            self.keep(previous, touched);
        }
        match patched {
            Ok(view) => {
                Costs::learn(&mut self.costs.patch, started.elapsed(), made.rows);
                (view, made, if shared { Make::Copied } else { Make::InPlace })
            }
            Err(_) => self.whole(table, columns, Whole::Refused, from, order),
        }
    }

    pub fn patch(&self, table: TableId, columns: u64, held: TableView, anew: &[(u128, u64)], (mirror, kinds, _): Source<'_>, made: &mut view::Made) -> io::Result<TableView> {
        view::patch(table, mirror, (kinds, columns), held, anew, made)
    }

    pub fn whole(&self, table: TableId, columns: u64, why: Whole, (mirror, kinds, gap): Source<'_>, order: &mut Vec<(u128, u32)>) -> (TableView, view::Made, Make) {
        let (view, made) = view::make(table, mirror, (kinds, columns), gap, order);
        (view, made, Make::Whole(why))
    }

    pub fn keep(&mut self, view: SharedView, since: &[(u128, u64)]) {
        if view.is_shared() && self.costs.patching(since.len(), 0) < self.rows as f64 {
            self.spare = Some((view, since.to_vec()));
        }
    }
}


#[derive(Default)]
pub struct Made {
    pub builds: Vec<(usize, u64, usize, Make, u64)>,
    pub untold: u64,
    pub told: Vec<(usize, u64, Option<Vec<(u128, u64)>>)>,
    pub told_reader: Vec<(u64, usize, Vec<(u128, u64)>)>,
    pub told_from: usize,
    pub told_reader_from: usize,
    pub origin: Option<Instant>,
    pub on: Option<String>,
}

impl Made {
    pub fn since(origin: Option<Instant>) -> Self {
        Self { origin, ..Self::default() }
    }

    pub fn at(&self) -> u64 {
        self.origin.map_or(0, |origin| origin.elapsed().as_nanos() as u64)
    }

    pub fn merge(&mut self, mut other: Self) {
        self.builds.append(&mut other.builds);
        self.told.append(&mut other.told);
        self.told_reader.append(&mut other.told_reader);
        self.untold += other.untold;
        self.on = self.on.take().or(other.on);
    }

    pub fn made_on(&mut self) {
        if self.on.is_none() {
            self.on = std::thread::current().name().map(str::to_owned);
        }
    }
}


pub fn note(touched: &mut Result<Touched, Whole>, changes: &Changes, moved: bool, held: u64, bound: usize) {
    let Ok(rows) = touched else { return };
    let why = match (changes.reset(), changes.log().is_some(), moved) {
        (true, ..) => Some(Whole::Reset),
        (_, true, _) => Some(Whole::Log),
        (.., true) => Some(Whole::Status),
        _ => None,
    };
    let (Some(gone), Some(born), None) = (keyed(changes.deleted()), keyed(changes.inserted()), why) else {
        *touched = Err(why.unwrap_or(Whole::Unkeyed));
        return;
    };
    for &key in gone.iter().chain(born) {
        rows.key(key);
    }
    for written in changes.columns() {
        let column = if written.column() < 64 { 1 << written.column() } else { view::EVERY };
        if column & held == 0 {
            continue;
        }
        for &slot in written.slots() {
            rows.write(slot, column);
        }
    }
    if let Some(old) = changes.renumber() {
        rows.renumber(old);
    }
    if rows.rows() > bound {
        *touched = Err(Whole::Cost);
    }
}


pub fn keyed(lives: &Lives) -> Option<&[u128]> {
    if lives.is_empty() {
        return Some(&[]);
    }
    lives.keys().filter(|keys| keys.len() == lives.len())
}

pub fn holds_rows(view: &SharedView) -> bool {
    matches!(view.held(), TableView::Complete(_) | TableView::Truncated { .. })
}

pub fn copy(table: TableId, shared: &SharedView) -> io::Result<TableView> {
    match shared.held() {
        TableView::Complete(rows) => rows.try_clone().map(TableView::Complete),
        TableView::Truncated { rows, missing } => rows.try_clone().map(|rows| TableView::Truncated { rows, missing: *missing }),
        _ => Err(io::Error::other(format!("{table:?} holds no rows to patch"))),
    }
}


pub fn held_fields(view: SharedView, held: u64, columns: usize) -> SharedView {
    let every = if columns >= 64 { u64::MAX } else { (1 << columns) - 1 };
    if held & every == every { view } else { view.with_fields(held) }
}

pub fn over(mask: u64, columns: usize) -> u64 {
    if columns >= 64 { mask } else { mask & ((1 << columns) - 1) }
}


const MAKE_THREADS: usize = 4;

const MAKE_THREAD: &str = "map views: making";

struct Kept {
    kinds: Vec<Kind>,
    columns: u64,
    gap: bool,
    views: Vec<Set>,
    exact: Option<Arc<ExactColumns>>,
}

impl Kept {
    fn new(kinds: Vec<Kind>) -> Self {
        Self { kinds, columns: 0, gap: false, views: Vec::new(), exact: None }
    }

    fn set(&self, columns: u64) -> Option<usize> {
        let columns = over(columns, self.kinds.len());
        self.views.iter().position(|held| held.columns == columns)
    }

    fn at(&mut self, columns: u64) -> usize {
        let columns = over(columns, self.kinds.len());
        match self.set(columns) {
            Some(at) => at,
            None => {
                self.views.push(Set { columns, made: Mutex::new(Columns::new()) });
                self.columns |= columns;
                self.views.len() - 1
            }
        }
    }

    fn stale(&self, columns: u64) -> bool {
        self.set(columns).is_none_or(|at| {
            let held = lock(&self.views[at].made);
            held.built.as_ref().is_none_or(|(stamp, _)| *stamp != held.stamp)
        })
    }

    fn costing(&self, columns: u64) -> u64 {
        let Some(at) = self.set(columns) else { return FIRST_MAKE_NS };
        let held = lock(&self.views[at].made);
        if held.built.as_ref().is_some_and(|(stamp, _)| *stamp == held.stamp) {
            return 0;
        }
        if held.last_ns > 0 { held.last_ns } else { FIRST_MAKE_NS }
    }

    fn arriving(&mut self) {
        self.views.iter_mut().for_each(|held| mine(&mut held.made).arriving());
    }

    fn stamp(&self, columns: u64) -> u64 {
        self.set(columns).map_or(0, |at| lock(&self.views[at].made).stamp)
    }

    fn tells(&self, columns: u64) -> bool {
        self.set(columns).is_some_and(|at| lock(&self.views[at].made).told)
    }

    fn keyed(&self, table: usize, mirror: Option<&Mirror>, keys: &BTreeSet<u64>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> (SharedView, Option<view::HeadFailure>) {
        let columns = over(u64::MAX, self.kinds.len());
        let Some(at) = self.set(columns) else { return (not_declared(TableId::ALL[table]), None) };
        let Some(mirror) = mirror else {
            viewer_runtime::view_record::add("map views", "sets_withheld", 1);
            return (not_declared(TableId::ALL[table]), None);
        };
        let mut held = lock(&self.views[at].made);
        let from = (mirror, self.kinds.as_slice(), self.gap || mirror.in_gap());
        match catch_unwind(AssertUnwindSafe(|| {
            if let (Some(source), Some(hour), false) = (&self.exact, mirror.head(), from.2) {
                held.keyed_from(table, columns, from, order, made, |order| source.decoded(hour, columns, Some(keys), order))
            } else {
                held.keyed_made(table, columns, from, keys, order, made)
            }
        })) {
            Ok(result) => result,
            Err(payload) => {
                let view = held.failed_make(table, &*payload);
                (view, held.built_failure.clone())
            }
        }
    }

    fn answers(&self, columns: u64, mirror: Option<&Mirror>, hour: Hour) -> bool {
        self.home(columns, mirror) && mirror.is_some_and(|mirror| stands_at(mirror, hour))
    }

    fn home(&self, columns: u64, mirror: Option<&Mirror>) -> bool {
        self.set(columns).is_some() && mirror.is_some_and(|mirror| mirror.keyed() && (self.exact.is_some() || holds_every(mirror, columns, self.kinds.len())))
    }

    fn view(&self, table: usize, columns: u64, mirror: Option<&Mirror>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> SharedView {
        self.view_with_failure(table, columns, mirror, order, made).0
    }

    fn view_with_failure(&self, table: usize, columns: u64, mirror: Option<&Mirror>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> (SharedView, Option<view::HeadFailure>) {
        let Some(at) = self.set(columns) else { return (not_declared(TableId::ALL[table]), None) };
        let set = &self.views[at];
        let Some(mirror) = mirror.filter(|mirror| mirror.keyed() && (self.exact.is_some() || holds_every(mirror, set.columns, self.kinds.len()))) else {
            viewer_runtime::view_record::add("map views", "sets_withheld", 1);
            return (not_declared(TableId::ALL[table]), None);
        };
        let mut held = lock(&set.made);
        let view = match &held.built {
            Some((stamp, view)) if *stamp == held.stamp => view.clone(),
            _ => match catch_unwind(AssertUnwindSafe(|| {
                let from = (mirror, self.kinds.as_slice(), self.gap || mirror.in_gap());
                if let (Some(source), Some(hour), false) = (&self.exact, mirror.head(), from.2) {
                    held.make_from(table, set.columns, from, order, made, |held, previous, since, order| {
                        held.spare = None;
                        let why = since.err().unwrap_or(if previous.is_some() { Whole::Cost } else { Whole::First });
                        drop(previous);
                        let (view, built) = source.decoded(hour, set.columns, None, order);
                        (view, built, Make::Whole(why))
                    })
                } else {
                    held.make(table, set.columns, from, order, made)
                }
            })) {
                Ok(view) => view,
                Err(payload) => held.failed_make(table, &*payload),
            },
        };
        let failure = held.built_failure.clone();
        let sampled = if matches!(view.held(), TableView::Complete(_)) && !self.gap && !mirror.in_gap() { mirror.sampled() } else { None };
        (view.with_sampled_at(sampled), failure)
    }

    fn bytes(&self) -> usize {
        self.views.iter().map(|held| lock(&held.made).built.as_ref().map_or(0, |(_, view)| view_bytes(view))).sum()
    }
}

fn stands_at(mirror: &Mirror, hour: Hour) -> bool {
    mirror.in_gap() || mirror.head() == Some(hour)
}

fn holds_every(mirror: &Mirror, columns: u64, width: usize) -> bool {
    (0..width.min(mirror.width())).filter(|&column| column >= 64 || columns & (1 << column) != 0).all(|column| mirror.holds(column))
}

fn view_bytes(view: &SharedView) -> usize {
    if view.is_not_declared() {
        return 0;
    }
    match view.held() {
        TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.heap_bytes(),
        TableView::Unavailable { reason } => reason.capacity(),
        TableView::NotRecorded { .. } => 0,
    }
}

#[derive(Default)]
struct Telling {
    pending: Told,
    unread: Told,
}

struct Reading {
    id: u64,
    older: Option<VecDeque<(Arc<DecodedFrame>, u64)>>,
    name: &'static str,
    posted: Option<Instant>,
    published_at: Option<Instant>,
    costed: bool,
    cost_ns: u64,
    marks: Marks,
    tables: Vec<(usize, u64)>,
    declared: Vec<Need>,
    keyed: Vec<Keyed>,
    frame_keys: Vec<(usize, Arc<BTreeSet<u64>>)>,
    keyed_views: Vec<(usize, Version, u64, Arc<BTreeSet<u64>>, SharedView, Option<view::HeadFailure>)>,
    frame: viewer_runtime::landed::PublishedKeyed<(Hour, u64, u64), (Arc<DecodedFrame>, Arc<[view::HeadFailure]>, (Hour, u64))>,
    schema_ver: u64,
    published: u64,
    told: Option<Telling>,
    arriving: Vec<TableId>,
    open: bool,
    pace: Pace,
    due_at: Option<Instant>,
    ready: bool,
}

#[derive(Default, Clone, Copy)]
struct Marks {
    owed: u64,
    wait_ns: u64,
    build_ns: u64,
    builds: u64,
    last: u64,
}

impl Reading {
    fn frame(&self) -> Option<&Arc<DecodedFrame>> {
        self.frame.as_ref().map(|(frame, ..)| frame)
    }

    fn failures(&self) -> Option<&Arc<[view::HeadFailure]>> {
        self.frame.as_ref().map(|(_, failures, _)| failures)
    }

    fn stands(&self) -> Option<(Hour, u64)> {
        self.frame.as_ref().map(|(_, _, stands)| *stands)
    }

    fn due(&self, now: Instant, epoch: u64) -> bool {
        let anew = self.frame().is_none() || self.stands().is_none_or(|(_, at)| at != epoch);
        match (self.pace, self.due_at) {
            (Pace::Ready, _) => self.ready || anew,
            (_, Some(due)) => now >= due || anew,
            (_, None) => true,
        }
    }

    fn keys_moved(&self) -> bool {
        self.keyed.iter().any(|keyed| {
            !self
                .keyed_views
                .iter()
                .any(|(table, _, version, keys, ..)| *table == keyed.table && *version == keyed.version && Arc::ptr_eq(keys, &keyed.keys))
        })
    }
}

struct Job {
    table: usize,
    columns: u64,
    waits: std::ops::Range<usize>,
}

struct Posted {
    mirrors: Arc<Mirrors>,
    hour: Hour,
    branch: BranchId,
    epoch: u64,
    deferred: Option<BTreeSet<u64>>,
}

#[derive(Default)]
struct Jobs {
    queue: Vec<Job>,
    waiting: Vec<usize>,
    next: usize,
    running: usize,
    left: Vec<usize>,
    from: Option<Posted>,
    made: Made,
    failed: Option<Box<dyn std::any::Any + Send>>,
    stop: bool,
}

impl Jobs {
    fn built(&mut self, at: usize, made: Made, failed: Option<Box<dyn std::any::Any + Send>>) -> Vec<usize> {
        let waits = self.queue[at].waits.clone();
        self.made.merge(made);
        let mut ready = Vec::new();
        for held in waits {
            let place = self.waiting[held];
            self.left[place] -= 1;
            if self.left[place] == 0 {
                ready.push(place);
            }
        }
        if self.failed.is_none() {
            self.failed = failed;
        }
        ready
    }
}

struct Shared {
    kept: RwLock<Vec<Option<Kept>>>,
    jobs: Mutex<Jobs>,
    work: Condvar,
    done: Condvar,
    readers: Mutex<Vec<Reading>>,
    made: Condvar,
    epoch: AtomicU64,
    display: std::sync::atomic::AtomicI64,
    next: AtomicU64,
    kinds: Vec<Vec<Kind>>,
    written: Vec<playhead::pages::TableWritten>,
    unwritten: Vec<std::sync::OnceLock<SharedView>>,
    entry: Arc<Entry>,
    script_values: Arc<ScriptValues>,
    exact_columns: Mutex<BTreeMap<usize, Arc<ExactColumns>>>,
    script_values_view: Mutex<Option<((Hour, BranchId, u64), SharedView)>>,
    learned: Mutex<BTreeMap<(&'static str, usize), (Option<Hour>, u64)>>,
    asked: Mutex<Vec<Asked>>,
    demand: AtomicU64,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    on_publish: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    whole_progress: Arc<std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    chosen: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    worker_built: Mutex<Option<(bool, Box<dyn FnOnce() + Send>)>>,
}

impl Shared {
    fn values_at(&self, at: (Hour, BranchId, u64)) -> SharedView {
        let mut view = lock(&self.script_values_view);
        if let Some((_, view)) = view.as_ref().filter(|(held, _)| *held == at) {
            return view.clone();
        }
        let next = view.as_ref().map_or_else(
            || SharedView::from(TableView::Unavailable { reason: format!("{SCRIPT_VALUES:?} is read by key") }).with_keyed_numbers(self.script_values.clone()),
            |(_, previous)| previous.clone(),
        ).with_stamp(Stamp::new(at.0));
        *view = Some((at, next.clone()));
        next
    }

    fn held(&self) -> Vec<(TableId, usize)> {
        let script = SCRIPT_VALUES.index();
        let mut held: Vec<_> = reading(self).iter().enumerate().filter(|(table, _)| *table != script).filter_map(|(table, held)| held.as_ref().map(|held| (TableId::ALL[table], held.bytes()))).collect();
        let mut bytes = self.script_values.columns.bytes();
        let mut seen = BTreeSet::new();
        let mut count = |view: &SharedView| {
            if seen.insert(view.as_ptr()) {
                bytes += view_bytes(view);
            }
        };
        for reader in lock(&self.readers).iter() {
            for frame in reader.frame().into_iter().chain(reader.older.iter().flatten().map(|(frame, _)| frame)) {
                count(&frame.tables[script]);
            }
            for (_, _, _, _, view, _) in reader.keyed_views.iter().filter(|(table, ..)| *table == script) {
                count(view);
            }
        }
        if bytes > 0 {
            held.push((SCRIPT_VALUES, bytes));
        }
        held
    }

    fn note_demand(&self) {
        self.demand.fetch_add(1, Ordering::Release);
        if let Some(wake) = self.whole_progress.get() {
            wake();
        }
        let wake = lock(&self.wake).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    fn notify_progress(&self) {
        if let Some(wake) = self.whole_progress.get() {
            wake();
        }
        let wake = lock(&self.on_publish).clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

enum Asked {
    Open { id: u64, name: &'static str, needs: Vec<Need>, pace: Pace },
    Declare { id: u64, needs: Vec<Need> },
    Close { id: u64 },
}

fn reading(shared: &Shared) -> RwLockReadGuard<'_, Vec<Option<Kept>>> {
    shared.kept.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn writing(shared: &Shared) -> RwLockWriteGuard<'_, Vec<Option<Kept>>> {
    shared.kept.write().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Record {
    steps: u64,
    jobs: u64,
    made_ms: Vec<f64>,
    tables: BTreeMap<usize, (u64, u64, u64)>,
    whole: BTreeMap<&'static str, u64>,
    how: (u64, u64, u64),
    on: BTreeSet<String>,
}

pub struct Store {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    order: Vec<(u128, u32)>,
    record: Record,
}

impl Store {
    pub fn new(entry: &Arc<Entry>) -> Self {
        let written = entry.pages.written_tables(&crate::playhead_host::timeline_runs(entry), TABLE_COUNT);
        let script_values = Arc::new(ScriptValues::new(entry, written[SCRIPT_VALUES.index()].clone()));
        let shared = Shared {
            kept: RwLock::new((0..TABLE_COUNT).map(|_| None).collect()),
            jobs: Mutex::default(),
            work: Condvar::new(),
            done: Condvar::new(),
            readers: Mutex::default(),
            made: Condvar::new(),
            epoch: AtomicU64::new(0),
            display: std::sync::atomic::AtomicI64::new(NOT_SHOWN),
            next: AtomicU64::new(0),
            kinds: entry.kinds().to_vec(),
            written,
            unwritten: (0..TABLE_COUNT).map(|_| std::sync::OnceLock::new()).collect(),
            entry: Arc::clone(entry),
            script_values,
            exact_columns: Mutex::default(),
            script_values_view: Mutex::default(),
            learned: Mutex::default(),
            asked: Mutex::default(),
            demand: AtomicU64::new(0),
            wake: Mutex::default(),
            on_publish: Arc::default(),
            whole_progress: Arc::default(),
            #[cfg(test)]
            chosen: Mutex::default(),
            #[cfg(test)]
            worker_built: Mutex::default(),
        };
        Self { shared: Arc::new(shared), threads: Vec::new(), order: Vec::new(), record: Record::default() }
    }

    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.shared.wake) = Some(wake);
    }

    pub(super) fn set_whole_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.whole_progress.set(wake);
    }

    pub fn use_columns(&self, table: TableId) {
        let at = table.index();
        let mut kept = writing(&self.shared);
        let mut columns = lock(&self.shared.exact_columns);
        let std::collections::btree_map::Entry::Vacant(slot) = columns.entry(at) else { return };
        let source = slot.insert(Arc::new(ExactColumns::new(&self.shared.entry, table, self.shared.written[at].clone())));
        if let Some(held) = &mut kept[at] {
            held.exact = Some(Arc::clone(source));
            held.arriving();
        }
        drop(columns);
        drop(kept);
        self.shared.note_demand();
    }

    pub fn demand(&self) -> u64 {
        self.shared.demand.load(Ordering::Acquire)
    }

    pub fn has_readers(&self) -> bool {
        lock(&self.shared.readers).iter().any(|reading| reading.open) || !lock(&self.shared.asked).is_empty()
    }

    pub fn asking(&self) -> bool {
        !lock(&self.shared.asked).is_empty()
    }

    pub fn declared(&self) -> Vec<playhead::Declared> {
        self.declarations().into_iter().map(|(read, _)| read).collect()
    }

    pub(super) fn declarations(&self) -> Vec<(playhead::Declared, bool)> {
        let mut wanted: BTreeMap<usize, u64> = reading(&self.shared).iter().enumerate().filter_map(|(table, kept)| Some((table, kept.as_ref()?.columns))).collect();
        for asked in lock(&self.shared.asked).iter() {
            if let Asked::Open { needs, .. } | Asked::Declare { needs, .. } = asked {
                for need in needs {
                    *wanted.entry(need.table.index()).or_default() |= need.columns;
                }
            }
        }
        let exact = lock(&self.shared.exact_columns);
        wanted
            .into_iter()
            .filter(|&(table, _)| table != SCRIPT_VALUES.index() && matches!(self.shared.written.get(table), Some(playhead::pages::TableWritten::Written)))
            .filter_map(|(table, mask)| {
                let width = self.shared.kinds.get(table)?.len();
                let columns = (0..width).filter(|&column| column >= 64 || mask & (1 << column) != 0).collect();
                Some((playhead::Declared { table, columns, keyed: true, passing: false, hourly: false }, exact.contains_key(&table)))
            })
            .collect()
    }

    pub fn reader(&self, name: &'static str, needs: &[Need]) -> Reader {
        open(&self.shared, name, needs, false, Pace::Step, false)
    }

    pub fn handle(&self) -> Views {
        Views { shared: Arc::clone(&self.shared) }
    }

    pub fn noting<'a>(&'a self, layers: &'a [bool]) -> Noting<'a> {
        Noting { kept: writing(&self.shared), layers }
    }

    pub(super) fn accept_asks(&self) {
        let asked = std::mem::take(&mut *lock(&self.shared.asked));
        for asked in asked {
            match asked {
                Asked::Open { id, name, needs, pace } => {
                    register(&self.shared, id, name, false, pace, true);
                    declare(&self.shared, id, &needs, false);
                }
                Asked::Declare { id, needs } => declare(&self.shared, id, &needs, false),
                Asked::Close { id } => close(&self.shared, id),
            }
        }
    }

    pub fn post(&mut self, mirrors: &Arc<Mirrors>, hour: Hour, branch: BranchId, epoch: u64, whole: bool) {
        self.settled();
        {
            let mut jobs = lock(&self.shared.jobs);
            jobs.from = Some(Posted { mirrors: Arc::clone(mirrors), hour, branch, epoch, deferred: Some(BTreeSet::new()) });
            jobs.running += 1;
        }
        self.accept_asks();
        let kept = reading(&self.shared);
        let mut readers = lock(&self.shared.readers);
        self.shared.epoch.store(epoch, Ordering::Release);
        readers.retain(|reading| reading.open);
        if let Some(at) = (0..readers.len()).filter(|&at| readers[at].posted.is_some() && readers[at].published_at.is_some()).max_by_key(|&at| readers[at].published_at) {
            readers[at].marks.last += 1;
        }
        let now = Instant::now();
        for reading in readers.iter_mut() {
            (reading.posted, reading.published_at, reading.costed) = (None, None, false);
        }
        let mut places: Vec<usize> =
            (0..readers.len()).filter(|&at| readers[at].open && (!readers[at].tables.is_empty() || readers[at].declared.is_empty()) && (whole || readers[at].due(now, epoch))).collect();
        places.sort_by_key(|&at| (std::cmp::Reverse(readers[at].cost_ns), at));
        let mut wanted: Vec<(usize, usize, u64, u64)> = Vec::new();
        for (order, &place) in places.iter().enumerate() {
            for &(table, columns) in &readers[place].tables {
                if table != SCRIPT_VALUES.index() && matches!(self.shared.written[table], playhead::pages::TableWritten::Written)
                    && kept[table].as_ref().is_none_or(|held| held.stale(columns))
                    && !wanted.iter().any(|&(_, held, set, _)| (held, set) == (table, columns))
                {
                    let cost = kept[table].as_ref().map_or(FIRST_MAKE_NS, |held| held.costing(columns));
                    wanted.push((order, table, columns, cost));
                }
            }
        }
        wanted.sort_by_key(|&(order, _, _, cost)| (order, std::cmp::Reverse(cost)));
        let (mut queue, mut waiting, mut left) = (Vec::with_capacity(wanted.len()), Vec::new(), vec![0_usize; readers.len()]);
        for (_, table, columns, _) in wanted {
            let from = waiting.len();
            for &place in &places {
                if readers[place].tables.contains(&(table, columns)) {
                    waiting.push(place);
                    left[place] += 1;
                }
            }
            queue.push(Job { table, columns, waits: from..waiting.len() });
        }
        let ready: Vec<usize> = places.iter().copied().filter(|&place| left[place] == 0 && (readers[place].stands() != Some((hour, epoch)) || readers[place].keys_moved())).collect();
        for &place in &places {
            if left[place] > 0 || ready.contains(&place) {
                readers[place].posted = Some(now);
                readers[place].marks.owed += 1;
            }
        }
        drop(readers);
        #[cfg(test)]
        {
            let chosen = lock(&self.shared.chosen).take();
            if let Some(chosen) = chosen {
                chosen();
            }
        }
        let owed = queue.len();
        {
            let mut jobs = lock(&self.shared.jobs);
            (jobs.queue, jobs.waiting, jobs.left, jobs.next) = (queue, waiting, left, 0);
            jobs.made = Made::since(viewer_runtime::view_record::on().then(Instant::now));
        }
        if !ready.is_empty() {
            publish(&self.shared, &kept, mirrors, &ready, (hour, branch, epoch), false);
        }
        drop(kept);
        if owed > 0 {
            self.record.steps += 1;
            self.record.jobs += owed as u64;
            self.spawn();
            self.shared.work.notify_all();
        }
        posted_work_done(&self.shared);
    }

    pub fn settled(&mut self) {
        self.wait_made(true);
    }

    pub fn made(&mut self) {
        self.wait_made(false);
    }

    fn wait_made(&mut self, release: bool) {
        loop {
            if make_next(&self.shared, &mut self.order) {
                continue;
            }
            let mut jobs = lock(&self.shared.jobs);
            if jobs.running > 0 {
                let _ = self.shared.done.wait_timeout(jobs, Duration::from_micros(100)).unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            let (made, failed) = (std::mem::take(&mut jobs.made), jobs.failed.take());
            if release {
                jobs.from = None;
            }
            jobs.next = 0;
            jobs.queue.clear();
            jobs.waiting.clear();
            jobs.left.clear();
            drop(jobs);
            self.fold(made);
            if let Some(payload) = failed {
                std::panic::resume_unwind(payload);
            }
            return;
        }
    }

    fn spawn(&mut self) {
        if !self.threads.is_empty() {
            return;
        }
        for _ in 0..MAKE_THREADS {
            let shared = Arc::clone(&self.shared);
            if let Ok(thread) = std::thread::Builder::new().name(MAKE_THREAD.to_owned()).spawn(move || make_views(&shared)) {
                self.threads.push(thread);
            }
        }
    }

    fn fold(&mut self, made: Made) {
        if made.builds.is_empty() {
            return;
        }
        let mut ended = 0_u64;
        for &(table, ns, rows, how, at) in &made.builds {
            let held = self.record.tables.entry(table).or_default();
            (held.0, held.1, held.2) = (held.0 + 1, held.1 + ns, held.2 + rows as u64);
            ended = ended.max(at + ns);
            match how {
                Make::InPlace => self.record.how.0 += 1,
                Make::Copied => self.record.how.1 += 1,
                Make::Whole(why) => {
                    self.record.how.2 += 1;
                    *self.record.whole.entry(why.name()).or_default() += 1;
                }
                Make::Keyed => {}
            }
        }
        if made.origin.is_some() {
            self.record.made_ms.push(ended as f64 / 1e6);
        }
        if let Some(on) = made.on {
            self.record.on.insert(on);
        }
    }

    pub fn held(&self) -> Vec<(TableId, usize)> {
        self.shared.held()
    }

    pub fn record(&self, distribution: fn(Vec<f64>) -> serde_json::Value) -> serde_json::Value {
        use serde_json::json;
        let readers: Vec<serde_json::Value> = lock(&self.shared.readers)
            .iter()
            .map(|reading| {
                json!({
                    "maker": reading.name,
                    "tables": reading.tables.len(),
                    "owed": reading.marks.owed,
                    "wait_ms": reading.marks.wait_ns as f64 / 1e6 / reading.marks.owed.max(1) as f64,
                    "build_ms": reading.marks.build_ns as f64 / 1e6 / reading.marks.builds.max(1) as f64,
                    "builds": reading.marks.builds,
                    "last": reading.marks.last,
                })
            })
            .collect();
        let learned: Vec<serde_json::Value> = lock(&self.shared.learned)
            .iter()
            .map(|(&(name, table), &(hour, times))| json!({"maker": name, "table": format!("{:?}", TableId::ALL[table]), "hour": hour, "times": times}))
            .collect();
        let mut tables: Vec<(usize, (u64, u64, u64))> = self.record.tables.iter().map(|(&table, &held)| (table, held)).collect();
        tables.sort_by_key(|&(_, (_, ns, _))| std::cmp::Reverse(ns));
        json!({
            "steps": self.record.steps,
            "jobs": self.record.jobs,
            "views_made_ms": distribution(self.record.made_ms.clone()),
            "made_on": self.record.on.iter().cloned().collect::<Vec<String>>(),
            "readers": readers,
            "learned": learned,
            "in_place": self.record.how.0, "copied": self.record.how.1, "whole": self.record.how.2,
            "whole_why": self.record.whole.iter().map(|(&why, &count)| (why.to_owned(), json!(count))).collect::<serde_json::Map<String, serde_json::Value>>(),
            "tables": tables.iter().map(|&(table, (makes, ns, rows))| json!({"table": format!("{:?}", TableId::ALL[table]), "makes": makes, "make_ms": ns as f64 / 1e6, "rows": rows})).collect::<Vec<serde_json::Value>>(),
            "scope": "The views the map side made from its own mirrors inside its step: steps that owed one, jobs posted, views_made_ms from a step's post to the last of its views, and per table its makes, their milliseconds and the rows made.",
        })
    }
}

#[derive(Clone)]
pub struct Views {
    shared: Arc<Shared>,
}

impl Views {
    pub fn show_through(&self, hour: Option<Hour>) {
        self.shared.display.store(hour.map_or(NOT_SHOWN, i64::from), Ordering::Release);
    }

    pub fn shown_through(&self) -> Option<Hour> {
        let hour = self.shared.display.load(Ordering::Acquire);
        (hour != NOT_SHOWN).then_some(hour as Hour)
    }

    pub fn seek(&self) -> u64 {
        self.shared.epoch.load(Ordering::Acquire)
    }

    pub fn entry(&self) -> &Arc<Entry> {
        &self.shared.entry
    }

    pub fn reader(&self, name: &'static str, needs: &[Need]) -> Reader {
        open(&self.shared, name, needs, false, Pace::Step, false)
    }

    pub fn reader_at(&self, name: &'static str, needs: &[Need], pace: Pace) -> Reader {
        open(&self.shared, name, needs, false, pace, false)
    }

    pub fn reader_through(&self, name: &'static str, needs: &[Need], pace: Pace) -> Reader {
        open(&self.shared, name, needs, false, pace, true)
    }

    pub fn reader_later(&self, name: &'static str, needs: &[Need], pace: Pace) -> Reader {
        let id = self.shared.next.fetch_add(1, Ordering::AcqRel);
        lock(&self.shared.asked).push(Asked::Open { id, name, needs: needs.to_vec(), pace });
        self.shared.note_demand();
        Reader { shared: Arc::clone(&self.shared), id, later: true }
    }

    pub fn telling(&self, name: &'static str, needs: &[Need]) -> Reader {
        open(&self.shared, name, needs, true, Pace::Step, false)
    }

    pub fn holds(&self, reader: &Reader) -> bool {
        Arc::ptr_eq(&self.shared, &reader.shared)
    }


    pub fn set_on_publish(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.shared.on_publish) = Some(wake);
    }

    pub fn notify_progress(&self) {
        self.shared.notify_progress();
    }

    pub fn progress_notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let progress = Arc::clone(&self.shared.whole_progress);
        let on_publish = Arc::clone(&self.shared.on_publish);
        Arc::new(move || {
            if let Some(wake) = progress.get() {
                wake();
            }
            let wake = lock(&on_publish).clone();
            if let Some(wake) = wake {
                wake();
            }
        })
    }

    pub fn demand(&self) -> u64 {
        self.shared.demand.load(Ordering::Acquire)
    }

    pub fn held(&self) -> Vec<(TableId, usize)> {
        self.shared.held()
    }
}

fn open(shared: &Arc<Shared>, name: &'static str, needs: &[Need], told: bool, pace: Pace, keeps_older: bool) -> Reader {
    let id = shared.next.fetch_add(1, Ordering::AcqRel);
    register(shared, id, name, told, pace, keeps_older);
    let reader = Reader { shared: Arc::clone(shared), id, later: false };
    reader.declared(needs);
    reader
}

fn register(shared: &Shared, id: u64, name: &'static str, told: bool, pace: Pace, keeps_older: bool) {
    let mut readers = lock(&shared.readers);
    readers.push(Reading {
        id,
        older: keeps_older.then(VecDeque::new),
        name,
        posted: None,
        published_at: None,
        costed: false,
        cost_ns: 0,
        marks: Marks::default(),
        tables: Vec::new(),
        declared: Vec::new(),
        keyed: Vec::new(),
        frame_keys: Vec::new(),
        keyed_views: Vec::new(),
        frame: Default::default(),
        schema_ver: 0,
        published: 0,
        told: told.then(Telling::default),
        arriving: Vec::new(),
        open: true,
        pace,
        due_at: None,
        ready: true,
    });
}

impl Drop for Store {
    fn drop(&mut self) {
        lock(&self.shared.jobs).stop = true;
        self.shared.work.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        {
            let mut jobs = lock(&self.shared.jobs);
            *jobs = Jobs { stop: true, running: jobs.running, ..Jobs::default() };
        }
        writing(&self.shared).iter_mut().for_each(|kept| *kept = None);
        lock(&self.shared.readers).clear();
        self.shared.script_values.columns.release();
    }
}

pub struct Noting<'a> {
    kept: RwLockWriteGuard<'a, Vec<Option<Kept>>>,
    layers: &'a [bool],
}

impl super::mirror::Notes for Noting<'_> {
    fn awake(&self, table: usize) -> bool {
        self.layers.get(table).copied().unwrap_or(false) || self.kept.get(table).is_some_and(Option::is_some)
    }

    fn gap(&mut self) {
        noted_gap(&mut self.kept);
    }

    fn applied(&mut self, table: usize, changes: &Changes, moved: bool) {
        noted_apply(&mut self.kept, table, changes, moved);
    }
}

fn noted_gap(kept: &mut [Option<Kept>]) {
    for kept in kept.iter_mut().flatten() {
        if kept.gap {
            continue;
        }
        kept.gap = true;
        for held in &mut kept.views {
            let held = mine(&mut held.made);
            held.stamp += 1;
            held.touched = Err(Whole::Gap);
        }
    }
}

fn noted_apply(kept: &mut [Option<Kept>], table: usize, changes: &Changes, moved: bool) {
    {
        let Some(Some(kept)) = kept.get_mut(table) else { return };
        kept.gap = false;
        let moved = moved || (kept.exact.is_some() && changes.read_rows_changed());
        let membership = changes.reset() || !changes.deleted().is_empty() || !changes.inserted().is_empty() || moved;
        let written = changes.columns().iter().filter(|written| !written.is_empty()).fold(0_u64, |held, written| held | if written.column() < 64 { 1 << written.column() } else { view::EVERY });
        for held in &mut kept.views {
            let columns = held.columns;
            let held = mine(&mut held.made);
            let rows = membership || written & columns != 0;
            if rows {
                held.stamp += 1;
            }
            if rows || changes.renumber().is_some() {
                note(&mut held.touched, changes, moved, columns, held.bound);
            }
        }
    }
}

pub struct Stepped {
    entry: Arc<Entry>,
    reader: playhead::Reader,
    consumer: playhead::Consumer,
    mirrors: Mirrors,
    kept: Vec<Option<Kept>>,
    tables: Vec<usize>,
    absent: Vec<(TableId, playhead::pages::TableWritten)>,
    hour: Option<(Hour, u64)>,
    order: Vec<(u128, u32)>,
    steps: Vec<Arc<playhead::Step>>,
}

impl Stepped {
    pub fn new(entry: &Arc<Entry>, tables: &[TableId]) -> io::Result<Self> {
        let (held, missing): (Vec<TableId>, Vec<TableId>) = tables.iter().partition(|&&table| super::details::held_at_all(entry, table));
        let absent = if missing.is_empty() {
            Vec::new()
        } else {
            let written = entry.pages().written_tables(&super::timeline_runs(entry), TABLE_COUNT);
            missing.into_iter().map(|table| (table, written[table.index()].clone())).collect()
        };
        let mut kept: Vec<Option<Kept>> = (0..TABLE_COUNT).map(|_| None).collect();
        let mut declared = Vec::with_capacity(held.len());
        for table in &held {
            let columns = entry.pages().kinds(table.index()).ok_or_else(|| io::Error::other(format!("No table {table:?} in the schema")))?;
            declared.push(playhead::Declared { table: table.index(), columns: (0..columns.len()).collect(), keyed: true, passing: false, hourly: false });
            let mut set = Kept::new(columns.to_vec());
            set.at(u64::MAX);
            kept[table.index()] = Some(set);
        }
        let playhead = entry.playhead();
        let reader = playhead.declare(declared, Some(super::mirror::rebuild(Arc::clone(&entry.pages))))?;
        let consumer = playhead.consumer(reader);
        Ok(Self {
            entry: Arc::clone(entry),
            reader,
            consumer,
            mirrors: Mirrors::default(),
            kept,
            tables: held.iter().map(|table| table.index()).collect(),
            absent,
            hour: None,
            order: Vec::new(),
            steps: Vec::new(),
        })
    }

    pub fn epoch(&self) -> u64 {
        self.entry.playhead().epoch()
    }

    pub fn take_through(&mut self, hour: Hour, epoch: u64) -> Option<Hour> {
        if epoch < self.epoch() {
            return None;
        }
        self.consumer.take(hour, &mut self.steps);
        self.steps.retain(|step| step.epoch() >= epoch);
        let ready = self.steps.iter().take_while(|step| step.epoch() == epoch && step.last() <= hour).count();
        let capture = self.entry.capture.get().copied();
        let mut tables = Vec::new();
        let steps: Vec<Arc<playhead::Step>> = self.steps.drain(..ready).collect();
        for step in &steps {
            self.hour = Some((step.last(), epoch));
            for changes in step.tables() {
                if changes.reset()
                    && let Some(state) = self.consumer.state(step, changes.table())
                    && let Ok(mut rebuilt) = state.downcast::<Mirror>()
                {
                    rebuilt.set_capture(capture);
                    self.mirrors.insert(rebuilt);
                }
            }
            let mut notes = SteppedNotes { kept: &mut self.kept };
            self.mirrors.apply_timed(step, &mut tables, &mut Default::default(), &mut notes);
        }
        self.hour.filter(|&(_, of)| of == epoch).map(|(hour, _)| hour)
    }

    pub fn frame(&mut self, branch: BranchId, hour: Hour) -> Option<DecodedFrame> {
        self.frame_with(branch, hour, false)
    }

    pub fn frame_past(&mut self, branch: BranchId, hour: Hour) -> Option<DecodedFrame> {
        self.frame_with(branch, hour, true)
    }

    fn frame_with(&mut self, branch: BranchId, hour: Hour, past: bool) -> Option<DecodedFrame> {
        let mirror = |table: usize| self.mirrors.get(TableId::ALL[table]);
        let answers = |table: usize| self.kept[table].as_ref().is_some_and(|kept| if past { kept.home(u64::MAX, mirror(table)) } else { kept.answers(u64::MAX, mirror(table), hour) });
        if !self.tables.iter().all(|&table| answers(table)) {
            return None;
        }
        let mut frame = DecodedFrame { branch, hour, tables: std::array::from_fn(|index| not_declared(TableId::ALL[index])), metrics: Metrics::default() };
        let mut made = Made::default();
        for &table in &self.tables {
            let kept = self.kept[table].as_ref().expect("held");
            let mirror = self.mirrors.get(TableId::ALL[table]);
            frame.tables[table] = match mirror.filter(|mirror| !stands_at(mirror, hour)) {
                Some(standing) => SharedView::from(TableView::Unavailable { reason: format!("{:?} was not read at hour {hour}: the steps reached it only at hour {:?}", TableId::ALL[table], standing.head()) }),
                None => kept.view(table, u64::MAX, mirror, &mut self.order, &mut made),
            };
        }
        for (table, state) in &self.absent {
            frame.tables[table.index()] = SharedView::from(match state {
                playhead::pages::TableWritten::Withheld(reason) | playhead::pages::TableWritten::Damaged(reason) => TableView::Unavailable { reason: reason.clone() },
                playhead::pages::TableWritten::Unwritten | playhead::pages::TableWritten::Written => match self.entry.capture.get() {
                    Some(&mode) => TableView::NotRecorded { mode },
                    None => TableView::Unavailable { reason: format!("{table:?} is not recorded here, in a recording whose capture mode the playhead was not given") },
                },
            });
        }
        Some(frame)
    }
}

impl Drop for Stepped {
    fn drop(&mut self) {
        self.entry.playhead().retire(self.reader);
    }
}

struct SteppedNotes<'a> {
    kept: &'a mut Vec<Option<Kept>>,
}

impl super::mirror::Notes for SteppedNotes<'_> {
    fn awake(&self, table: usize) -> bool {
        self.kept.get(table).is_some_and(Option::is_some)
    }

    fn gap(&mut self) {
        noted_gap(self.kept);
    }

    fn applied(&mut self, table: usize, changes: &Changes, moved: bool) {
        noted_apply(self.kept, table, changes, moved);
    }
}

fn make_next(shared: &Shared, order: &mut Vec<(u128, u32)>) -> bool {
    let kept = reading(shared);
    let (at, table, columns, mirrors, stands, origin) = {
        let mut jobs = lock(&shared.jobs);
        if jobs.next == jobs.queue.len() {
            return false;
        }
        let Some(from) = &jobs.from else { return false };
        let (mirrors, stands, origin) = (Arc::clone(&from.mirrors), (from.hour, from.branch, from.epoch), jobs.made.origin);
        let at = jobs.next;
        (jobs.next, jobs.running) = (at + 1, jobs.running + 1);
        (at, jobs.queue[at].table, jobs.queue[at].columns, mirrors, stands, origin)
    };
    let mut made = Made::since(origin);
    let failed = catch_unwind(AssertUnwindSafe(|| {
        if let Some(held) = kept[table].as_ref() {
            held.view(table, columns, mirrors.get(TableId::ALL[table]), order, &mut made);
        }
    }))
    .err();
    #[cfg(test)]
    let after = {
        let refresh = lock(&shared.worker_built).take();
        refresh.and_then(|(after, refresh)| {
            if after { Some(refresh) } else { refresh(); None }
        })
    };
    telling(shared, &kept, &mut made);
    #[cfg(test)]
    if let Some(refresh) = after {
        refresh();
    }
    let ready = lock(&shared.jobs).built(at, made, failed);
    if !ready.is_empty() {
        publish(shared, &kept, &mirrors, &ready, stands, false);
    }
    drop(kept);
    drop(mirrors);
    posted_work_done(shared);
    true
}

fn make_views(shared: &Shared) {
    let mut order: Vec<(u128, u32)> = Vec::new();
    loop {
        while make_next(shared, &mut order) {}
        let mut jobs = lock(&shared.jobs);
        while !jobs.stop && jobs.next == jobs.queue.len() {
            jobs = shared.work.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if jobs.stop {
            return;
        }
    }
}

#[derive(PartialEq, Eq)]
enum Version {
    Mirror(u64),
    Exact(Hour, u64),
    Unwritten,
}

#[derive(Clone, Copy)]
enum RowsAt<'a> {
    Mirror(Option<&'a Mirror>),
    Exact(&'a ScriptValues, Hour, u64),
}

impl RowsAt<'_> {
    fn ready(self, kept: &Kept, columns: u64, hour: Hour) -> bool {
        match self {
            Self::Mirror(mirror) => kept.answers(columns, mirror, hour),
            Self::Exact(..) => true,
        }
    }

    fn version(self, kept: &Kept) -> Version {
        match self {
            Self::Mirror(_) => Version::Mirror(kept.stamp(u64::MAX)),
            Self::Exact(_, hour, epoch) => Version::Exact(hour, epoch),
        }
    }

    fn view(self, kept: &Kept, (table, columns): (usize, u64), keys: Option<&BTreeSet<u64>>, order: &mut Vec<(u128, u32)>, made: &mut Made) -> (SharedView, Option<view::HeadFailure>) {
        match (self, keys) {
            (Self::Mirror(mirror), Some(keys)) => kept.keyed(table, mirror, keys, order, made),
            (Self::Mirror(mirror), None) => kept.view_with_failure(table, columns, mirror, order, made),
            (Self::Exact(source, hour, _), keys) => source.columns.snapshot(hour, columns, keys, order),
        }
    }
}

fn publish(shared: &Shared, kept: &[Option<Kept>], mirrors: &Mirrors, places: &[usize], (hour, branch, epoch): (Hour, BranchId, u64), republishing: bool) {
    let (mut order, mut made) = (Vec::new(), Made::default());
    let mut readers = lock(&shared.readers);
    let mut published = false;
    let rows_at = |table| if table == SCRIPT_VALUES.index() { RowsAt::Exact(&shared.script_values, hour, epoch) } else { RowsAt::Mirror(mirrors.get(TableId::ALL[table])) };
    for &place in places {
        let Some(reading) = readers.get(place).filter(|reading| reading.open) else { continue };
        let same_head = reading.stands() == Some((hour, epoch)) && reading.frame().is_some_and(|frame| frame.branch == branch);
        if republishing && same_head && !reading.keys_moved() {
            continue;
        }
        let is_written = |table: usize| matches!(shared.written[table], playhead::pages::TableWritten::Written);
        let home = |&(table, columns): &(usize, u64)| !is_written(table) || kept[table].as_ref().is_some_and(|kept| rows_at(table).ready(kept, columns, hour));
        let waiting: Vec<TableId> = reading
            .tables
            .iter()
            .copied()
            .chain(reading.keyed.iter().map(|keyed| (keyed.table, u64::MAX)))
            .filter(|set| !home(set))
            .map(|(table, _)| TableId::ALL[table])
            .collect();
        if !waiting.is_empty() {
            viewer_runtime::view_record::add("map views", "frames_withheld", 1);
            readers[place].arriving = waiting;
            continue;
        }
        let mut held: Vec<Option<(SharedView, Option<view::HeadFailure>)>> = vec![None; TABLE_COUNT];
        let not_written = |table: usize| -> (SharedView, Option<view::HeadFailure>) {
            match &shared.written[table] {
                playhead::pages::TableWritten::Damaged(reason) => {
                    let view = shared.unwritten[table]
                        .get_or_init(|| TableView::Unavailable { reason: reason.clone() }.into())
                        .clone();
                    let failure = view::HeadFailure {
                        table: TableId::ALL[table],
                        kind: view::HeadFailureKind::BuildError(reason.clone()),
                    };
                    (view, Some(failure))
                }
                playhead::pages::TableWritten::Withheld(reason) => {
                    let view = shared.unwritten[table]
                        .get_or_init(|| TableView::Unavailable { reason: reason.clone() }.into())
                        .clone();
                    (view, None)
                }
                playhead::pages::TableWritten::Unwritten | playhead::pages::TableWritten::Written => {
                    let view = shared.unwritten[table]
                        .get_or_init(|| {
                            match shared.entry.capture.get() {
                                Some(&mode) => TableView::NotRecorded { mode },
                                None => TableView::Unavailable { reason: format!("{:?} is not recorded here, in a recording whose capture mode the playhead was not given", TableId::ALL[table]) },
                            }
                            .into()
                        })
                        .clone();
                    (view, None)
                }
            }
        };
        for &(table, columns) in &reading.tables {
            if !is_written(table) {
                held[table] = Some(not_written(table));
            } else if let Some(kept) = kept[table].as_ref() {
                held[table] = Some(rows_at(table).view(kept, (table, columns), None, &mut order, &mut made));
            }
        }
        let (id, keyed) = (reading.id, reading.keyed.clone());
        let before = std::mem::take(&mut readers[place].keyed_views);
        let (mut views, mut keys) = (Vec::with_capacity(keyed.len()), Vec::with_capacity(keyed.len()));
        for keyed in &keyed {
            let kept = kept[keyed.table].as_ref();
            if is_written(keyed.table) && kept.is_none() {
                continue;
            }
            let rows = rows_at(keyed.table);
            let stamp = kept.map_or(Version::Unwritten, |kept| rows.version(kept));
            let was = before.iter().find(|(table, ..)| *table == keyed.table);
            let (view, failure) = match (was, kept) {
                (Some((_, shown, version, _, view, failure)), _) if *shown == stamp && *version == keyed.version => (view.clone(), failure.clone()),
                (_, Some(kept)) => {
                    let tells = kept.tells(u64::MAX);
                    let (view, failure) = rows.view(kept, (keyed.table, u64::MAX), Some(&keyed.keys), &mut order, &mut made);
                    let asked: Vec<(u128, u64)> = order.iter().map(|&(key, _)| key).filter(|&key| was.is_none_or(|(_, _, _, keys, ..)| !keys.contains(&prefix(key)))).map(|key| (key, view::EVERY)).collect();
                    if tells && !asked.is_empty() {
                        made.told_reader.push((id, keyed.table, asked));
                    }
                    (view, failure)
                }
                _ => not_written(keyed.table),
            };
            held[keyed.table] = Some((view.clone(), failure.clone()));
            keys.push((keyed.table, Arc::clone(&keyed.keys)));
            views.push((keyed.table, stamp, keyed.version, Arc::clone(&keyed.keys), view, failure));
        }
        let script_changed = !same_head && held[SCRIPT_VALUES.index()].is_some() && is_written(SCRIPT_VALUES.index());
        let mut failures: Vec<view::HeadFailure> = Vec::new();
        let tables = std::array::from_fn(|table| match held[table].take() {
            Some((view, failure)) => {
                if let Some(failure) = failure {
                    failures.push(failure);
                }
                view
            }
            None if table == SCRIPT_VALUES.index() => shared.values_at((hour, branch, epoch)),
            None => not_declared(TableId::ALL[table]),
        });
        let frame = DecodedFrame { branch, hour, tables, metrics: Metrics::default() };
        told(&mut readers, kept, &mut made);
        let reading = &mut readers[place];
        if let (Some(older), Some(was)) = (reading.older.as_mut(), reading.frame.take()) {
            let (was, _, (_, seek)) = was.into_inner();
            older.push_front((was, seek));
            older.truncate(FRAMES_KEPT);
        }
        reading.frame.publish((hour, epoch, reading.schema_ver), (Arc::new(frame), Arc::from(failures), (hour, epoch)));
        published = true;
        reading.published += 1;
        reading.due_at = reading.pace.interval().map(|interval| Instant::now() + interval);
        reading.ready = false;
        if reading.published_at.is_none() {
            let made_at = Instant::now();
            reading.published_at = Some(made_at);
            if let Some(posted) = reading.posted {
                reading.marks.wait_ns += made_at.saturating_duration_since(posted).as_nanos() as u64;
            }
        }
        (reading.frame_keys, reading.keyed_views) = (keys, views);
        reading.arriving.clear();
        if let Some(telling) = &mut reading.told {
            if script_changed {
                telling.pending.insert(SCRIPT_VALUES, None);
            }
            for (table, rows) in std::mem::take(&mut telling.pending) {
                tell(&mut telling.unread, table, rows.as_ref());
            }
        }
    }
    drop(readers);
    shared.made.notify_all();
    if published {
        shared.entry.playhead().wake_waiters();
        shared.notify_progress();
    }
}

fn republish(shared: &Shared, id: u64) {
    let (mirrors, stands) = {
        let mut jobs = lock(&shared.jobs);
        let Some(from) = &mut jobs.from else { return };
        if let Some(deferred) = &mut from.deferred {
            deferred.insert(id);
            return;
        }
        let held = (Arc::clone(&from.mirrors), (from.hour, from.branch, from.epoch));
        jobs.running += 1;
        held
    };
    refresh_readers(shared, mirrors, stands, [id]);
}

fn posted_work_done(shared: &Shared) {
    let refresh = {
        let mut jobs = lock(&shared.jobs);
        jobs.running -= 1;
        let refresh = if jobs.running == 0 && jobs.next == jobs.queue.len() {
            jobs.from.as_mut().and_then(|from| {
                let readers = from.deferred.take().filter(|readers| !readers.is_empty())?;
                Some((Arc::clone(&from.mirrors), (from.hour, from.branch, from.epoch), readers))
            })
        } else {
            None
        };
        if refresh.is_some() {
            jobs.running += 1;
        }
        refresh
    };
    if let Some((mirrors, stands, readers)) = refresh {
        refresh_readers(shared, mirrors, stands, readers);
    } else {
        shared.done.notify_all();
    }
}

fn refresh_readers(shared: &Shared, mirrors: Arc<Mirrors>, stands: (Hour, BranchId, u64), ids: impl IntoIterator<Item = u64>) {
    let kept = reading(shared);
    let places: Vec<_> = {
        let readers = lock(&shared.readers);
        ids.into_iter().filter_map(|id| Reader::place(&readers, id)).collect()
    };
    if !places.is_empty() {
        publish(shared, &kept, &mirrors, &places, stands, true);
    }
    drop(kept);
    drop(mirrors);
    lock(&shared.jobs).running -= 1;
    shared.done.notify_all();
}

fn telling(shared: &Shared, kept: &[Option<Kept>], made: &mut Made) {
    if made.told.len() == made.told_from && made.told_reader.len() == made.told_reader_from {
        return;
    }
    let mut readers = lock(&shared.readers);
    told(&mut readers, kept, made);
}

fn told(readers: &mut [Reading], kept: &[Option<Kept>], made: &mut Made) {
    for &(table, columns, ref rows) in &made.told[made.told_from..] {
        let whole = kept[table].as_ref().is_some_and(|kept| over(columns, kept.kinds.len()) == over(u64::MAX, kept.kinds.len()));
        for reading in readers.iter_mut().filter(|reading| reading.told.is_some()) {
            let reads = reading.tables.contains(&(table, columns)) || (whole && reading.keyed.iter().any(|keyed| keyed.table == table));
            if let (true, Some(telling)) = (reads, &mut reading.told) {
                tell(&mut telling.pending, TableId::ALL[table], rows.as_ref().map(|rows| rows.iter().map(|(key, columns)| (key, columns))));
            }
        }
    }
    made.told_from = made.told.len();
    for &(id, table, ref rows) in &made.told_reader[made.told_reader_from..] {
        if let Some(telling) = readers.iter_mut().find(|reading| reading.id == id).and_then(|reading| reading.told.as_mut()) {
            tell(&mut telling.pending, TableId::ALL[table], Some(rows.iter().map(|(key, columns)| (key, columns))));
        }
    }
    made.told_reader_from = made.told_reader.len();
}

#[derive(Clone, Debug)]
pub struct Stands {
    frame: std::sync::Weak<DecodedFrame>,
    hour: Hour,
}

impl Stands {
    pub fn at(frame: std::sync::Weak<DecodedFrame>, hour: Hour) -> Self {
        Self { frame, hour }
    }

    pub fn look(&mut self, through: impl FnOnce(*const DecodedFrame) -> Option<Hour>) -> Hour {
        if let Some(hour) = through(self.frame.as_ptr()) {
            self.hour = self.hour.max(hour);
        }
        self.hour
    }

    pub fn hour(&self) -> Hour {
        self.hour
    }
}

pub struct Reader {
    shared: Arc<Shared>,
    id: u64,
    later: bool,
}

impl Reader {
    pub fn notify_progress(&self) {
        self.shared.notify_progress();
    }

    pub fn has_rows_at(&self, hour: Hour) -> bool {
        let readers = lock(&self.shared.readers);
        Self::place(&readers, self.id).and_then(|at| readers[at].stands()) == Some((hour, self.seek()))
    }

    fn place(readers: &[Reading], id: u64) -> Option<usize> {
        readers.iter().position(|held| held.id == id)
    }

    pub fn frame(&self) -> Option<Arc<DecodedFrame>> {
        let readers = lock(&self.shared.readers);
        readers.get(Self::place(&readers, self.id)?)?.frame().cloned()
    }

    pub fn frame_with_failures(&self) -> Option<(Arc<DecodedFrame>, Arc<[view::HeadFailure]>)> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        Some((reading.frame()?.clone(), Arc::clone(reading.failures()?)))
    }

    pub fn frame_now_through(&self, hour: Hour) -> Option<Arc<DecodedFrame>> {
        let readers = match self.shared.readers.try_lock() {
            Ok(readers) => readers,
            Err(std::sync::TryLockError::Poisoned(held)) => held.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        self.through_in(&readers, hour)
    }

    pub fn frame_through(&self, hour: Hour) -> Option<Arc<DecodedFrame>> {
        self.through_in(&lock(&self.shared.readers), hour)
    }

    fn through_in(&self, readers: &[Reading], hour: Hour) -> Option<Arc<DecodedFrame>> {
        let reading = readers.get(Self::place(readers, self.id)?)?;
        let seek = self.seek();
        let newest = reading.frame().into_iter().filter(|_| reading.stands().is_some_and(|(_, at)| at == seek));
        newest.chain(reading.older.iter().flatten().filter(|&&(_, at)| at == seek).map(|(frame, _)| frame)).find(|frame| frame.hour <= hour).cloned()
    }

    pub fn frame_of(&self, hour: Hour) -> Option<Arc<DecodedFrame>> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        let seek = self.seek();
        let newest = reading.frame().into_iter().filter(|_| reading.stands().is_some_and(|(_, at)| at == seek));
        newest.chain(reading.older.iter().flatten().filter(|&&(_, at)| at == seek).map(|(frame, _)| frame)).find(|frame| frame.hour == hour).cloned()
    }

    pub fn frame_now(&self) -> Option<Arc<DecodedFrame>> {
        let readers = match self.shared.readers.try_lock() {
            Ok(readers) => readers,
            Err(std::sync::TryLockError::Poisoned(held)) => held.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                viewer_runtime::view_record::add("map views", "frames_the_frame_did_not_wait_for", 1);
                return None;
            }
        };
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        reading.frame().cloned().filter(|_| reading.stands().is_some_and(|(_, at)| at == self.seek()))
    }

    pub fn newest(&self) -> Option<(Arc<DecodedFrame>, u64)> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        Some((reading.frame()?.clone(), reading.published))
    }

    pub fn newest_seek(&self) -> Option<(Arc<DecodedFrame>, u64, u64)> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        Some((reading.frame()?.clone(), reading.published, reading.stands()?.1))
    }

    pub fn seek(&self) -> u64 {
        self.shared.epoch.load(Ordering::Acquire)
    }

    pub fn through(&self, frame: *const DecodedFrame) -> Option<Hour> {
        self.through_seek(frame).map(|(hour, _)| hour)
    }

    pub fn through_seek(&self, frame: *const DecodedFrame) -> Option<(Hour, u64)> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        reading.frame().filter(|held| std::ptr::eq(Arc::as_ptr(held), frame)).and_then(|_| reading.stands())
    }

    pub fn arriving(&self) -> Vec<TableId> {
        let readers = lock(&self.shared.readers);
        Self::place(&readers, self.id).map_or_else(Vec::new, |at| readers[at].arriving.clone())
    }

    pub fn reads(&self) -> usize {
        let readers = lock(&self.shared.readers);
        Self::place(&readers, self.id).map_or(0, |at| readers[at].tables.len())
    }

    pub fn published(&self) -> u64 {
        let readers = lock(&self.shared.readers);
        Self::place(&readers, self.id).map_or(0, |at| readers[at].published)
    }

    pub fn wait_newer(&self, seen: u64, limit: Duration) -> u64 {
        let now = Instant::now();
        let until = now + limit;
        let mut readers = lock(&self.shared.readers);
        let mut woke = false;
        if let Some(at) = Self::place(&readers, self.id) {
            if !readers[at].ready {
                readers[at].ready = true;
                woke = true;
            }
        }
        if let Some(at) = Self::place(&readers, self.id)
            && let Some(made) = readers[at].published_at.filter(|_| readers[at].published == seen && !readers[at].costed)
        {
            let reading = &mut readers[at];
            let took = now.saturating_duration_since(made).as_nanos() as u64;
            reading.cost_ns = if reading.cost_ns == 0 { took } else { (reading.cost_ns * 3 + took) / 4 };
            reading.marks.build_ns += took;
            reading.marks.builds += 1;
            reading.costed = true;
        }
        if woke {
            drop(readers);
            self.shared.note_demand();
            readers = lock(&self.shared.readers);
        }
        loop {
            let published = Self::place(&readers, self.id).map_or(0, |at| readers[at].published);
            let now = Instant::now();
            if published > seen || now >= until {
                return published;
            }
            readers = self.shared.made.wait_timeout(readers, until - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    pub fn ready(&self) {
        let mut readers = lock(&self.shared.readers);
        let woke = Self::place(&readers, self.id).is_some_and(|at| !std::mem::replace(&mut readers[at].ready, true));
        drop(readers);
        if woke {
            self.shared.note_demand();
        }
    }


    pub fn wait(&self, mut ready: impl FnMut(&DecodedFrame) -> bool, limit: Duration) -> io::Result<Arc<DecodedFrame>> {
        let until = Instant::now() + limit;
        let mut readers = lock(&self.shared.readers);
        loop {
            if let Some(at) = Self::place(&readers, self.id)
                && let Some(frame) = readers[at].frame().cloned().filter(|frame| ready(frame))
            {
                return Ok(frame);
            }
            let now = Instant::now();
            if now >= until {
                let arriving = Self::place(&readers, self.id).map_or_else(Vec::new, |at| readers[at].arriving.clone());
                let waiting = if arriving.is_empty() { String::new() } else { format!(" (waiting for {arriving:?})") };
                return Err(io::Error::other(format!("No rows at the head came within {limit:?}{waiting}")));
            }
            readers = self.shared.made.wait_timeout(readers, until - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    pub fn frame_keyed(&self) -> Option<(Arc<DecodedFrame>, HeldKeys)> {
        let readers = lock(&self.shared.readers);
        let reading = readers.get(Self::place(&readers, self.id)?)?;
        Some((reading.frame()?.clone(), HeldKeys::of_tables(reading.frame_keys.clone())))
    }

    pub fn take_keyed(&self) -> Option<(Arc<DecodedFrame>, Told, HeldKeys)> {
        let mut readers = lock(&self.shared.readers);
        let at = Self::place(&readers, self.id)?;
        let reading = &mut readers[at];
        let frame = reading.frame()?.clone();
        let told = reading.told.as_mut().map(|telling| std::mem::take(&mut telling.unread)).unwrap_or_default();
        Some((frame, told, HeldKeys::of_tables(reading.frame_keys.clone())))
    }

    pub fn declare_keyed(&self, table: TableId, keys: Arc<BTreeSet<u64>>) {
        let mut readers = lock(&self.shared.readers);
        let Some(at) = Self::place(&readers, self.id) else { return };
        let joined = match readers[at].keyed.iter_mut().find(|keyed| keyed.table == table.index()) {
            Some(keyed) if Arc::ptr_eq(&keyed.keys, &keys) => return,
            Some(keyed) => {
                (keyed.keys, keyed.version) = (keys, keyed.version + 1);
                false
            }
            None => {
                readers[at].keyed.push(Keyed { table: table.index(), keys, version: 0 });
                readers[at].keyed.sort_unstable_by_key(|keyed| keyed.table);
                true
            }
        };
        readers[at].schema_ver += 1;
        let needs = readers[at].declared.clone();
        drop(readers);
        if joined {
            self.declared(&needs);
        } else {
            self.shared.note_demand();
            republish(&self.shared, self.id);
        }
    }

    pub fn declare_only(&self, tables: &[TableId]) {
        let readers = lock(&self.shared.readers);
        let Some(at) = Self::place(&readers, self.id) else { return };
        if readers[at].declared.iter().map(|need| need.table).eq(tables.iter().copied()) && readers[at].declared.iter().all(|need| need.columns == u64::MAX) {
            return;
        }
        drop(readers);
        self.declared(&tables.iter().map(|&table| Need::every(table)).collect::<Vec<Need>>());
    }

    pub fn declare_only_needs(&self, needs: &[Need]) {
        match self.later {
            true => {
                lock(&self.shared.asked).push(Asked::Declare { id: self.id, needs: needs.to_vec() });
                self.shared.note_demand();
            }
            false => self.declared(needs),
        }
    }

    fn declared(&self, needs: &[Need]) {
        declare(&self.shared, self.id, needs, true);
        self.shared.note_demand();
    }
}

fn declare(shared: &Arc<Shared>, id: u64, needs: &[Need], republishes: bool) {
    {
        let mut sets: BTreeMap<usize, u64> = BTreeMap::new();
        for need in needs {
            *sets.entry(need.table.index()).or_default() |= need.columns;
        }
        let mut kept = writing(&shared);
        let mut tables = Vec::with_capacity(sets.len());
        for (&table, &columns) in &sets {
            let Some(kinds) = shared.kinds.get(table) else { continue };
            tables.push((table, over(columns, kinds.len())));
        }
        let mut readers = lock(&shared.readers);
        if let Some(at) = Reader::place(&readers, id) {
            let learned: Vec<usize> = tables.iter().map(|&(table, _)| table).filter(|table| !readers[at].tables.iter().any(|&(held, _)| held == *table)).collect();
            if !learned.is_empty() && !readers[at].tables.is_empty() {
                let hour = lock(&shared.jobs).from.as_ref().map(|from| from.hour);
                let name = readers[at].name;
                let mut held = lock(&shared.learned);
                for table in learned {
                    held.entry((name, table)).or_insert((hour, 0)).1 += 1;
                }
            }
            if readers[at].tables != tables {
                readers[at].frame.clear();
                readers[at].schema_ver += 1;
                readers[at].frame_keys.clear();
                readers[at].keyed_views.clear();
            }
            readers[at].tables = tables;
            readers[at].declared = needs.to_vec();
        }
        let mut read: BTreeMap<usize, Vec<(u64, bool, bool)>> = BTreeMap::new();
        for reading in readers.iter().filter(|reading| reading.open) {
            let tells = reading.told.is_some();
            let wanted = reading.tables.iter().map(|&(table, columns)| (table, columns, true)).chain(reading.keyed.iter().map(|keyed| (keyed.table, u64::MAX, false)));
            for (table, columns, whole) in wanted {
                let kinds = shared.kinds.get(table).map_or(0, Vec::len);
                let columns = over(columns, kinds);
                let held = read.entry(table).or_default();
                match held.iter_mut().find(|(set, _, _)| *set == columns) {
                    Some((_, told, read_whole)) => (*told, *read_whole) = (*told || tells, *read_whole || whole),
                    None => held.push((columns, tells, whole)),
                }
            }
        }
        drop(readers);
        for (&table, sets) in &read {
            if !matches!(shared.written[table], playhead::pages::TableWritten::Written) {
                continue;
            }
            let Some(kinds) = shared.kinds.get(table) else { continue };
            let held = kept[table].get_or_insert_with(|| {
                let mut held = Kept::new(kinds.clone());
                held.exact = lock(&shared.exact_columns).get(&table).cloned();
                held
            });
            let columns = sets.iter().fold(0, |columns, &(set, _, _)| columns | set);
            let widened = columns & !held.columns != 0;
            for &(set, told, whole) in sets {
                let at = held.at(set);
                let made = mine(&mut held.views[at].made);
                (made.told, made.whole_read) = (told, whole);
                if !whole {
                    made.bound = usize::MAX;
                }
            }
            if widened {
                held.arriving();
            }
        }
        for (table, held) in kept.iter_mut().enumerate() {
            let Some(kept) = held else { continue };
            let sets = read.get(&table).map(Vec::as_slice).unwrap_or_default();
            kept.views.retain(|set| sets.iter().any(|&(columns, _, _)| columns == set.columns));
            kept.columns = kept.views.iter().fold(0, |held, set| held | set.columns);
            if kept.views.is_empty() {
                *held = None;
            }
        }
        drop(kept);
        if republishes {
            republish(shared, id);
        }
    }
}

const FRAMES_KEPT: usize = 4;

const NOT_SHOWN: i64 = i64::MIN;

pub struct AtHead {
    name: &'static str,
    open: Option<Reader>,
    needs: Vec<Need>,
    pace: Pace,
    asked: bool,
    now: bool,
}

impl std::fmt::Debug for AtHead {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("AtHead").field("name", &self.name).field("open", &self.open.is_some()).field("tables", &self.needs.len()).finish()
    }
}

impl AtHead {
    pub fn new(name: &'static str, tables: &[TableId], pace: Pace) -> Self {
        Self::with(name, &tables.iter().copied().map(Need::every).collect::<Vec<Need>>(), pace)
    }

    pub fn with(name: &'static str, needs: &[Need], pace: Pace) -> Self {
        Self { name, open: None, needs: needs.to_vec(), pace, asked: false, now: false }
    }

    pub fn keeps_its_answer(mut self) -> Self {
        self.now = true;
        self
    }

    pub fn learning(name: &'static str) -> Self {
        Self::with(name, &[], Pace::PerSecond(4)).keeps_its_answer()
    }

    pub fn frame(&mut self, views: &Views) -> Option<Arc<DecodedFrame>> {
        if let Some(hour) = views.shown_through() {
            return self.frame_through(views, hour);
        }
        let now = self.now;
        let open = self.kept(views);
        if now { open.frame_now() } else { open.frame() }
    }

    pub fn frame_through(&mut self, views: &Views, hour: Hour) -> Option<Arc<DecodedFrame>> {
        let now = self.now;
        let open = self.kept(views);
        if now { open.frame_now_through(hour) } else { open.frame_through(hour) }
    }

    pub fn keep(&mut self, views: &Views) {
        self.kept(views);
    }

    pub fn frame_of(&mut self, views: &Views, hour: Hour) -> Option<Arc<DecodedFrame>> {
        self.kept(views).frame_of(hour)
    }

    fn kept(&mut self, views: &Views) -> &Reader {
        self.asked = true;
        let reader = match self.open.take() {
            Some(reader) if views.holds(&reader) => reader,
            _ => match self.now {
                true => views.reader_later(self.name, &self.needs, self.pace),
                false => views.reader_through(self.name, &self.needs, self.pace),
            },
        };
        self.open.insert(reader)
    }

    pub fn declare(&mut self, needs: &[Need]) {
        let mut needs = needs.to_vec();
        needs.sort_unstable_by_key(|need| need.table.index());
        if needs == self.needs {
            return;
        }
        self.needs = needs;
        if let Some(reader) = &self.open {
            reader.declare_only_needs(&self.needs);
        }
    }

    pub fn discover<T>(&mut self, read: impl FnOnce() -> T) -> Option<T> {
        let (value, missed) = replay_frame::frame::discovering_fields(read);
        if missed.is_empty() {
            return Some(value);
        }
        self.learn_needs(missed.into_iter().map(|(table, fields)| Need::columns(table, fields)));
        None
    }

    pub fn learn(&mut self, tables: &[TableId]) {
        let new: Vec<Need> = tables.iter().copied().filter(|table| !self.needs.iter().any(|need| need.table == *table && need.columns == u64::MAX)).map(Need::every).collect();
        if !new.is_empty() {
            self.learn_needs(new);
        }
    }

    fn learn_needs(&mut self, needs: impl IntoIterator<Item = Need>) {
        for need in needs {
            match self.needs.iter_mut().find(|held| held.table == need.table) {
                Some(held) => held.columns |= need.columns,
                None => self.needs.push(need),
            }
        }
        self.needs.sort_unstable_by_key(|need| need.table.index());
        if let Some(reader) = &self.open {
            reader.declare_only_needs(&self.needs);
        }
    }

    pub fn end_frame(&mut self) {
        if !std::mem::take(&mut self.asked) {
            self.open = None;
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn wait(&self, ready: impl FnMut(&DecodedFrame) -> bool, limit: Duration) -> io::Result<Arc<DecodedFrame>> {
        self.open.as_ref().ok_or_else(|| io::Error::other("The reader at the head is not open"))?.wait(ready, limit)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        match self.later {
            true => lock(&self.shared.asked).push(Asked::Close { id: self.id }),
            false => close(&self.shared, self.id),
        }
        self.shared.note_demand();
    }
}

fn close(shared: &Arc<Shared>, id: u64) {
    let mut readers = lock(&shared.readers);
    if let Some(at) = Reader::place(&readers, id) {
        readers[at].open = false;
        readers[at].frame.clear();
    }
    drop(readers);
    declare(shared, id, &[], true);
}
