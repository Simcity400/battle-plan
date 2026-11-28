
#[cfg(test)]
mod tests;

use viewer_layer_core::layers::{Carried, Inputs, Layer, Published, Retirement, Settling, Upstream, View};
use viewer_layer_core::playhead_host::{mirror::{Mirror, Mirrors}, timeline::{self, Mark}, views::{self, Need}};
use playhead::Step;
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::{DecodedFrame, discovering};
use std::{
    fmt::Debug,
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_counter_products::map::orders;
use viewer_runtime::{memo::TableKey, name_draw::{Placements, WorldName}};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_gpu::ribbon_content::OrderField;

const WHOLE: Duration = Duration::from_secs(60);

const LOOK: Duration = Duration::from_millis(8);

pub trait Plans: Send + Sync {
    fn build(&self, frame: &DecodedFrame, ours: u32, show_allied: bool) -> io::Result<OrderField>;
}


pub fn differ_labels(today: &Placements, layer: &Placements) -> Option<String> {
    let Placements { country, warnings, generation: _, near } = today;
    for (at, (theirs, ours)) in country.iter().zip(&layer.country).enumerate() {
        if theirs.key != ours.key {
            return Some(format!("label {at}: today {:?}, the layer {:?}", theirs.key, ours.key));
        }
        if let Some((field, theirs_value, ours_value)) = differ_name(theirs, ours) {
            return Some(format!("label {:?} (at {at}), {field}: today {theirs_value}, the layer {ours_value}", theirs.key));
        }
    }
    if country.len() != layer.country.len() {
        let extra = |names: &[WorldName]| names.get(country.len().min(layer.country.len())).map(|name| name.key.clone());
        return Some(format!("{} labels today, {} the layer; the first unmatched today {:?}, the layer {:?}", country.len(), layer.country.len(), extra(country), extra(&layer.country)));
    }
    if let Some((field, theirs, ours)) = each("warnings", warnings, &layer.warnings) {
        return Some(format!("{field}: today {theirs}, the layer {ours}"));
    }
    match (near, &layer.near) {
        (None, None) => None,
        (Some(theirs), Some(ours)) => differ_labels(theirs, ours).map(|part| format!("near: {part}")),
        _ => Some(format!("near: today {}, the layer {}", near.is_some(), layer.near.is_some())),
    }
}

fn differ_name(today: &WorldName, layer: &WorldName) -> Option<(String, String, String)> {
    let WorldName { key: _, strategic_provinces, text, pages, corners, uvs, colors, aabb, wrapped, font_size, spaces, alpha } = today;
    whole("strategic_provinces", &strategic_provinces.is_some(), &layer.strategic_provinces.is_some())
        .or_else(|| each("strategic_provinces", strategic_provinces.as_deref().unwrap_or_default(), layer.strategic_provinces.as_deref().unwrap_or_default()))
        .or_else(|| whole("text", text, &layer.text))
        .or_else(|| each("pages", pages, &layer.pages))
        .or_else(|| each("corners", corners, &layer.corners))
        .or_else(|| each("uvs", uvs, &layer.uvs))
        .or_else(|| each("colors", colors, &layer.colors))
        .or_else(|| each("aabb", aabb, &layer.aabb))
        .or_else(|| whole("wrapped", wrapped, &layer.wrapped))
        .or_else(|| whole("font_size", font_size, &layer.font_size))
        .or_else(|| whole("spaces", spaces, &layer.spaces))
        .or_else(|| whole("alpha", alpha, &layer.alpha))
}

fn whole<T: Debug + ?Sized>(field: &str, today: &T, layer: &T) -> Option<(String, String, String)> {
    let (today, layer) = (format!("{today:?}"), format!("{layer:?}"));
    (today != layer).then(|| (field.to_owned(), today, layer))
}

fn each<T: Debug>(field: &str, today: &[T], layer: &[T]) -> Option<(String, String, String)> {
    if let Some(at) = today.iter().zip(layer).position(|(theirs, ours)| format!("{theirs:?}") != format!("{ours:?}")) {
        return Some((format!("{field}[{at}]"), format!("{:?}", today[at]), format!("{:?}", layer[at])));
    }
    (today.len() != layer.len()).then(|| (format!("{field} length"), today.len().to_string(), layer.len().to_string()))
}

#[derive(Clone)]
struct Made {
    generation: u64,
    stamp: u64,
    hour: Hour,
    ours: u32,
    stands: Hour,
    frame: Option<Arc<DecodedFrame>>,
    plan: Result<Arc<OrderField>, String>,
    notes: Vec<String>,
}

#[derive(Default)]
struct Held {
    asked: Option<(u32, bool)>,
    generation: u64,
    made: viewer_runtime::landed::PublishedKeyed<(u64, u64, Hour), Made>,
    published: u64,
    stopped: viewer_runtime::landed::Published<Option<String>>,
    marked: [u64; 3],
    stop: bool,
    #[cfg(test)]
    on_wait: Option<Box<dyn FnOnce() + Send>>,
}

impl Held {
    fn continuing(&self) -> io::Result<()> {
        if self.stop {
            Err(io::Error::new(io::ErrorKind::Interrupted, "The map's orders layer is retiring"))
        } else {
            Ok(())
        }
    }
}

struct Shared {
    held: Mutex<Held>,
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait(&self, until: Instant, published: u64, reader: &views::Reader, hour: Hour, strict: bool) -> io::Result<()> {
        let held = self.lock();
        held.continuing()?;
        if held.published != published || held.stopped.as_ref().is_some() || !rows_cover(reader, hour, strict) {
            return Ok(());
        }
        #[cfg(test)]
        let held = {
            let mut held = held;
            if let Some(on_wait) = held.on_wait.take() {
                on_wait();
            }
            held
        };
        let (held, _) = self.done.wait_timeout(held, until.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.continuing()
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.done.notify_all();
    }
}

fn rows_cover(reader: &views::Reader, hour: Hour, strict: bool) -> bool {
    if strict {
        return reader.has_rows_at(hour);
    }
    reader.newest().and_then(|(frame, _)| reader.through_seek(Arc::as_ptr(&frame)))
        .is_some_and(|(through, seek)| through >= hour && seek == reader.seek())
}

fn plan_notes(own: Result<usize, String>) -> Vec<String> {
    match own {
        Ok(drawn) => {
            let mut out = viewer_foundation::tables::Limitations::default();
            out.note_battle_plan(drawn);
            out.into_lines()
        }
        Err(error) => vec![format!("Field ArmyOrderPlans: the viewing country's own orders cannot be counted at the playback head, so what the drawn battle plan cannot take from the recording is not stated: {error}")],
    }
}

fn declare(shared: &Shared, reader: &views::Reader, declared: &mut Vec<TableId>, learned: &[TableId], show_allied: bool) {
    let mut tables = orders::memo_tables(show_allied);
    tables.extend(learned.iter().copied());
    tables.sort_unstable_by_key(|table| table.index());
    tables.dedup();
    if tables != *declared {
        *declared = tables;
        reader.declare_only(declared);
        let _held = shared.lock();
        shared.done.notify_all();
    }
}

fn work(shared: &Shared, plans: &dyn Plans, reader: &views::Reader, maker: timeline::Maker) {
    let mut declared = orders::memo_tables(false);
    let mut learned: Vec<TableId> = Vec::new();
    let mut built: Option<(TableKey, (u32, bool))> = None;
    let mut handled: Option<(u64, (u32, bool))> = None;
    let mut generation = 0;
    let mut build_seq = 0u64;
    loop {
        let (asked, want) = {
            let held = shared.lock();
            if held.stop {
                return;
            }
            (held.asked, held.generation)
        };
        if want != generation {
            (generation, built, handled) = (want, None, None);
        }
        let Some(asked) = asked else {
            reader.wait_newer(u64::MAX, LOOK);
            continue;
        };
        declare(shared, reader, &mut declared, &learned, asked.1);
        let seen = handled.map_or(0, |(published, _)| published);
        let Some((newest, published)) = reader.newest() else {
            reader.wait_newer(seen, LOOK);
            continue;
        };
        if handled == Some((published, asked)) {
            reader.wait_newer(published, LOOK);
            continue;
        }
        let Some((through, seek)) = reader.through_seek(Arc::as_ptr(&newest)) else {
            continue;
        };
        if seek != reader.seek() {
            reader.wait_newer(published, LOOK);
            continue;
        }
        handled = Some((published, asked));
        timeline::mark(maker, Mark::Rows);
        let stands = through.max(newest.hour);
        let key = (TableKey::of(&*newest, &declared), asked);
        if built.as_ref() == Some(&key) {
            let mut held = shared.lock();
            if held.generation == generation
                && let Some(mut made) = held.made.as_ref().filter(|made| made.generation == generation && stands > made.stands).cloned()
            {
                let stamp = held.published + 1;
                held.published = stamp;
                (made.stands, made.frame, made.stamp) = (stands, Some(Arc::clone(&newest)), stamp);
                held.made.publish((generation, build_seq, made.stands), made);
                reader.notify_progress();
            }
            drop(held);
            shared.done.notify_all();
            continue;
        }
        let (frame, (ours, show_allied)) = (newest, asked);
        timeline::mark(maker, Mark::BuildBegan);
        let started = Instant::now();
        let ((plan, own), read) = discovering(|| {
            let at = replay_frame::cpu::scope("plan.own_count", Some("plan"));
            let own = viewer_layer_core::scene_content::links_of(&frame).and_then(|links| orders::own_order_count(&frame, &links, ours));
            drop(at);
            (plans.build(&frame, ours, show_allied), own)
        });
        let micros = started.elapsed().as_micros() as u64;
        timeline::mark(maker, Mark::BuildEnded);
        viewer_runtime::view_record::add("arrows", "builds", 1);
        viewer_runtime::view_record::add("arrows", "build_us", micros);
        let missed: Vec<TableId> = read.into_iter().filter(|table| !declared.contains(table)).collect();
        if !missed.is_empty() {
            learned.extend(missed);
            let mut held = shared.lock();
            (held.marked[1], held.marked[2]) = (held.marked[1] + 1, held.marked[2] + micros);
            (built, handled) = (None, None);
            continue;
        }
        let mut held = shared.lock();
        held.marked[2] += micros;
        if held.stop {
            return;
        }
        if held.generation != generation {
            (built, handled) = (None, None);
            continue;
        }
        held.marked[0] += 1;
        let stamp = held.published + 1;
        held.published = stamp;
        build_seq += 1;
        built = Some(key);
        held.made.publish(
            (generation, build_seq, stands),
            Made {
                generation,
                stamp,
                hour: frame.hour,
                ours,
                stands,
                frame: Some(frame),
                plan: plan.map(Arc::new).map_err(|error| format!("The orders layer cannot make the plan: {error}")),
                notes: plan_notes(own.map_err(|error| error.to_string())),
            },
        );
        drop(held);
        shared.done.notify_all();
        reader.notify_progress();
    }
}

pub struct Orders {
    shared: Arc<Shared>,
    plans: Arc<dyn Plans>,
    worker: Mutex<Option<JoinHandle<()>>>,
    given: Mutex<Option<(Choice, Arc<Vision>)>>,
    vision: Option<(Choice, u32)>,
    rows: Mutex<Option<Arc<views::Reader>>>,
    maker: timeline::Maker,
    window: bool,
    show_allied: bool,
    head: Option<Hour>,
    made: Option<viewer_runtime::landed::Landed<Made>>,
    generation: u64,
    refused: Option<String>,
    marked: [u64; 3],
}

impl Orders {
    pub fn new(plans: Arc<dyn Plans>) -> Self {
        Self {
            shared: Arc::new(Shared { held: Mutex::new(Held::default()), done: Condvar::new() }),
            plans,
            worker: Mutex::new(None),
            given: Mutex::new(None),
            vision: None,
            rows: Mutex::new(None),
            maker: timeline::maker("battle_plan"),
            window: false,
            show_allied: false,
            head: None,
            made: None,
            generation: 0,
            refused: None,
            marked: [0; 3],
        }
    }

    pub fn set_views(&self, store: &views::Store) -> io::Result<()> {
        let tables = orders::memo_tables(false);
        let reader = Arc::new(store.reader("orders", &tables.iter().copied().map(Need::every).collect::<Vec<Need>>()));
        *self.rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&reader));
        let (shared, plans, maker) = (Arc::clone(&self.shared), Arc::clone(&self.plans), self.maker);
        let worker = std::thread::Builder::new().name("battle plan".into()).spawn(move || {
            viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
            work(&shared, &*plans, &reader, maker);
        });
        match worker {
            Ok(worker) => *self.worker.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(worker),
            Err(error) => self.shared.lock().stopped.set_some(format!("The orders layer is refused: its worker cannot start: {error}")),
        }
        Ok(())
    }

    pub fn in_window(mut self) -> Self {
        self.window = true;
        self
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        *self.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((choice, vision));
    }

    pub fn perspective(&self) -> Option<&Choice> {
        self.vision.as_ref().map(|(choice, _)| choice)
    }

    pub fn show_allied(&self) -> bool {
        self.show_allied
    }

    pub fn plan(&self) -> Option<(Hour, &Arc<OrderField>)> {
        self.shown().and_then(|made| made.plan.as_ref().ok().map(|plan| (made.hour, plan)))
    }

    pub fn landed_plan(&self) -> Option<viewer_runtime::landed::Landed<(Hour, Arc<OrderField>)>> {
        let made = self.shown_landed()?;
        let hour = made.hour;
        let plan = made.plan.as_ref().ok().cloned()?;
        Some(made.as_ref().map(|_| (hour, plan)))
    }

    pub fn refused(&self) -> Option<&str> {
        if let Some(why) = self.shown().and_then(|made| made.plan.as_ref().err()) {
            return Some(why);
        }
        self.refused.as_deref()
    }

    pub fn made_for(&self) -> Option<Hour> {
        self.shown().filter(|made| made.generation == self.generation).map(|made| made.stands)
    }

    pub fn warnings(&self) -> Vec<String> {
        match (self.plan(), self.refused()) {
            (Some((_, plan)), _) => plan.report.warnings(),
            (None, Some(why)) => vec![format!("Battle plan: not drawn: {why}")],
            (None, None) => vec!["Battle plan: loading at the playback head".to_owned()],
        }
    }

    pub fn notes(&self) -> &[String] {
        self.shown().map_or(&[], |made| &made.notes)
    }

    pub fn marked(&self) -> [u64; 3] {
        self.marked
    }

    fn settle_at(&mut self, hour: Hour, settling: Settling) -> io::Result<()> {
        let rows = self.rows.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
            .ok_or_else(|| io::Error::other("The orders layer has no rows at the head"))?;
        let until = Instant::now() + settling.waiting(WHOLE);
        let strict = settling.strict();
        self.take_vision();
        let began = Instant::now();
        let mut waited = false;
        loop {
            let published = self.take();
            if self.refused.is_some() {
                if strict {
                    return Err(io::Error::other(format!("{}: hour {hour} asked; {}", self.refused.as_deref().unwrap_or_default(), self.maker_state())));
                }
                return Ok(());
            }
            if self.made_for().is_some_and(|stands| stands == hour || (stands > hour && !strict)) {
                self.mark_join(began, waited, true);
                timeline::mark(self.maker, Mark::Joined);
                return Ok(());
            }
            let now = Instant::now();
            if now >= until {
                if strict {
                    return Err(io::Error::other(format!("The orders layer made no plan at {hour:?} in time: the plan it holds stands at {:?}; {}", self.made_for(), self.maker_state())));
                }
                self.mark_join(began, waited, false);
                timeline::mark(self.maker, Mark::Joined);
                return Ok(());
            }
            if !rows_cover(&rows, hour, strict) {
                viewer_runtime::view_record::add("arrows", "joins_left_rows_pending", 1);
                return self.shared.lock().continuing();
            }
            if viewer_runtime::view_record::on() {
                let held = rows.newest().is_some_and(|(frame, _)| frame.hour >= hour);
                viewer_runtime::view_record::add("arrows", if held { "waits_for_the_build" } else { "waits_for_the_rows" }, 1);
            }
            waited = true;
            self.shared.wait(if strict { until } else { until.min(now + LOOK) }, published, &rows, hour, strict)?;
        }
    }

    fn maker_state(&self) -> String {
        let reader = self.rows.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let newest = reader.as_ref().and_then(|reader| reader.newest());
        let standing = reader.as_ref().zip(newest.as_ref()).and_then(|(reader, (frame, _))| reader.through_seek(Arc::as_ptr(frame)));
        let held = self.shared.lock();
        format!(
            "the layer draws picture {} at head {:?} and holds {:?}; the worker published {}, made {:?} for {:?}, stopped {:?}; its rows: newest {:?}, standing through and at seek {:?} of seek {:?}, arriving {:?}",
            self.generation,
            self.head,
            self.made.as_ref().map(|made| (made.generation, made.hour, made.stands, made.plan.is_ok())),
            held.published,
            held.made.as_ref().map(|made| (made.generation, made.hour, made.stands)),
            held.asked,
            held.stopped.as_ref(),
            newest.as_ref().map(|(frame, published)| (*published, frame.hour)),
            standing,
            reader.as_ref().map(|reader| reader.seek()),
            reader.as_ref().map(|reader| reader.arriving()),
        )
    }

    fn mark_join(&self, began: Instant, waited: bool, joined: bool) {
        if !viewer_runtime::view_record::on() {
            return;
        }
        viewer_runtime::view_record::add("arrows", "joins", 1);
        viewer_runtime::view_record::add("arrows", "join_us", began.elapsed().as_micros() as u64);
        viewer_runtime::view_record::add("arrows", if waited { "joins_that_waited" } else { "joins_without_a_wait" }, 1);
        if !joined {
            viewer_runtime::view_record::add("arrows", "joins_out_of_budget", 1);
        }
    }

    fn take(&mut self) -> u64 {
        let published = {
            let held = self.shared.lock();
            self.marked = held.marked;
            if let Some(why) = held.stopped.as_ref() {
                self.refused = Some(why.clone());
            }
            if let Some(made) = held.made.as_ref().filter(|made| made.generation == self.generation)
                && self.made.as_ref().is_none_or(|ours| ours.stamp < made.stamp)
            {
                if made.plan.is_ok() && self.made.as_ref().is_none_or(|ours| made.hour != ours.hour) {
                    viewer_runtime::view_record::add("arrows", "installs", 1);
                    if self.made.as_ref().is_some_and(|ours| made.hour > ours.hour + 1) {
                        viewer_runtime::view_record::add("arrows", "steps_over_an_hour", 1);
                    }
                }
                self.made = held.made.landed_clone();
            }
            held.published
        };
        let reader = self.rows.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(made) = self.made.take() {
            let made = if let Some(frame) = &made.frame {
                let through = reader.and_then(|reader| reader.through(Arc::as_ptr(frame)));
                made.map(|mut made| {
                    match through {
                        Some(through) => made.stands = made.stands.max(through),
                        None => made.frame = None,
                    }
                    made
                })
            } else {
                made
            };
            self.made = Some(made);
        }
        published
    }

    fn shown_landed(&self) -> Option<&viewer_runtime::landed::Landed<Made>> {
        self.made.as_ref().filter(|made| self.vision.as_ref().is_some_and(|&(_, ours)| ours == made.ours))
    }

    fn shown(&self) -> Option<&Made> {
        self.shown_landed().map(|made| &**made)
    }

    fn take_vision(&mut self) {
        if let Some((choice, vision)) = self.given.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            self.vision = Some((choice, vision.ours()));
        }
        if let Some(&(_, ours)) = self.vision.as_ref() {
            let mut held = self.shared.lock();
            let asked = Some((ours, self.show_allied));
            if held.asked != asked {
                held.asked = asked;
            }
        }
    }
}

