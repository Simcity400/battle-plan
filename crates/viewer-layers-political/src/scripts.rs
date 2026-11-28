
use viewer_layer_core::layers::{Carried, Inputs, Layer, Published, Rate, Settling, View};
use viewer_layer_core::playhead_host::{MapSide, mirror::Mirrors, views::{Need, Pace, Reader as ViewReader, Views}};
use dashboard_text::triggers::World;
use playhead::Step;
use replay_data::{
    Hour,
    rows::{TableId, tables},
};
use replay_frame::frame::{DecodedFrame, discovering_fields};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use viewer_foundation::{perspective, tables::core};
use viewer_foundation::trigger_content::TriggerContent;
use viewer_raids::map::raids::{HourLayer, IconCache, RaidAssets, RaidIcons, RaidReport, cycle::Cycle, paths::HourPaths};
use viewer_runtime::landed::{self, Landed};

pub const NAME: &str = "scripts at the head";

pub const MUSIC_RATE: Rate = Rate::a_second(4);

const CATCH_UP: Duration = Duration::from_secs(120);

const CORE: [TableId; 4] = [TableId::Countries, TableId::CountryLinks, TableId::Provinces, TableId::States];

type Missed = Vec<(TableId, u64)>;

pub enum Clock {
    Machine(Instant),
    #[cfg(any(test, feature = "test-support"))]
    Stepped(Duration),
}

impl Default for Clock {
    fn default() -> Self {
        Self::Machine(Instant::now())
    }
}

impl Clock {
    fn now(&self) -> Duration {
        match self {
            Self::Machine(from) => from.elapsed(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Stepped(at) => *at,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn step(&mut self, by: Duration) {
        match self {
            Self::Machine(_) => panic!("the machine's clock is not stepped"),
            Self::Stepped(at) => *at += by,
        }
    }
}

static PRIMED: std::sync::OnceLock<Vec<Need>> = std::sync::OnceLock::new();

pub fn prime(needs: Vec<Need>) {
    let _ = PRIMED.set(needs);
}

fn joined(needs: &mut Vec<Need>, missed: impl IntoIterator<Item = (TableId, u64)>) {
    for (table, fields) in missed {
        match needs.iter_mut().find(|need| need.table == table) {
            Some(need) => need.columns |= fields,
            None => needs.push(Need::columns(table, fields)),
        }
    }
    needs.sort_unstable_by_key(|need| need.table.index());
}

#[derive(Clone)]
pub struct Held {
    raids: Arc<RaidAssets>,
    triggers: Arc<TriggerContent>,
    audio: Option<Arc<viewer_audio::Content>>,
    tables: Option<engine_executable::executable_tables::Tables>,
}

impl Held {
    pub fn new(raids: Arc<RaidAssets>, triggers: Arc<TriggerContent>, audio: Option<Arc<viewer_audio::Content>>, tables: Option<engine_executable::executable_tables::Tables>) -> Self {
        Self { raids, triggers, audio, tables }
    }
}

#[derive(Clone)]
pub struct Product {
    pub raids: Option<Arc<RaidIcons>>,
    pub notes: Option<(u32, Arc<Vec<String>>)>,
    pub music: Option<(Hour, Arc<viewer_audio::Chances>)>,
    pub hour: Hour,
    pub complete: bool,
}

struct Job {
    frame: Arc<DecodedFrame>,
    choice: perspective::Choice,
    content: Held,
    raids: bool,
    music: bool,
    whole: bool,
    epoch: u64,
    resets: u64,
}

enum Work {
    Evaluate(Box<Job>),
    Free(Box<dyn Send>),
}

struct Done {
    product: Option<Product>,
    missed: Missed,
    ns: u64,
    epoch: u64,
}

struct Chain {
    resets: u64,
    content: std::sync::Weak<RaidAssets>,
    country: perspective::Country,
    hour: Hour,
    cycle: Cycle,
    icons: IconCache,
}

struct Open {
    reader: ViewReader,
    jobs: mpsc::Sender<Work>,
    done: landed::Receiver<Done>,
    busy: bool,
    seen: Option<Arc<DecodedFrame>>,
    handed: bool,
    handed_whole: bool,
    evaluating: Option<Hour>,
}

impl Open {
    fn new(views: &Views, needs: &[Need]) -> io::Result<Self> {
        let reader = views.reader_at(NAME, needs, Pace::Ready);
        let (jobs, receive) = mpsc::channel::<Work>();
        let (send, done) = landed::channel();
        let progress = views.progress_notifier();
        std::thread::Builder::new().name(NAME.into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            let mut evaluating = Evaluating::default();
            for work in receive {
                let job = match work {
                    Work::Evaluate(job) => job,
                    Work::Free(held) => {
                        drop(held);
                        continue;
                    }
                };
                let started = Instant::now();
                let mut missed = Missed::new();
                let tables = job.content.tables;
                let product = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    engine_executable::executable_tables::scope(tables, || evaluating.evaluate(&job, &mut missed))
                }));
                let product = product.unwrap_or_else(|_| {
                    evaluating.failed(&job);
                    missed.clear();
                    Some(Product {
                        raids: Some(Arc::new(RaidIcons { report: RaidReport { worker_failed: true, ..Default::default() }, ..Default::default() })),
                        notes: None,
                        music: None,
                        hour: job.frame.hour,
                        complete: true,
                    })
                });
                let (epoch, ns) = (job.epoch, started.elapsed().as_nanos() as u64);
                drop(job);
                if send.send(Done { product, missed, ns, epoch }).is_err() {
                    return;
                }
                progress();
            }
        })?;
        Ok(Self { reader, jobs, done, busy: false, seen: None, handed: false, handed_whole: false, evaluating: None })
    }

    fn free(&self, held: impl Send + 'static) {
        let _ = self.jobs.send(Work::Free(Box::new(held)));
    }

    fn see(&mut self, frame: Option<Arc<DecodedFrame>>) {
        if let Some(old) = std::mem::replace(&mut self.seen, frame) {
            self.free(old);
        }
    }
}

