
#[cfg(test)]
mod tests;

use viewer_layer_core::layers::{Carried, Settling, Inputs, Layer, Published, Retirement, RetirementFlag, View};
use viewer_layer_core::playhead_host::{MapSide, mirror::Mirrors, views::{Need, Pace, Reader as ViewReader, Stands, Views, view::HeadFailure}};
use playhead::Step;
use replay_data::{
    Hour,
    rows::{TableId, tables},
};
use replay_frame::frame::{DecodedFrame, SharedView, discovering};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use viewer_foundation::{perspective, tables::core};
use viewer_raids::map::intel_icons::{self, IntelIcons};
use viewer_map_modes::map_mode::{MapMode, Stock};

pub const NAME: &str = "intel icons";

const CORE: [TableId; 5] = [TableId::Countries, TableId::CountryLinks, TableId::Provinces, TableId::States, TableId::Clock];

const CATCH_UP: Duration = Duration::from_secs(120);

pub trait Evaluate: Send + Sync + 'static {
    fn row_tables(&self) -> &[TableId] { &[] }
    fn icons(&self, frame: &DecodedFrame, choice: &perspective::Choice) -> IntelIcons;
}


pub fn ours(frame: &DecodedFrame, choice: &perspective::Choice) -> io::Result<u32> {
    let links: BTreeMap<u32, u32> = core::<tables::CountryLinks>(frame)?.iter().map(|link| (link.id, link.canonical_slot)).collect();
    let tags: BTreeMap<u32, String> = core::<tables::Countries>(frame)?.iter().map(|country| (country.canonical_slot, String::from_utf8_lossy(&country.tag).into_owned())).collect();
    perspective::viewer(frame, &links, &tags, &choice.country).map(|(ours, _)| ours)
}

#[derive(Clone, Debug)]
pub struct FailedView {
    pub failure: HeadFailure,
    pub view: SharedView,
}

struct Installed {
    shown: (viewer_runtime::landed::Landed<Arc<IntelIcons>>, Hour),
    failures: Arc<[FailedView]>,
}

struct Job {
    frame: Arc<DecodedFrame>,
    failures: Arc<[HeadFailure]>,
    choice: perspective::Choice,
    epoch: u64,
}

enum Work {
    Evaluate(Job),
    Free(Box<dyn Send>),
}

struct Done {
    icons: Option<IntelIcons>,
    failures: Arc<[FailedView]>,
    missed: Vec<TableId>,
    hour: Hour,
    frame: std::sync::Weak<DecodedFrame>,
    ns: u64,
    epoch: u64,
}

struct Open {
    reader: ViewReader,
    jobs: mpsc::Sender<Work>,
    done: viewer_runtime::landed::Receiver<Done>,
    busy: bool,
    seen: Option<(Arc<DecodedFrame>, Arc<[HeadFailure]>)>,
    handed: bool,
}