impl Drop for Orders {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(worker) = self.worker.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

impl Layer for Orders {
    fn retirement(&self) -> Option<Retirement> {
        let shared = Arc::clone(&self.shared);
        let rows = self.rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(Arc::downgrade);
        Some(Box::new(move || {
            shared.stop();
            if let Some(reader) = rows.and_then(|reader| reader.upgrade()) {
                reader.notify_progress();
            }
        }))
    }

    fn name(&self) -> &'static str {
        "orders"
    }

    fn views(&mut self, store: &views::Store) -> io::Result<()> {
        self.set_views(store)
    }

    fn placed(&mut self, _view: &viewer_layer_core::layers::View) -> Option<u64> {
        Some(0)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if self.window {
            inputs.after("vision");
        }
        inputs.table(TableId::ArmyOrders, true)?;
        self.show_allied = view.graphics.show_allied_plans;
        Ok(())
    }

    fn compose(&mut self, _mirrors: &Mirrors) -> io::Result<()> {
        self.generation += 1;
        self.shared.lock().generation = self.generation;
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.head = Some(step.last());
    }

    fn ask(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<bool> {
        if self.window
            && let Some(vision) = upstream.get::<viewer_layer_core::vision::Vision>()
        {
            match vision.ours() {
                Some(ours) if self.vision.as_ref().is_none_or(|(choice, held)| *held != ours || choice != vision.choice()) => {
                    self.vision = Some((vision.choice().clone(), ours));
                }
                None if self.vision.as_ref().is_some_and(|(choice, _)| choice != vision.choice()) => self.vision = None,
                _ => {}
            }
        }
        if let Some(head) = mirrors.get(TableId::ArmyOrders).and_then(Mirror::head) {
            self.head = Some(head);
        }
        self.take_vision();
        Ok(true)
    }

    fn join(&mut self, _mirrors: &Mirrors, _upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        if self.vision.is_none() {
            self.refused = Some("The orders layer is refused: it is given no vision of the viewing country yet".into());
            return Ok(());
        }
        self.refused = None;
        match self.head {
            Some(head) => self.settle_at(head, settling),
            None => {
                self.take();
                Ok(())
            }
        }
    }

    fn settle_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        self.ask(mirrors, upstream)?;
        self.join(mirrors, upstream, settling)
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.settle_with(mirrors, &Upstream::new(&[]), settling)
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> Published {
        let Some(stands) = self.made_for() else { return Published::Nothing };
        match self.head {
            Some(head) if stands < head => Published::Cadence(stands),
            _ => Published::Current,
        }
    }

    fn made_at(&self, hour: Hour) -> bool {
        self.perspective().is_none()
            || (self.made_for() == Some(hour)
                && self.rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|reader| reader.has_rows_at(hour)))
    }

    fn held(&self) -> usize {
        self.plan().map_or(0, |(_, plan)| std::mem::size_of_val(&plan.vertices[..]) + std::mem::size_of_val(&plan.native_vertices[..]) + std::mem::size_of_val(&plan.draws[..]))
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}