fn learning<T>(missed: &mut Missed, read: impl FnOnce() -> T) -> T {
    let (value, tables) = discovering_fields(read);
    missed.extend(tables);
    value
}

fn raid_notes(frame: &DecodedFrame, ours: u32) -> Vec<String> {
    let mut out = viewer_foundation::tables::Limitations::default();
    if let Ok(raids) = frame.complete_fields::<tables::Raids>(replay_data::rows::mask::<replay_data::rows::RaidRow>(&["parent_slot", "phase"])) {
        let own: Vec<_> = raids.iter().filter(|raid| raid.parent_slot == ours && raid.phase != 5).collect();
        out.note_raid_arrows(own.len(), own.iter().filter(|raid| raid.phase == 4).count());
    }
    out.into_lines()
}

fn ours(frame: &DecodedFrame, choice: &perspective::Choice) -> io::Result<u32> {
    let links: BTreeMap<u32, u32> = core::<tables::CountryLinks>(frame)?.iter().map(|link| (link.id, link.canonical_slot)).collect();
    let tags: BTreeMap<u32, String> = core::<tables::Countries>(frame)?.iter().map(|country| (country.canonical_slot, String::from_utf8_lossy(&country.tag).into_owned())).collect();
    perspective::viewer(frame, &links, &tags, &choice.country).map(|(slot, _)| slot)
}

#[derive(Default)]
struct Evaluating {
    chain: Option<Chain>,
    memo: Memo,
    kept: Option<map_products::music_inputs::Kept>,
    failed: Option<u64>,
}

struct Memo(map_products::music_inputs::Memo);

impl Default for Memo {
    fn default() -> Self {
        Self(map_products::music_inputs::Memo::by_identity())
    }
}

impl Evaluating {
    fn failed(&mut self, job: &Job) {
        self.chain = None;
        self.kept = None;
        self.failed = Some(job.resets);
    }