impl Open {
    fn new(views: &Views, tables: &[TableId], pace: Pace, evaluate: &Arc<dyn Evaluate>) -> io::Result<Self> {
        let reader = views.reader_at(NAME, &tables.iter().copied().map(Need::every).collect::<Vec<Need>>(), pace);
        let (jobs, receive) = mpsc::channel::<Work>();
        let (send, done) = viewer_runtime::landed::channel();
        let progress = views.progress_notifier();
        let evaluate = Arc::clone(evaluate);
        std::thread::Builder::new().name("intel icons".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            for work in receive {
                let job = match work {
                    Work::Evaluate(job) => job,
                    Work::Free(held) => {
                        drop(held);
                        continue;
                    }
                };
                let started = Instant::now();
                let (icons, missed) = discovering(|| evaluate.icons(&job.frame, &job.choice));
                let failures: Arc<[FailedView]> = if job.failures.is_empty() {
                    Arc::from([])
                } else {
                    job.failures
                        .iter()
                        .map(|failure| FailedView {
                            failure: failure.clone(),
                            view: job.frame.tables[failure.table.index()].clone(),
                        })
                        .collect()
                };
                let (hour, frame) = (job.frame.hour, Arc::downgrade(&job.frame));
                drop(job.frame);
                let icons = missed.is_empty().then_some(icons);
                if send.send(Done { icons, failures, missed, hour, frame, ns: started.elapsed().as_nanos() as u64, epoch: job.epoch }).is_err() {
                    return;
                }
                progress();
            }
        })?;
        Ok(Self { reader, jobs, done, busy: false, seen: None, handed: false })
    }

    fn free(&self, held: impl Send + 'static) {
        let _ = self.jobs.send(Work::Free(Box::new(held)));
    }

    fn see(&mut self, frame: Option<(Arc<DecodedFrame>, Arc<[HeadFailure]>)>) {
        if let Some(old) = std::mem::replace(&mut self.seen, frame) {
            self.free(old);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub evaluations: BTreeMap<u32, u64>,
    pub ages: BTreeMap<i32, u64>,
    pub loading: u64,
    pub skipped: u64,
    pub learning: u64,
    pub stale: u64,
    pub opened: u64,
    pub closed: u64,
}

#[derive(Debug, Default)]
struct Installs(std::sync::Mutex<BTreeMap<u32, u64>>);

impl Stats {
    pub fn summary(&self) -> Value {
        let spread = |held: &BTreeMap<i32, u64>| {
            let total: u64 = held.values().sum();
            let at = |share: f64| {
                let wanted = (share * total as f64).ceil().max(1.0) as u64;
                let mut seen = 0;
                held.iter().find(|(_, count)| {
                    seen += **count;
                    seen >= wanted
                }).map(|(value, _)| *value)
            };
            json!({"count": total, "p50": at(0.5), "p99": at(0.99), "worst": held.keys().next_back()})
        };
        let evaluations: BTreeMap<i32, u64> = self.evaluations.iter().map(|(&tenths, &count)| (tenths as i32, count)).collect();
        let tenths = |value: Value| value.as_i64().map(|tenths| tenths as f64 / 10.0);
        let evaluation = spread(&evaluations);
        json!({
            "evaluations": evaluation["count"], "evaluation_ms_p50": tenths(evaluation["p50"].clone()), "evaluation_ms_p99": tenths(evaluation["p99"].clone()), "evaluation_ms_worst": tenths(evaluation["worst"].clone()),
            "age_hours": spread(&self.ages), "frames_loading": self.loading, "rows_skipped": self.skipped, "discarded_learning": self.learning, "dropped_stale": self.stale,
            "opened": self.opened, "closed": self.closed,
        })
    }
}

impl Installs {
    fn summary(&self) -> Value {
        let held = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let total: u64 = held.values().sum();
        let at = |share: f64| {
            let wanted = (share * total as f64).ceil().max(1.0) as u64;
            let mut seen = 0;
            held.iter().find(|(_, count)| {
                seen += **count;
                seen >= wanted
            }).map(|(tens, _)| f64::from(*tens) * 0.01)
        };
        json!({"count": total, "ms_p50": at(0.5), "ms_p99": at(0.99), "ms_worst": held.keys().next_back().map(|tens| f64::from(*tens) * 0.01)})
    }
}

pub struct Icons {
    evaluate: Arc<dyn Evaluate>,
    retiring: RetirementFlag,
    views: Option<Views>,
    pace: Pace,
    shown: bool,
    choice: perspective::Choice,
    tables: Vec<TableId>,
    open: Option<Open>,
    icons: Option<Installed>,
    stands: Option<Stands>,
    epoch: u64,
    applied: Option<Hour>,
    stats: Stats,
    installs: Installs,
}

impl Icons {
    pub fn new(evaluate: Arc<dyn Evaluate>, pace: Pace) -> Self {
        let mut tables = CORE.to_vec();
        tables.extend_from_slice(evaluate.row_tables());
        tables.sort_unstable_by_key(|table| table.index());
        tables.dedup();
        Self { evaluate, retiring: RetirementFlag::default(), views: None, pace, shown: false, choice: perspective::Choice::default(), tables, open: None, icons: None, stands: None, epoch: 0, applied: None, stats: Stats::default(), installs: Installs::default() }
    }

    pub fn icons(&self) -> Option<(&Arc<IntelIcons>, Hour)> {
        self.icons.as_ref().map(|installed| (&*installed.shown.0, installed.shown.1))
    }

    pub fn landed_icons(&self) -> Option<viewer_runtime::landed::Landed<Arc<IntelIcons>>> {
        self.icons.as_ref().map(|installed| installed.shown.0.clone())
    }

    pub fn icons_with_failures(&self) -> Option<(&Arc<IntelIcons>, Hour, &[FailedView])> {
        self.icons
            .as_ref()
            .map(|installed| (&*installed.shown.0, installed.shown.1, &*installed.failures))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, icons: IntelIcons, hour: Hour, failures: Vec<FailedView>) {
        let (send, receive) = viewer_runtime::landed::channel();
        send.send(Arc::new(icons)).unwrap();
        self.show(Some(Installed {
            shown: (receive.try_recv().unwrap(), hour),
            failures: Arc::from(failures),
        }));
    }

    pub fn tables(&self) -> &[TableId] {
        &self.tables
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn evaluate(&self, frame: &DecodedFrame) -> IntelIcons {
        discovering(|| self.evaluate.icons(frame, &self.choice)).0
    }

    pub fn newest_rows(&self) -> Option<Arc<DecodedFrame>> {
        self.open.as_ref().and_then(|open| open.reader.frame())
    }

    fn close(&mut self) {
        let icons = self.icons.take();
        if let Some(open) = self.open.take() {
            self.stats.closed += 1;
            let _ = std::thread::Builder::new().name("intel icons closing".into()).spawn(move || {
                viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
                drop((open, icons));
            });
        }
        self.epoch += 1;
    }

    fn show(&mut self, icons: Option<Installed>) {
        if let Some(old) = std::mem::replace(&mut self.icons, icons) {
            match &self.open {
                Some(open) => open.free(old),
                None => drop(old),
            }
        }
    }

    fn ensure_open(&mut self) {
        if self.open.is_some() || !self.shown {
            return;
        }
        let Some(views) = &self.views else { return };
        match Open::new(views, &self.tables, self.pace, &self.evaluate) {
            Ok(open) => {
                self.open = Some(open);
                self.stats.opened += 1;
            }
            Err(error) => eprintln!("The {NAME} layer's reader is refused: {error}"),
        }
    }

    fn receive(&mut self) {
        while let Some(done) = self.open.as_ref().and_then(|open| open.done.try_recv().ok()) {
            let _ = self.take(done);
        }
    }

    fn take(&mut self, done: viewer_runtime::landed::Landed<Done>) -> bool {
        let Some(open) = &mut self.open else { return false };
        open.busy = false;
        if done.epoch != self.epoch {
            self.stats.stale += 1;
            return false;
        }
        *self.stats.evaluations.entry((done.ns / 100_000) as u32).or_default() += 1;
        let (hour, frame, failures, missed) = (done.hour, done.frame.clone(), Arc::clone(&done.failures), done.missed.clone());
        match done.map(|done| done.icons).transpose() {
            Some(icons) => {
                self.show(Some(Installed {
                    shown: (icons.map(Arc::new), hour),
                    failures,
                }));
                self.stands = Some(Stands::at(frame, hour));
            }
            None => {
                self.stats.learning += 1;
                self.tables.extend(missed);
                self.tables.sort_unstable_by_key(|table| table.index());
                self.tables.dedup();
                open.reader.declare_only(&self.tables);
                open.see(None);
                return true;
            }
        }
        false
    }

    fn offer(&mut self) {
        let Some(open) = &mut self.open else { return };
        if let Some((frame, failures)) = open.reader.frame_with_failures()
            && !open.seen.as_ref().is_some_and(|(seen, _)| Arc::ptr_eq(seen, &frame))
        {
            if open.seen.is_some() && !open.handed {
                self.stats.skipped += 1;
            }
            open.see(Some((frame, failures)));
            open.handed = false;
        }
        if open.busy || open.handed {
            return;
        }
        let Some((frame, failures)) = open.seen.clone() else { return };
        if open.jobs.send(Work::Evaluate(Job { frame, failures, choice: self.choice.clone(), epoch: self.epoch })).is_ok() {
            open.busy = true;
            open.handed = true;
        }
    }

    fn catch_up(&mut self, settling: Settling) -> io::Result<()> {
        let Some(hour) = self.applied else { return Ok(()) };
        let until = Instant::now() + settling.waiting(CATCH_UP);
        let ran_out = |why: String| if settling.strict() { Err(io::Error::other(why)) } else { Ok(()) };
        loop {
            if self.retiring.requested() {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if self.icons.as_ref().is_some_and(|installed| installed.shown.1 == hour) && !self.open.as_ref().is_some_and(|open| open.busy) {
                return Ok(());
            }
            let Some(open) = &mut self.open else { return Ok(()) };
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return ran_out(format!("The {NAME} layer did not reach hour {hour} in {CATCH_UP:?}"));
            }
            if open.busy {
                let done = match open.done.recv_timeout(left.min(RetirementFlag::POLL)) {
                    Ok(done) => done,
                    Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < until => continue,
                    Err(_) => return ran_out(format!("The {NAME} layer's worker did not answer for hour {hour}")),
                };
                if self.take(done) {
                    return Ok(());
                }
                continue;
            }
            let Some((frame, failures)) = open.reader.frame_with_failures() else {
                return Ok(());
            };
            if frame.hour < hour {
                let seen = open.reader.published();
                if open.reader.wait_newer(seen, left.min(RetirementFlag::POLL)) == seen && Instant::now() >= until {
                    return ran_out(format!("The {NAME} layer's rows stand at {}, not {hour}", frame.hour));
                }
                continue;
            }
            let seen = open.seen.as_ref().is_some_and(|(seen, _)| Arc::ptr_eq(seen, &frame));
            if frame.hour > hour || (seen && open.handed) {
                return Ok(());
            }
            if !seen {
                open.see(Some((frame, failures)));
                open.handed = false;
            }
            self.offer();
        }
    }

    fn icon_bytes(icons: &IntelIcons) -> usize {
        use intel_icons::Write;
        let mut bytes = std::mem::size_of::<IntelIcons>() + icons.states.capacity() * std::mem::size_of::<intel_icons::StateIcon>();
        for state in &icons.states {
            bytes += state.entries.capacity() * std::mem::size_of::<intel_icons::Entry>();
            for entry in &state.entries {
                bytes += entry.writes.capacity() * std::mem::size_of::<Write>();
                bytes += entry.writes.iter().map(|write| match write {
                    Write::Sprite(_, text) | Write::Portrait(_, text) => text.capacity(),
                    Write::Text(_, text) | Write::Withheld(_, text) => text.capacity(),
                    _ => 0,
                }).sum::<usize>();
                let tooltip = &entry.tooltip;
                bytes += std::mem::size_of_val(&**tooltip) + tooltip.title.capacity() + tooltip.body.capacity() + tooltip.refused.keys().map(String::capacity).sum::<usize>();
            }
        }
        bytes + icons.report.reasons.keys().map(String::capacity).sum::<usize>()
    }
}

impl Layer for Icons {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn placed(&mut self, _view: &viewer_layer_core::layers::View) -> Option<u64> {
        Some(0)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let shown = view.map_mode == MapMode::Stock(Stock::Operatives);
        if shown && view.perspective != self.choice {
            self.choice = view.perspective.clone();
            self.show(None);
            self.epoch += 1;
            if let Some(open) = &mut self.open {
                open.handed = false;
            }
        }
        self.shown = shown;
        if !shown {
            self.close();
            return Ok(());
        }
        self.ensure_open();
        inputs.table(TableId::Countries, true)
    }

    fn views(&mut self, store: &viewer_layer_core::playhead_host::views::Store) -> io::Result<()> {
        self.views = Some(store.handle());
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.applied = mirrors.get(TableId::Countries).and_then(|mirror| mirror.head()).or(self.applied);
        self.show(None);
        self.epoch += 1;
        if let Some(open) = &mut self.open {
            open.handed = false;
        }
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.applied = Some(step.last());
    }

    fn settle(&mut self, _mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        if !self.shown {
            return Ok(());
        }
        self.ensure_open();
        self.receive();
        self.offer();
        if let Some(open) = self.open.as_ref().filter(|open| !open.busy) {
            open.reader.ready();
        }
        self.catch_up(settling)?;
        if let Some(open) = &self.open {
            if let Some(stands) = &mut self.stands {
                stands.look(|frame| open.reader.through(frame));
            }
        }
        match (&self.icons, self.applied) {
            (Some(installed), Some(applied)) => *self.stats.ages.entry(applied - installed.shown.1).or_default() += 1,
            (None, _) => self.stats.loading += 1,
            _ => {}
        }
        Ok(())
    }


    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> Published {
        self.icons.as_ref().map_or(Published::Current, |installed| Published::Cadence(self.stands.as_ref().map_or(installed.shown.1, |stands| stands.hour().max(installed.shown.1))))
    }

    fn made_at(&self, hour: Hour) -> bool {
        self.open.as_ref().is_none_or(|open| {
            !open.busy && open.seen.is_some() && open.handed && matches!(self.published(), Published::Cadence(held) if held == hour)
        })
    }

    fn held(&self) -> usize {
        self.icons.as_ref().map_or(0, |installed| Self::icon_bytes(&installed.shown.0)) + self.tables.capacity() * std::mem::size_of::<TableId>()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn drawn(side: viewer_layer_core::playhead_host::LayerParts<'_>) -> Option<viewer_runtime::landed::Landed<Arc<IntelIcons>>> {
    if !side.draws(NAME) {
        return None;
    }
    let layer = side.parts()?.1.iter().find_map(|layer| layer.any().downcast_ref::<Icons>())?;
    layer.landed_icons()
}

pub fn summary(side: &MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Value {
    let Some(layer) = side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<Icons>())) else { return Value::Null };
    json!({
        "stats": layer.stats().summary(),
        "installs": layer.installs.summary(),
        "tables": layer.tables().iter().map(|table| format!("{table:?}")).collect::<Vec<_>>(),
        "icons_bytes": layer.held(),
        "open": layer.is_open(),
    })
}