    fn evaluate(&mut self, job: &Job, missed: &mut Missed) -> Option<Product> {
        let frame: &DecodedFrame = &job.frame;
        let ours = learning(missed, || ours(frame, &job.choice)).map_err(|error| error.to_string());
        let music = if job.music { self.music(job, ours.clone(), missed) } else { None };
        let (raids, notes, complete) = if job.raids {
            self.raids(job, ours, missed)
        } else {
            self.chain = None;
            (None, None, true)
        };
        if !missed.is_empty() {
            return None;
        }
        Some(Product { raids, notes, music, hour: frame.hour, complete })
    }

    fn music(&mut self, job: &Job, ours: Result<u32, String>, missed: &mut Missed) -> Option<(Hour, Arc<viewer_audio::Chances>)> {
        let frame: &DecodedFrame = &job.frame;
        let audio = job.content.audio.as_ref().filter(|audio| !audio.music.songs.is_empty());
        let (Some(audio), Ok(ours)) = (audio, ours) else {
            self.kept = None;
            return None;
        };
        let (chances, kept) = learning(missed, || {
            let world = World::read(frame, job.content.triggers.facts());
            self.memo.0.chances_kept(frame, audio, ours, &world)
        });
        if !missed.is_empty() {
            return None;
        }
        let at = kept.hour();
        self.kept = Some(kept);
        Some((at, Arc::new(chances)))
    }

    fn raids(&mut self, job: &Job, ours: Result<u32, String>, missed: &mut Missed) -> (Option<Arc<RaidIcons>>, Option<(u32, Arc<Vec<String>>)>, bool) {
        let frame: &DecodedFrame = &job.frame;
        let assets = &job.content.raids;
        let unavailable = |why: String| (Some(Arc::new(RaidIcons::unavailable(frame, 0, why))), None, true);
        let ours = match ours {
            Ok(ours) => ours,
            Err(error) => return unavailable(error),
        };
        if self.failed == Some(job.resets) {
            return (None, None, true);
        }
        let restart = self.chain.as_ref().is_none_or(|chain| {
            chain.resets != job.resets || !std::ptr::eq(chain.content.as_ptr(), Arc::as_ptr(assets)) || chain.country != job.choice.country || frame.hour < chain.hour
        });
        if restart {
            self.chain = Some(Chain { resets: job.resets, content: Arc::downgrade(assets), country: job.choice.country.clone(), hour: frame.hour, cycle: Cycle::new(frame.branch, ours), icons: IconCache::default() });
        }
        let world = match learning(missed, || World::new(frame, assets.trigger_content.facts())) {
            Ok(world) => world,
            Err(reason) => return unavailable(reason.to_string()),
        };
        let paths = learning(missed, || HourPaths::new(assets, frame));
        let layer = learning(missed, || HourLayer::new(assets, &world, ours, &paths));
        let chain = self.chain.as_mut().expect("started above");
        let mut cycle = match chain.cycle.clone() {
            cycle if cycle.ours() == ours && cycle.branch() == frame.branch => cycle,
            _ => Cycle::new(frame.branch, ours),
        };
        let icons = learning(missed, || {
            if let Err(reason) = cycle.begin(assets, &layer.sweep) {
                cycle = Cycle::restarted(frame.branch, ours, reason);
                let _ = cycle.begin(assets, &layer.sweep);
            }
            cycle.step(&layer.sweep);
            while job.whole && !cycle.complete() {
                let before = cycle.progress();
                cycle.step(&layer.sweep);
                if cycle.progress() == before {
                    break;
                }
            }
            layer.icons(&cycle, &mut chain.icons)
        });
        let notes = learning(missed, || raid_notes(frame, ours));
        if !missed.is_empty() {
            chain.icons = IconCache::default();
            return (None, None, false);
        }
        let complete = cycle.complete();
        chain.cycle = cycle;
        chain.hour = frame.hour;
        (Some(Arc::new(icons)), Some((ours, Arc::new(notes))), complete)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub evaluations: BTreeMap<u32, u64>,
    pub whole: BTreeMap<u32, u64>,
    pub rated: u64,
    pub rows_behind: u64,
    pub skipped: u64,
    pub learning: u64,
    pub stale: u64,
    pub waited: u64,
    pub ran_out: u64,
    pub opened: u64,
    pub closed: u64,
}

impl Stats {
    pub fn summary(&self) -> Value {
        let spread = |held: &BTreeMap<u32, u64>| {
            let total: u64 = held.values().sum();
            let at = |share: f64| {
                let wanted = (share * total as f64).ceil().max(1.0) as u64;
                let mut seen = 0;
                held.iter().find(|(_, count)| {
                    seen += **count;
                    seen >= wanted
                }).map(|(tenths, _)| f64::from(*tenths) / 10.0)
            };
            json!({"count": total, "ms_p50": at(0.5), "ms_p99": at(0.99), "ms_worst": held.keys().next_back().map(|tenths| f64::from(*tenths) / 10.0)})
        };
        json!({
            "evaluations": spread(&self.evaluations), "whole_cycle_evaluations": spread(&self.whole),
            "music_withheld_by_its_rate": self.rated, "rows_behind_a_waiting_settle": self.rows_behind, "rows_skipped": self.skipped, "discarded_learning": self.learning,
            "dropped_stale": self.stale, "settles_that_waited": self.waited, "waits_that_ran_out": self.ran_out,
            "opened": self.opened, "closed": self.closed,
        })
    }
}

pub struct Scripts {
    content: Held,
    views: Option<Views>,
    shown: (bool, bool),
    choice: perspective::Choice,
    needs: Vec<Need>,
    open: Option<Open>,
    product: Option<Landed<Product>>,
    applied: Option<Hour>,
    asked_at: Option<Duration>,
    pub clock: Clock,
    epoch: u64,
    stats: Stats,
    #[cfg(test)]
    block_catch_up: bool,
}

#[cfg(test)]
fn blocking_catch_up(scripts: &Scripts) -> bool {
    scripts.block_catch_up
}

#[cfg(not(test))]
fn blocking_catch_up(_scripts: &Scripts) -> bool {
    false
}

impl Scripts {
    pub fn new(held: Held) -> Self {
        use replay_data::rows::{BuildingRow, CountryBuildingResourceRow, DivisionEquipmentRow, ProjectRow, StateBaseResourceRow, mask};
        let mut needs = PRIMED.get().cloned().unwrap_or_default();
        joined(&mut needs, [
            (TableId::StateBuildings, mask::<BuildingRow>(&["level", "healthy_level"])),
            (TableId::StateBaseResources, mask::<StateBaseResourceRow>(&["amount"])),
            (TableId::CountryBuildingResources, mask::<CountryBuildingResourceRow>(&["building", "count", "resource", "amount"])),
            (TableId::DivisionEquipment, mask::<DivisionEquipmentRow>(&["variant_handle", "amount_raw"])),
            (TableId::WingEquipment, u64::MAX),
            (TableId::Projects, mask::<ProjectRow>(&["definition_id", "completed"])),
            (TableId::Nukes, u64::MAX),
            (TableId::RaidCooldowns, u64::MAX),
        ]);
        Self::with(held, needs)
    }

    pub fn with(content: Held, mut needs: Vec<Need>) -> Self {
        joined(&mut needs, CORE.iter().map(|&table| (table, 0)));
        joined(&mut needs, [(TableId::Clock, u64::MAX)]);
        Self {
            content,
            views: None,
            shown: (false, false),
            choice: perspective::Choice::default(),
            needs,
            open: None,
            product: None,
            applied: None,
            asked_at: None,
            clock: Clock::default(),
            epoch: 0,
            stats: Stats::default(),
            #[cfg(test)]
            block_catch_up: false,
        }
    }

    pub fn product(&self) -> Option<&Product> {
        self.product.as_deref()
    }

    pub fn raids(&self, branch: replay_data::BranchId, ours: u32) -> Option<Landed<Arc<RaidIcons>>> {
        let product = self.product.as_ref()?;
        let icons = product.raids.clone().filter(|icons| match icons.built_for {
            Some((built, _, country)) => (built, country) == (branch, ours),
            None => icons.report.worker_failed,
        })?;
        Some(product.as_ref().map(|_| icons))
    }

    pub fn notes(&self, ours: u32) -> Option<Arc<Vec<String>>> {
        self.product.as_ref()?.notes.as_ref().filter(|(country, _)| *country == ours).map(|(_, notes)| Arc::clone(notes))
    }

    pub fn music_at(&self) -> Option<(Hour, Arc<viewer_audio::Chances>)> {
        self.product.as_ref()?.music.clone()
    }

    pub fn landed_music(&self) -> Option<Landed<Arc<viewer_audio::Chances>>> {
        let product = self.product.as_ref()?;
        let (_, music) = product.music.as_ref()?;
        Some(product.as_ref().map(|_| Arc::clone(music)))
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn needs(&self) -> &[Need] {
        &self.needs
    }

    fn close(&mut self) -> Option<std::thread::JoinHandle<()>> {
        let product = self.product.take();
        let closing = self.open.take().and_then(|open| {
            self.stats.closed += 1;
            std::thread::Builder::new().name("scripts closing".into()).spawn(move || {
                viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
                drop((open, product));
            }).ok()
        });
        self.asked_at = None;
        self.epoch += 1;
        closing
    }

    fn show(&mut self, product: Option<Landed<Product>>) {
        if let Some(old) = std::mem::replace(&mut self.product, product) {
            match &self.open {
                Some(open) => open.free(old),
                None => drop(old),
            }
        }
    }

    fn ensure_open(&mut self) {
        if self.open.is_some() || self.shown == (false, false) {
            return;
        }
        let Some(views) = &self.views else { return };
        match Open::new(views, &self.needs) {
            Ok(open) => {
                self.open = Some(open);
                self.stats.opened += 1;
            }
            Err(error) => eprintln!("The map's scripts at the head are refused: {error}"),
        }
    }

    fn receive(&mut self) {
        while let Some(done) = self.open.as_ref().and_then(|open| open.done.try_recv().ok()) {
            let _ = self.take(done);
        }
    }

    fn take(&mut self, done: Landed<Done>) -> bool {
        let Some(open) = &mut self.open else { return false };
        (open.busy, open.evaluating) = (false, None);
        if done.epoch != self.epoch {
            self.stats.stale += 1;
            return false;
        }
        let tenths = (done.ns / 100_000) as u32;
        if done.product.is_some() {
            *self.stats.evaluations.entry(tenths).or_default() += 1;
            self.show(Some(done.map(|done| done.product.expect("checked"))));
            return false;
        }
        let done = done.into_inner();
        *self.stats.evaluations.entry(tenths).or_default() += 1;
        self.stats.learning += 1;
        joined(&mut self.needs, done.missed);
        open.reader.declare_only_needs(&self.needs);
        open.see(None);
        (open.handed, open.handed_whole) = (false, false);
        true
    }

    fn ask_rows(&mut self) {
        if let Some(open) = &self.open {
            open.reader.ready();
        }
    }

    fn music_due(&mut self, whole: bool) -> bool {
        if !self.shown.1 {
            return false;
        }
        let now = self.clock.now();
        if !whole && self.asked_at.is_some_and(|asked| now.saturating_sub(asked) < MUSIC_RATE.interval()) {
            self.stats.rated += 1;
            return false;
        }
        self.asked_at = Some(now);
        true
    }

    fn offer(&mut self, whole: bool) {
        let Some(open) = self.open.as_mut() else { return };
        if let Some((frame, _, _)) = open.reader.newest_seek()
            && !open.seen.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, &frame))
        {
            if open.seen.is_some() && !open.handed {
                self.stats.skipped += 1;
            }
            open.see(Some(frame));
            (open.handed, open.handed_whole) = (false, false);
        }
        if open.busy || (open.handed && (!whole || open.handed_whole)) {
            return;
        }
        let Some((frame, _, resets)) = open.reader.newest_seek() else { return };
        let music = self.music_due(whole);
        let open = self.open.as_mut().expect("held above");
        let job = Job { frame, choice: self.choice.clone(), content: self.content.clone(), raids: self.shown.0, music, whole, epoch: self.epoch, resets };
        if open.jobs.send(Work::Evaluate(Box::new(job))).is_ok() {
            (open.busy, open.handed, open.handed_whole) = (true, true, whole);
            open.evaluating = open.seen.as_ref().map(|frame| frame.hour);
        }
    }

    fn catch_up(&mut self, settling: Settling) -> io::Result<()> {
        let Some(hour) = self.applied else { return Ok(()) };
        if self.shown == (false, false) {
            return Ok(());
        }
        self.stats.waited += 1;
        let blocking = blocking_catch_up(self);
        let until = (!blocking).then(|| Instant::now() + settling.waiting(CATCH_UP));
        let ran_out = |why: String, stats: &mut Stats| {
            stats.ran_out += 1;
            if settling.strict() { Err(io::Error::other(why)) } else { Ok(()) }
        };
        let music = self.shown.1;
        loop {
            let whole = self.product.as_ref().is_some_and(|product| {
                product.hour == hour && product.complete && (!music || product.music.as_ref().is_some_and(|(at, _)| *at == hour))
            });
            if whole && !self.open.as_ref().is_some_and(|open| open.busy) {
                return Ok(());
            }
            let Some(open) = &mut self.open else { return Ok(()) };
            let left = until.map(|until| until.saturating_duration_since(Instant::now()));
            if left.is_some_and(|left| left.is_zero()) {
                return ran_out(format!("The {NAME} layer did not reach hour {hour} in {CATCH_UP:?}"), &mut self.stats);
            }
            if open.busy {
                let done = match left {
                    None => match open.done.recv() {
                        Ok(done) => done,
                        Err(_) => {
                            open.busy = false;
                            open.evaluating = None;
                            return Err(io::Error::other(format!(
                                "The {NAME} layer's worker stopped before answering for hour {hour}"
                            )));
                        }
                    },
                    Some(left) => match open.done.recv_timeout(left) {
                        Ok(done) => done,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            open.busy = false;
                            open.evaluating = None;
                            return Err(io::Error::other(format!(
                                "The {NAME} layer's worker stopped before answering for hour {hour}"
                            )));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            return ran_out(
                                format!("The {NAME} layer's worker did not answer for hour {hour}"),
                                &mut self.stats,
                            );
                        }
                    },
                };
                if self.take(done) {
                    return Ok(());
                }
                continue;
            }
            let Some(frame) = open.reader.frame() else {
                self.ask_rows();
                return Ok(());
            };
            if frame.hour != hour {
                self.stats.rows_behind += 1;
                self.ask_rows();
                return Ok(());
            }
            self.offer(true);
            if !self.open.as_ref().is_some_and(|open| open.busy) {
                return Ok(());
            }
        }
    }
}

impl Layer for Scripts {
    fn name(&self) -> &'static str {
        NAME
    }

    fn placed(&mut self, _view: &View) -> Option<u64> {
        Some(0)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let shown = (view.graphics.counter(view.graphics.raid_icons), view.sound);
        if shown != (false, false) && view.perspective != self.choice {
            self.choice = view.perspective.clone();
            self.show(None);
            self.epoch += 1;
            if let Some(open) = &mut self.open {
                (open.handed, open.handed_whole) = (false, false);
            }
        }
        if shown != self.shown {
            self.shown = shown;
            self.show(None);
        }
        if shown == (false, false) {
            drop(self.close());
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
        self.asked_at = None;
        if let Some(open) = &mut self.open {
            (open.handed, open.handed_whole) = (false, false);
        }
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.applied = Some(step.last());
    }

    fn made_at(&self, head: Hour) -> bool {
        self.shown == (false, false) || self.product.as_ref().is_some_and(|product| product.hour == head && product.complete)
    }

    fn behind_bound(&self) -> u32 {
        Rate::a_publication().behind()
    }

    fn ask(&mut self, _mirrors: &Mirrors, _upstream: &viewer_layer_core::layers::Upstream<'_>) -> io::Result<bool> {
        if self.shown == (false, false) {
            return Ok(false);
        }
        self.ensure_open();
        self.receive();
        self.offer(false);
        self.ask_rows();
        Ok(true)
    }

    fn join(&mut self, _mirrors: &Mirrors, _upstream: &viewer_layer_core::layers::Upstream<'_>, settling: Settling) -> io::Result<()> {
        if self.shown == (false, false) {
            return Ok(());
        }
        self.ensure_open();
        self.receive();
        if settling.waits() {
            self.offer(true);
            self.catch_up(settling)?;
            self.ask_rows();
        }
        Ok(())
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        let upstream = viewer_layer_core::layers::Upstream::new(&[]);
        self.ask(mirrors, &upstream)?;
        self.join(mirrors, &upstream, settling)
    }

    fn carried(&self) -> Carried {
        Carried { entities: usize::from(self.open.is_some() && self.product.as_ref().is_some_and(|product| !product.complete)) }
    }

    fn published(&self) -> Published {
        let Some(open) = &self.open else {
            return self.product.as_ref().map_or(Published::Nothing, |product| Published::Cadence(product.hour));
        };
        let seen = open.seen.as_ref().map(|frame| frame.hour);
        match (&self.product, seen) {
            (Some(product), Some(seen)) if product.hour >= seen => Published::Current,
            (Some(_), Some(seen)) if open.busy && open.evaluating == Some(seen) => Published::Current,
            (Some(product), _) => Published::Cadence(product.hour),
            (None, _) => Published::Nothing,
        }
    }

    fn held(&self) -> usize {
        self.needs.capacity() * std::mem::size_of::<Need>()
            + self.product.as_ref().map_or(0, |product| {
                product.raids.as_ref().map_or(0, |icons| icons.icons.capacity() * 256)
                    + product.notes.as_ref().map_or(0, |(_, notes)| notes.iter().map(String::capacity).sum::<usize>())
                    + product.music.as_ref().map_or(0, |(_, music)| music.weights.capacity() * 16 + music.refusals.iter().map(String::capacity).sum::<usize>())
            })
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn drawn<'a>(side: viewer_layer_core::playhead_host::LayerParts<'a>) -> Option<&'a Scripts> {
    side.drawn::<Scripts>(NAME)
}

pub fn raids(side: viewer_layer_core::playhead_host::LayerParts<'_>, branch: replay_data::BranchId, ours: u32) -> Option<Landed<Arc<RaidIcons>>> {
    drawn(side)?.raids(branch, ours)
}

pub fn notes(side: &MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>, ours: u32) -> Option<Arc<Vec<String>>> {
    drawn(side.layer_parts())?.notes(ours)
}

pub fn music(side: &MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Option<Landed<Arc<viewer_audio::Chances>>> {
    drawn(side.layer_parts())?.landed_music()
}

pub fn summary(side: &MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Value {
    let Some(layer) = side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<Scripts>())) else { return Value::Null };
    json!({
        "stats": layer.stats().summary(),
        "music_rate_a_second": MUSIC_RATE.makings_a_second(),
        "music_hour": layer.product().and_then(|product| product.music.as_ref().map(|(hour, _)| *hour)),
        "tables": layer.needs().iter().map(|need| format!("{:?}", need.table)).collect::<Vec<_>>(),
        "hour": layer.product().map(|product| product.hour),
        "cycle_complete": layer.product().map(|product| product.complete),
        "held": Layer::held(layer),
    })
}

#[cfg(test)]
mod tests;
