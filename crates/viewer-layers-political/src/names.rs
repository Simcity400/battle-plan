
use viewer_layer_core::{layers::{Carried, Settling, Dirty, Due, Fed, Gpu, Inputs, Layer, Num, Published, Retirement, RetirementFlag, Text, View}, owned::{Shape, Table, Tables}, rebuilt::{Build, Built, Seen, Whole}};
use viewer_layer_core::playhead_host::{Entry, mirror::{Cell, Mirror, Mirrors}, views::Recording};
use engine_executable::executable_tables::Tables as Executable;
use replay_frame::frame::DecodedFrame;
use content_reader_country_names::country_names::{CountryNames, NamingAppearance, NamingRows, Operands, naming_operands};
use content_reader_localisation::localisation::Localisation;
use map_display_data::landscape::{Placements, WorldName};
use playhead::Step;
use replay_columnar::manifest::Status;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Instant,
};
use viewer_runtime::{landed::{self, Landed}, name_draw::next_generation};
use viewer_map_modes::map_mode::{MapMode, NameLayer, NameLayers};
use viewer_map_text::name_pass::{self, AreaText};

#[derive(Clone, Copy)]
struct Columns {
    controller: Num<u32>,
    link_slot: Num<u32>,
    country_slot: Num<u32>,
    tag: Text,
    cosmetic: Text,
    name_tag: Text,
    leader: Text,
    overlord: Num<i32>,
    ruling_party: Num<u32>,
    token: Text,
    autonomy: Text,
}

impl Columns {
    fn shapes(&self) -> Vec<Shape> {
        use TableId as T;
        vec![
            Shape::new(T::Provinces, &[self.controller.column], &[]),
            Shape::new(T::CountryLinks, &[self.link_slot.column], &[]),
            Shape::new(T::Countries, &[self.country_slot.column], &[]).with_texts(&[self.tag.column]),
            Shape::new(T::CountryAppearance, &[self.overlord.column], &[]).with_texts(&[self.cosmetic.column, self.name_tag.column, self.leader.column]),
            Shape::new(T::Politics, &[self.ruling_party.column], &[]),
            Shape::new(T::PoliticalParties, &[], &[]).with_texts(&[self.token.column]),
            Shape::new(T::Autonomy, &[], &[]).with_texts(&[self.autonomy.column]),
        ]
    }
}

const CORE: [TableId; 3] = [TableId::Provinces, TableId::CountryLinks, TableId::Countries];
const NAMING: [TableId; 4] = [TableId::CountryAppearance, TableId::Politics, TableId::PoliticalParties, TableId::Autonomy];

#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub members: BTreeSet<u32>,
    pub tag: Option<String>,
    pub text: Result<String, String>,
    pub placed: Vec<(Vec<u32>, Option<Arc<WorldName>>)>,
    placed_as: Option<(String, String)>,
}

trait Target {
    fn keyed(&self) -> Option<u64>;
    fn begin(&mut self, generation: u64);
    fn set(&mut self, generation: u64, slot: u32, names: &[&WorldName]) -> io::Result<()>;
    fn install(&mut self, placements: &Placements) -> io::Result<()>;
    fn holds(&self, placements: &Placements) -> bool;
}

struct OnScene<'a, 'b>(&'a mut Gpu<'b>);

impl Target for OnScene<'_, '_> {
    fn keyed(&self) -> Option<u64> {
        self.0.scene.country_names_keyed()
    }

    fn begin(&mut self, generation: u64) {
        self.0.scene.begin_country_names(generation);
    }

    fn set(&mut self, generation: u64, slot: u32, names: &[&WorldName]) -> io::Result<()> {
        let gpu = &mut *self.0;
        gpu.scene.set_country_names(gpu.device, gpu.queue, generation, u64::from(slot), names)
    }

    fn install(&mut self, placements: &Placements) -> io::Result<()> {
        let gpu = &mut *self.0;
        gpu.scene.set_hour_layers(gpu.device, gpu.queue, viewer_world::scene::HourLayers { names: Some(placements), ..Default::default() })
    }

    fn holds(&self, placements: &Placements) -> bool {
        self.0.scene.holds_country_names(placements)
    }
}

fn class(status: Status) -> u8 {
    match status {
        Status::Complete => 0,
        Status::Truncated { .. } => 1,
        _ => 2,
    }
}

fn rows(table: Option<&Table>) -> Option<&Table> {
    table.filter(|table| matches!(table.status(), Some(Status::Complete | Status::Truncated { .. })))
}

fn bytes<'t>(table: &'t Table, column: Text, slot: u32, what: &str) -> Result<&'t [u8], String> {
    match table.named_text(column, slot) {
        Ok(Cell::Value(bytes)) => Ok(bytes),
        Ok(_) => Ok(&[]),
        Err(id) => Err(format!("{what}: string {id} is not named at the head")),
    }
}

pub trait Atlas: Send + Sync {
    fn provinces(&self) -> Vec<u32>;
    fn localisation(&self) -> Arc<Localisation>;
    fn components(&self, members: &BTreeSet<u32>) -> Vec<Vec<u32>>;
    fn place(&self, tag: &str, text: &str, component: &[u32]) -> Option<WorldName>;
    fn name_layers(&self, mode: &MapMode) -> Option<NameLayers>;
    fn names_cutoff(&self) -> f32;
    fn mode_names(&self, frame: &DecodedFrame, mode: &MapMode) -> io::Result<Arc<Placements>>;
    fn executable(&self) -> Option<Executable>;
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Countries,
    Whole,
    Nothing,
}

fn kind(atlas: &dyn Atlas, mode: &MapMode) -> Result<Kind, String> {
    let layers = atlas.name_layers(mode).ok_or_else(|| format!("map mode {} is not declared by the recording's mod stack", mode.token()))?;
    Ok(match (layers.far, layers.near) {
        (NameLayer::Nothing, NameLayer::Nothing) => Kind::Nothing,
        (NameLayer::Countries, NameLayer::Nothing) => Kind::Countries,
        _ => Kind::Whole,
    })
}

pub struct ModeNames {
    atlas: Arc<dyn Atlas>,
    mode: Mutex<MapMode>,
}

impl Build for ModeNames {
    type Product = Arc<Placements>;

    fn tables(&self) -> Option<Executable> {
        self.atlas.executable()
    }

    fn build(&self, frame: &DecodedFrame, _seen: &Seen) -> io::Result<Arc<Placements>> {
        let mode = self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        self.atlas.mode_names(frame, &mode)
    }

    fn viewed(&self, view: &View) -> bool {
        let mut mode = self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *mode == view.map_mode {
            return false;
        }
        *mode = view.map_mode.clone();
        true
    }
}

fn whole_drawn(build: &ModeNames, view: &View) -> bool {
    kind(&*build.atlas, &view.map_mode) == Ok(Kind::Whole)
}

fn placement_bytes(placements: &Arc<Placements>) -> usize {
    placements.country.iter().map(|name| name.key.capacity() + name.text.capacity() + name.pages.capacity() * 8 + name.corners.capacity() * 12 + name.uvs.capacity() * 16 + 96).sum()
}

fn nothing() -> Arc<Placements> {
    Arc::new(Placements { generation: next_generation(), ..Placements::default() })
}

const PLACERS: usize = 2;

struct Placing {
    slot: u32,
    tag: String,
    shown: String,
    members: BTreeSet<u32>,
    kept: Vec<(Vec<u32>, Option<Arc<WorldName>>)>,
}

struct Placed {
    slot: u32,
    tag: String,
    shown: String,
    placed: Vec<(Vec<u32>, Option<Arc<WorldName>>)>,
    ns: u64,
}

struct Placers {
    queue: Arc<(Mutex<(VecDeque<Placing>, bool)>, Condvar)>,
    done: mpsc::Receiver<Placed>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Placers {
    fn new(atlas: &Arc<dyn Atlas>) -> Self {
        let queue: Arc<(Mutex<(VecDeque<Placing>, bool)>, Condvar)> = Arc::default();
        let (sender, done) = mpsc::channel();
        let threads = (0..PLACERS)
            .filter_map(|_| {
                let (held, atlas, sender) = (Arc::clone(&queue), Arc::clone(atlas), sender.clone());
                thread::Builder::new().name("names placement".into()).spawn(move || {
                    viewer_runtime::worker_priority::background_priority();
                    place_jobs(&held, &*atlas, &sender)
                }).ok()
            })
            .collect();
        Self { queue, done, threads }
    }

    fn place(&self, atlas: &dyn Atlas, jobs: Vec<Placing>) -> Vec<Placed> {
        if self.threads.is_empty() {
            return jobs.into_iter().map(|job| placing(atlas, job)).collect();
        }
        let count = jobs.len();
        {
            let (lock, wake) = &*self.queue;
            lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.extend(jobs);
            wake.notify_all();
        }
        self.done.iter().take(count).collect()
    }
}

fn placing(atlas: &dyn Atlas, job: Placing) -> Placed {
    let started = Instant::now();
    let placed = place_area(atlas, &job.tag, &job.shown, &job.members, job.kept);
    Placed { slot: job.slot, tag: job.tag, shown: job.shown, placed, ns: started.elapsed().as_nanos() as u64 }
}

fn place_jobs(queue: &(Mutex<(VecDeque<Placing>, bool)>, Condvar), atlas: &dyn Atlas, sender: &mpsc::Sender<Placed>) {
    let (lock, wake) = queue;
    loop {
        let job = {
            let mut queue = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if queue.1 {
                    return;
                }
                if let Some(job) = queue.0.pop_front() {
                    break job;
                }
                queue = wake.wait(queue).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if sender.send(placing(atlas, job)).is_err() {
            return;
        }
    }
}

impl Drop for Placers {
    fn drop(&mut self) {
        let (lock, wake) = &*self.queue;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 = true;
        wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn place_area(atlas: &dyn Atlas, tag: &str, shown: &str, members: &BTreeSet<u32>, kept: Vec<(Vec<u32>, Option<Arc<WorldName>>)>) -> Vec<(Vec<u32>, Option<Arc<WorldName>>)> {
    let mut kept: HashMap<Vec<u32>, Option<Arc<WorldName>>> = kept.into_iter().collect();
    atlas
        .components(members)
        .into_iter()
        .map(|component| {
            let name = match kept.remove(&component) {
                Some(name) => name,
                None => atlas.place(tag, shown, &component).map(Arc::new),
            };
            (component, name)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Latency {
    pub buckets: [u64; 7],
    pub worst_frames: u64,
    pub worker_areas: u64,
    pub worker_us: u64,
    pub worker_worst_us: u64,
    pub worker_buckets: [u64; 5],
    pub in_flight_peak: u64,
}

struct Model {
    content: Arc<dyn Atlas>,
    defined: Vec<u32>,
    columns: Columns,
    tables: Tables,
    refused: Option<String>,
    slot_of: Vec<u32>,
    links: HashMap<u32, u32>,
    natives: BTreeSet<(u32, u32)>,
    native_slot: HashMap<u32, u32>,
    operands: BTreeMap<u32, Result<Operands, String>>,
    areas: BTreeMap<u32, Area>,
    provinces: Dirty,
    operand_slots: Dirty,
    places: BTreeSet<u32>,
    kept_marks: u64,
    controller_of: Vec<u32>,
    by_controller: HashMap<u32, BTreeSet<u32>>,
    link_raws: BTreeSet<u32>,
    country_natives: BTreeSet<u32>,
    every_operand: bool,
    texts_dirty: bool,
    changed_operands: BTreeSet<u32>,
    text_slots: BTreeSet<u32>,
    statuses: HashMap<usize, Status>,
    frame: u64,
    marked: [usize; 3],
    touched: BTreeSet<u32>,
    whole: bool,
    stamp: Option<Hour>,
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    whole: bool,
    areas: Vec<(u32, Option<Area>)>,
    refused: Option<String>,
    marked: [usize; 3],
    placed: Vec<u64>,
    held: usize,
}

impl Model {
    fn new(content: Arc<dyn Atlas>, columns: Columns, tables: Tables) -> Self {
        Self {
            defined: content.provinces(),
            content,
            columns,
            tables,
            refused: Some("composing".into()),
            slot_of: Vec::new(),
            links: HashMap::new(),
            natives: BTreeSet::new(),
            native_slot: HashMap::new(),
            operands: BTreeMap::new(),
            areas: BTreeMap::new(),
            provinces: Dirty::default(),
            operand_slots: Dirty::default(),
            places: BTreeSet::new(),
            kept_marks: 0,
            controller_of: Vec::new(),
            by_controller: HashMap::new(),
            link_raws: BTreeSet::new(),
            country_natives: BTreeSet::new(),
            every_operand: false,
            texts_dirty: false,
            changed_operands: BTreeSet::new(),
            text_slots: BTreeSet::new(),
            statuses: HashMap::new(),
            frame: 0,
            marked: [0; 3],
            touched: BTreeSet::new(),
            whole: true,
            stamp: None,
        }
    }

    fn core(&self) -> Option<String> {
        for table in CORE {
            let Some(held) = self.tables.get(table) else { return Some(format!("{table:?} is not read yet")) };
            if held.status() != Some(Status::Complete) {
                let why = held.not_recorded().unwrap_or_else(|| format!("{table:?} is not wholly recorded at this hour"));
                return Some(format!("{why}; the core tables Provinces, CountryLinks and Countries carry the political map itself, so this hour cannot be drawn"));
            }
        }
        None
    }

    fn restart(&mut self) {
        self.slot_of.clear();
        self.whole = true;
        self.areas.clear();
        self.operands.clear();
        self.provinces.clear();
        self.operand_slots.clear();
        self.places.clear();
        self.by_controller.clear();
        self.link_raws.clear();
        self.country_natives.clear();
        self.statuses = NAMING.iter().filter_map(|&table| self.tables.get(table).and_then(Table::status).map(|status| (table.index(), status))).collect();
        self.read_links();
        self.read_countries();
        let max = self.defined.last().copied().unwrap_or(0);
        self.slot_of.resize(max as usize + 1, 0);
        self.controller_of.clear();
        self.controller_of.resize(max as usize + 1, 0);
        for &id in &self.defined {
            self.provinces.mark(id, self.frame);
        }
        self.every_operand = true;
    }

    fn owe(&mut self, slot: u32) {
        self.touched.insert(slot);
    }

    fn settle_link(&mut self, raw: u32) {
        let new = self.tables.get(TableId::CountryLinks).and_then(|links| links.number(self.columns.link_slot, links.slot(u128::from(raw))?).value());
        let old = match new {
            Some(slot) => self.links.insert(raw, slot),
            None => self.links.remove(&raw),
        };
        if old == new {
            return;
        }
        let ruled: Vec<u32> = self.by_controller.get(&raw).into_iter().flatten().copied().collect();
        ruled.into_iter().for_each(|id| {
            self.provinces.mark(id, self.frame);
        });
        self.mark_followers(&[old, new]);
    }

    fn mark_followers(&mut self, slots: &[Option<u32>]) {
        let wanted: Vec<u32> = slots.iter().flatten().copied().collect();
        let marked: Vec<u32> = self.operands.iter().filter(|(_, operands)| operands.as_ref().map_or(true, |operands| operands.overlord.is_some_and(|overlord| wanted.contains(&overlord)))).map(|(&slot, _)| slot).collect();
        let marked = marked.into_iter().chain(wanted.iter().copied());
        for slot in marked.collect::<Vec<_>>() {
            if self.operand_slots.mark(slot, self.frame) {
                self.marked[1] += 1;
            }
        }
    }

    fn settle_country(&mut self, native: u32) {
        let new = self.tables.get(TableId::Countries).and_then(|countries| countries.number(self.columns.country_slot, countries.slot(u128::from(native))?).value());
        let old = match new {
            Some(slot) => self.native_slot.insert(native, slot),
            None => self.native_slot.remove(&native),
        };
        if let Some(old) = old {
            self.natives.remove(&(old, native));
        }
        if let Some(new) = new {
            self.natives.insert((new, native));
        }
        self.mark_followers(&[old, new]);
        self.texts_dirty = true;
    }

    fn read_links(&mut self) {
        let column = self.columns.link_slot;
        self.links.clear();
        if let Some(links) = self.tables.get(TableId::CountryLinks) {
            for slot in links.rows() {
                if let (Some(key), Some(canonical)) = (links.key(slot), links.number(column, slot).value()) {
                    self.links.insert(key as u32, canonical);
                }
            }
        }
    }

    fn read_countries(&mut self) {
        let column = self.columns.country_slot;
        self.natives.clear();
        self.native_slot.clear();
        if let Some(countries) = self.tables.get(TableId::Countries) {
            for slot in countries.rows() {
                if let (Some(key), Some(canonical)) = (countries.key(slot), countries.number(column, slot).value()) {
                    self.natives.insert((canonical, key as u32));
                    self.native_slot.insert(key as u32, canonical);
                }
            }
        }
    }

    fn by_slot(&self, slot: u32) -> Option<u32> {
        self.natives.range((slot, 0)..=(slot, u32::MAX)).next_back().map(|&(_, native)| native)
    }

    fn country_tag(&self, native: u32) -> Result<Vec<u8>, String> {
        let countries = self.tables.get(TableId::Countries).ok_or("Countries is not read")?;
        let row = countries.slot(u128::from(native)).ok_or_else(|| format!("country {native} has no Countries row"))?;
        bytes(countries, self.columns.tag, row, "country tag").map(<[u8]>::to_vec)
    }

    fn operands_of(&self, native: u32) -> Result<Operands, String> {
        let own = self.country_tag(native)?;
        naming_operands(&TableRows { model: self }, native, &own)
    }

    fn settle_operands(&mut self, slot: u32) {
        let operands = self.by_slot(slot).map(|native| self.operands_of(native));
        let changed = match operands {
            Some(operands) => self.operands.insert(slot, operands.clone()).as_ref() != Some(&operands),
            None => self.operands.remove(&slot).is_some(),
        };
        if changed {
            self.changed_operands.insert(slot);
        }
    }

    fn settle_province(&mut self, id: u32) {
        let at = id as usize;
        if at >= self.slot_of.len() || self.defined.binary_search(&id).is_err() {
            return;
        }
        let provinces = self.tables.get(TableId::Provinces).expect("the core is read");
        let controller = provinces.slot(u128::from(id)).and_then(|row| provinces.number(self.columns.controller, row).value()).unwrap_or(0);
        let ruler = std::mem::replace(&mut self.controller_of[at], controller);
        if ruler != controller {
            if let Some(set) = self.by_controller.get_mut(&ruler) {
                set.remove(&id);
                if set.is_empty() {
                    self.by_controller.remove(&ruler);
                }
            }
            self.by_controller.entry(controller).or_default().insert(id);
        }
        let slot = if controller > 0 { self.links.get(&controller).map_or(0, |&slot| slot + 1) } else { 0 };
        let held = self.slot_of[at];
        if held == slot {
            return;
        }
        self.slot_of[at] = slot;
        if held != 0
            && let Some(area) = self.areas.get_mut(&(held - 1))
        {
            area.members.remove(&id);
            self.places.insert(held - 1);
        }
        if slot != 0 {
            let fresh = !self.areas.contains_key(&(slot - 1));
            self.areas
                .entry(slot - 1)
                .or_insert_with(|| Area { members: BTreeSet::new(), tag: None, text: Err(String::new()), placed: Vec::new(), placed_as: None })
                .members
                .insert(id);
            if fresh {
                self.text_slots.insert(slot - 1);
            }
            self.places.insert(slot - 1);
        }
    }

    fn settle_texts(&mut self) {
        let all = std::mem::take(&mut self.texts_dirty);
        let changed = std::mem::take(&mut self.changed_operands);
        let fresh = std::mem::take(&mut self.text_slots);
        let reads = |slot: u32, operands: &BTreeMap<u32, Result<Operands, String>>| {
            let mut at = Some(slot);
            for _ in 0..=operands.len() {
                let Some(slot) = at else { return false };
                if changed.contains(&slot) {
                    return true;
                }
                at = operands.get(&slot).and_then(|operands| operands.as_ref().ok()).and_then(|operands| operands.overlord);
            }
            false
        };
        let slots: Vec<u32> = self.areas.keys().copied().filter(|&slot| all || fresh.contains(&slot) || reads(slot, &self.operands)).collect();
        if slots.is_empty() {
            return;
        }
        let localisation = self.content.localisation();
        let mut operands = std::mem::take(&mut self.operands);
        let zero = !operands.contains_key(&0);
        if zero {
            operands.insert(0, Err("country slot zero's reserved naming operands are not read by the names layer".into()));
        }
        let names = CountryNames::recorded(&localisation, operands, None);
        let texts: Vec<(u32, Result<String, String>)> = slots.iter().map(|&slot| (slot, names.name(slot))).collect();
        self.operands = names.into_operands();
        if zero {
            self.operands.remove(&0);
        }
        for (slot, text) in texts {
            let tag = self.by_slot(slot).and_then(|native| self.country_tag(native).ok()).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let area = self.areas.get_mut(&slot).expect("listed");
            if area.tag != tag || area.text != text {
                area.tag = tag;
                area.text = text;
                self.owe(slot);
                self.places.insert(slot);
            }
        }
    }

    fn placing(&mut self, slot: u32) -> Option<Placing> {
        self.marked[2] += 1;
        let area = self.areas.get(&slot)?;
        if area.members.is_empty() {
            self.owe(slot);
            self.areas.remove(&slot);
            return None;
        }
        let Some(tag) = area.tag.clone() else {
            let area = self.areas.get_mut(&slot).expect("held");
            area.placed.clear();
            area.placed_as = None;
            self.owe(slot);
            return None;
        };
        let shown = match name_pass::area_text(&tag, &area.text, false) {
            AreaText::Shown(text) | AreaText::Tag(text, _) => text.to_owned(),
            AreaText::Withheld(_) => unreachable!("a country's area is not a group's"),
        };
        let kept = if area.placed_as.as_ref() == Some(&(tag.clone(), shown.clone())) { area.placed.clone() } else { Vec::new() };
        Some(Placing { slot, tag, shown, members: area.members.clone(), kept })
    }

    fn settle(&mut self, placers: &Placers, placing: bool) -> Vec<u64> {
        self.frame += 1;
        match self.core() {
            Some(why) => {
                if self.refused.as_ref() != Some(&why) {
                    self.restart();
                    self.provinces.clear();
                    self.every_operand = false;
                    self.refused = Some(why);
                }
                return Vec::new();
            }
            None => {
                if self.refused.take().is_some() {
                    self.restart();
                }
            }
        }
        for raw in std::mem::take(&mut self.link_raws) {
            self.settle_link(raw);
        }
        for native in std::mem::take(&mut self.country_natives) {
            self.settle_country(native);
        }
        let mut dirty = std::mem::take(&mut self.provinces);
        let _ = dirty.settle(|id| {
            self.settle_province(id);
            Ok(())
        });
        self.provinces = dirty;
        if self.every_operand {
            self.every_operand = false;
            self.operand_slots.clear();
            let slots: BTreeSet<u32> = self.natives.iter().map(|&(slot, _)| slot).chain(self.operands.keys().copied()).collect();
            slots.into_iter().for_each(|slot| self.settle_operands(slot));
            self.texts_dirty = true;
        }
        let mut dirty = std::mem::take(&mut self.operand_slots);
        let _ = dirty.settle(|slot| {
            self.settle_operands(slot);
            Ok(())
        });
        self.operand_slots = dirty;
        self.settle_texts();
        if !placing {
            self.kept_marks = self.places.len() as u64;
            return Vec::new();
        }
        self.kept_marks = 0;
        let jobs: Vec<Placing> = std::mem::take(&mut self.places).into_iter().filter_map(|slot| self.placing(slot)).collect();
        let mut times = Vec::with_capacity(jobs.len());
        for done in placers.place(&*self.content, jobs) {
            times.push(done.ns);
            if let Some(area) = self.areas.get_mut(&done.slot) {
                area.placed = done.placed;
                area.placed_as = Some((done.tag, done.shown));
                self.owe(done.slot);
            }
        }
        times
    }

    fn step(&mut self, step: &Step) {
        self.stamp = self.stamp.max(Some(step.last()));
        if step.gap() {
            self.tables.gap();
            return;
        }
        let columns = self.columns;
        for changes in step.tables() {
            let table = TableId::ALL[changes.table()];
            let Some(held) = self.tables.get_mut(table) else { continue };
            let _ = held.step(changes);
            if NAMING.contains(&table)
                && let Some((status, _)) = changes.status()
                && self.statuses.insert(changes.table(), status).map(class) != Some(class(status))
            {
                self.every_operand = true;
            }
            let held = self.tables.get(table).expect("kept");
            let lives = changes.deleted().keys().unwrap_or_default().iter().chain(changes.inserted().keys().unwrap_or_default()).copied();
            let wanted: &[usize] = match table {
                TableId::Provinces => &[columns.controller.column],
                TableId::CountryLinks => &[columns.link_slot.column],
                TableId::Countries => &[columns.country_slot.column, columns.tag.column],
                TableId::CountryAppearance => &[columns.cosmetic.column, columns.name_tag.column, columns.leader.column, columns.overlord.column],
                TableId::Politics => &[columns.ruling_party.column],
                TableId::PoliticalParties => &[columns.token.column],
                TableId::Autonomy => &[columns.autonomy.column],
                _ => continue,
            };
            let written: Vec<u128> = changes.columns().iter().filter(|written| wanted.contains(&written.column())).flat_map(|written| written.slots().iter().filter_map(|&slot| held.key(slot))).collect();
            let keys: Vec<u128> = lives.chain(written).collect();
            match table {
                TableId::Provinces => {
                    for key in keys {
                        if self.provinces.mark(key as u32, self.frame) {
                            self.marked[0] += 1;
                        }
                    }
                }
                TableId::CountryLinks => self.link_raws.extend(keys.into_iter().map(|key| key as u32)),
                TableId::Countries => self.country_natives.extend(keys.into_iter().map(|key| key as u32)),
                TableId::PoliticalParties => keys.into_iter().for_each(|key| self.mark_native((key >> 32) as u32)),
                _ => keys.into_iter().for_each(|key| self.mark_native(key as u32)),
            }
        }
    }

    fn mark_native(&mut self, native: u32) {
        if let Some(&slot) = self.native_slot.get(&native)
            && self.operand_slots.mark(slot, self.frame)
        {
            self.marked[1] += 1;
        }
    }

    fn made(&mut self, epoch: u64, placed: Vec<u64>) -> Made {
        let whole = std::mem::take(&mut self.whole);
        let touched = std::mem::take(&mut self.touched);
        let areas = if whole { self.areas.iter().map(|(&slot, area)| (slot, Some(area.clone()))).collect() } else { touched.into_iter().map(|slot| (slot, self.areas.get(&slot).cloned())).collect() };
        Made { epoch, stamp: self.stamp, whole, areas, refused: self.refused.clone(), marked: std::mem::take(&mut self.marked), placed, held: self.held() }
    }

    fn held(&self) -> usize {
        self.tables.bytes()
            + self.areas.values().map(area_bytes).sum::<usize>()
            + self.slot_of.capacity() * 4
            + (self.links.capacity() + self.native_slot.capacity()) * 12
            + self.natives.len() * 16
            + self.operands.values().map(|operands| operands.as_ref().map_or(64, |operands| operands.heap_bytes() + 96)).sum::<usize>()
            + self.provinces.bytes()
            + self.operand_slots.bytes()
            + self.controller_of.capacity() * 4
            + self.by_controller.values().map(|set| set.len() * 8 + 48).sum::<usize>()
    }
}

fn area_bytes(area: &Area) -> usize {
    let name_bytes = |name: &WorldName| name.key.capacity() + name.text.capacity() + name.pages.capacity() * 8 + name.corners.capacity() * 12 + name.uvs.capacity() * 16 + name.colors.capacity() * 16 + 96;
    area.members.len() * 8
        + area.tag.as_ref().map_or(0, String::capacity)
        + area.text.as_ref().map_or_else(String::capacity, String::capacity)
        + area.placed.iter().map(|(component, name)| component.capacity() * 4 + name.as_ref().map_or(0, |name| name_bytes(name))).sum::<usize>()
        + 96
}

struct TableRows<'a> {
    model: &'a Model,
}

impl TableRows<'_> {
    fn row(&self, table: TableId, key: u128) -> Option<(&Table, u32)> {
        let held = rows(self.model.tables.get(table))?;
        Some((held, held.slot(key)?))
    }

    fn text(&self, table: TableId, key: u128, column: Text, what: &str) -> Result<Option<Vec<u8>>, String> {
        self.row(table, key).map(|(held, row)| bytes(held, column, row, what).map(<[u8]>::to_vec)).transpose()
    }
}

impl NamingRows for TableRows<'_> {
    fn appearance(&self, id: u32) -> Result<Option<NamingAppearance>, String> {
        let columns = self.model.columns;
        let Some((held, row)) = self.row(TableId::CountryAppearance, u128::from(id)) else { return Ok(None) };
        let text = |column, what| bytes(held, column, row, what).map(<[u8]>::to_vec);
        Ok(Some(NamingAppearance {
            name_tag: text(columns.name_tag, "original tag")?,
            cosmetic_tag: text(columns.cosmetic, "cosmetic tag")?,
            leader_ideology: text(columns.leader, "leader ideology")?,
            overlord: held.number(columns.overlord, row).value().unwrap_or(0),
        }))
    }

    fn ruling_party(&self, id: u32) -> Result<Option<Option<u32>>, String> {
        Ok(self.row(TableId::Politics, u128::from(id)).map(|(held, row)| held.number(self.model.columns.ruling_party, row).value()))
    }

    fn party_token(&self, id: u32, party: u32) -> Result<Option<Vec<u8>>, String> {
        self.text(TableId::PoliticalParties, u128::from(id) << 32 | u128::from(party), self.model.columns.token, "ideology group")
    }

    fn link(&self, raw: u32) -> Result<Option<u32>, String> {
        Ok(self.model.links.get(&raw).copied())
    }

    fn autonomy(&self, id: u32) -> Result<Option<Vec<u8>>, String> {
        self.text(TableId::Autonomy, u128::from(id), self.model.columns.autonomy, "autonomy")
    }

    fn slot_tag(&self, slot: u32) -> Result<Option<Vec<u8>>, String> {
        self.model.by_slot(slot).map(|native| self.model.country_tag(native)).transpose()
    }

    fn complete(&self, table: TableId) -> bool {
        self.model.tables.get(table).is_some_and(|held| held.status() == Some(Status::Complete))
    }
}

enum Job {
    Reset { epoch: u64, tables: Box<Tables>, columns: Columns, stamp: Option<Hour>, placing: bool },
    Step { epoch: u64, step: Arc<Step> },
    Placing(bool),
}

fn superseded(next: Option<&Job>, epoch: u64, due: &Due) -> bool {
    match next {
        Some(Job::Reset { .. }) => true,
        Some(Job::Step { epoch: of, step }) => *of == epoch && due.reached(step.last()),
        Some(Job::Placing(_)) | None => false,
    }
}

fn run(content: Arc<dyn Atlas>, jobs: mpsc::Receiver<Job>, made: landed::Sender<Made>, due: Due) {
    let placers = Placers::new(&content);
    let mut model: Option<Model> = None;
    let mut epoch = 0;
    let mut placing = true;
    while let Ok(job) = jobs.recv() {
        let mut queue: VecDeque<Job> = std::iter::once(job).chain(jobs.try_iter()).collect();
        while let Some(job) = queue.pop_front() {
            match job {
                Job::Reset { epoch: next, tables, columns, stamp, placing: drawn } => {
                    (epoch, placing) = (next, drawn);
                    let mut made = Model::new(Arc::clone(&content), columns, *tables);
                    made.stamp = stamp;
                    model = Some(made);
                }
                Job::Step { epoch: of, step } if of == epoch => {
                    let Some(model) = &mut model else { continue };
                    model.step(&step);
                    drop(step);
                    if superseded(queue.front(), epoch, &due) {
                        continue;
                    }
                }
                Job::Placing(drawn) => {
                    placing = drawn;
                    if !drawn || model.is_none() {
                        continue;
                    }
                }
                Job::Step { .. } => continue,
            }
            let Some(model) = &mut model else { continue };
            let placed = model.settle(&placers, placing);
            if made.send(model.made(epoch, placed)).is_err() {
                return;
            }
        }
    }
}

pub struct Names {
    retiring: RetirementFlag,
    content: Arc<dyn Atlas>,
    columns: Option<Columns>,
    refused: Option<String>,
    areas: BTreeMap<u32, Area>,
    warned: Option<Vec<String>>,
    jobs: Option<mpsc::Sender<Job>>,
    made: Option<landed::Receiver<Made>>,
    epoch: u64,
    fed: Fed<Made>,
    applied_at: VecDeque<(Hour, u64)>,
    latency: Latency,
    worker_held: usize,
    drawn: Arc<Placements>,
    landed_drawn: Option<Landed<Arc<Placements>>>,
    made_token: Option<Landed<()>>,
    drawn_stale: bool,
    sent: Option<u64>,
    unsent: VecDeque<u32>,
    unsent_held: HashSet<u32>,
    frame: u64,
    marked: [usize; 3],
    kind: Kind,
    mode: MapMode,
    restart: bool,
    placing: bool,
    modes: Option<Whole<ModeNames>>,
    whole_shown: Option<Landed<Arc<Built<Arc<Placements>>>>>,
    whole_pending: Option<Landed<Arc<Placements>>>,
    kept: VecDeque<Arc<Step>>,
    last_epoch: u64,
}

impl Names {
    pub fn new(content: Arc<dyn Atlas>) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            kind: Kind::Countries,
            mode: MapMode::default(),
            restart: false,
            placing: true,
            modes: None,
            whole_shown: None,
            whole_pending: None,
            kept: VecDeque::new(),
            last_epoch: 0,
            content,
            columns: None,
            refused: Some("not composed yet".into()),
            areas: BTreeMap::new(),
            jobs: None,
            made: None,
            epoch: 0,
            fed: Fed::default(),
            applied_at: VecDeque::new(),
            latency: Latency::default(),
            worker_held: 0,
            drawn: Arc::default(),
            landed_drawn: None,
            made_token: None,
            drawn_stale: true,
            warned: None,
            sent: None,
            unsent: VecDeque::new(),
            unsent_held: HashSet::new(),
            frame: 0,
            marked: [0; 3],
        }
    }

    pub fn in_window(mut self, entry: Arc<Entry>, recording: Recording) -> io::Result<Self> {
        let build = ModeNames { atlas: Arc::clone(&self.content), mode: Mutex::new(MapMode::default()) };
        let tables: Vec<TableId> = CORE.into_iter().chain(NAMING).collect();
        let modes = Whole::new("mode names", build, whole_drawn, placement_bytes, &tables, false)?.exact();
        self.modes = Some(modes.in_window(entry, recording)?);
        Ok(self)
    }

    pub fn fresh(&self) -> Self {
        Self { columns: self.columns, placing: self.placing, ..Self::new(Arc::clone(&self.content)) }
    }

    pub fn refused(&self) -> Option<&str> {
        match self.kind {
            Kind::Countries => self.refused.as_deref(),
            Kind::Whole => self.modes.as_ref().and_then(Whole::refusal),
            Kind::Nothing => None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn shown(&self) -> &Arc<Placements> {
        &self.drawn
    }

    fn leave(&mut self) {
        (self.areas, self.unsent, self.unsent_held, self.applied_at) = Default::default();
        self.warned = None;
        (self.drawn, self.landed_drawn, self.made_token, self.drawn_stale, self.sent, self.worker_held) = (nothing(), None, None, false, None, 0);
        self.epoch += 1;
        self.fed.compose(None);
        (self.whole_shown, self.whole_pending) = (None, None);
    }

    fn start(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        let columns = self.columns.expect("the names layer is declared before it starts");
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = landed::channel();
            let (content, due) = (Arc::clone(&self.content), self.fed.due());
            thread::Builder::new().name("names fold".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                run(content, receive, send, due)
            })?;
            (self.jobs, self.made) = (Some(jobs), Some(made));
        }
        self.epoch += 1;
        self.fed.compose(mirrors.get(TableId::Provinces).and_then(Mirror::head));
        self.applied_at.clear();
        let tables = Box::new(Tables::from_mirrors(mirrors, &columns.shapes()));
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Reset { epoch: self.epoch, tables, columns, stamp: None, placing: self.placing });
        }
        let through = self.fed.through();
        let again: Vec<Arc<Step>> = self.kept.iter().filter(|step| step.epoch() == self.last_epoch && through.is_none_or(|through| step.last() > through)).cloned().collect();
        if let Some(jobs) = &self.jobs {
            for step in self.fed.hand(&again) {
                let _ = jobs.send(Job::Step { epoch: self.epoch, step: Arc::clone(step) });
            }
        }
        Ok(())
    }

    fn take_whole(&mut self) {
        let Some(built) = self.modes.as_ref().and_then(Whole::product) else { return };
        if self.whole_shown.as_ref().is_some_and(|shown| Arc::ptr_eq(&**shown, &**built)) {
            return;
        }
        self.whole_pending = Some(built.as_ref().map(|built| match &built.product {
            Ok(placements) => Arc::clone(placements),
            Err(why) => Arc::new(Placements { warnings: vec![format!("names: {why}")], generation: next_generation(), ..Placements::default() }),
        }));
        self.whole_shown = Some(built.clone());
    }

    fn push_any(&mut self, target: &mut dyn Target) -> io::Result<()> {
        match self.kind {
            Kind::Countries => self.push(target),
            Kind::Whole => {
                if let Some(placements) = self.whole_pending.take() {
                    viewer_runtime::view_record::add("names layer", "installs", 1);
                    target.install(&placements)?;
                    self.drawn = Arc::clone(&*placements);
                    self.landed_drawn = Some(placements);
                } else if !target.holds(&self.drawn) {
                    viewer_runtime::view_record::add("names layer", "installs", 1);
                    target.install(&self.drawn)?;
                }
                Ok(())
            }
            Kind::Nothing => Ok(()),
        }
    }

    pub fn areas(&self) -> &BTreeMap<u32, Area> {
        &self.areas
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    pub fn latency(&self) -> Latency {
        self.latency
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn made_through(&self) -> Option<Hour> {
        self.fed.made_through()
    }

    pub fn placements(&self) -> Placements {
        let country = self.areas.keys().flat_map(|&slot| self.names_of(slot)).cloned().collect();
        Placements { country, warnings: self.warnings(), ..Placements::default() }
    }

    fn names_of(&self, slot: u32) -> Vec<&WorldName> {
        self.areas.get(&slot).filter(|area| area.tag.is_some()).map_or_else(Vec::new, |area| area.placed.iter().filter_map(|(_, name)| name.as_deref()).collect())
    }

    fn warnings_held(&mut self) -> Vec<String> {
        match &self.warned {
            Some(warnings) => warnings.clone(),
            None => self.warned.insert(self.warnings()).clone(),
        }
    }

    fn warnings(&self) -> Vec<String> {
        let (mut unnamed, mut unplaced) = (Vec::new(), Vec::new());
        for area in self.areas.values() {
            let Some(tag) = &area.tag else { continue };
            if let AreaText::Tag(_, why) = name_pass::area_text(tag, &area.text, false) {
                unnamed.push(why);
            }
            for (component, name) in &area.placed {
                if name.is_none() {
                    unplaced.push(name_pass::unplaced_line(tag, component));
                }
            }
        }
        name_pass::placement_warnings(&unnamed, &[], unplaced)
    }

    fn owe(&mut self, slot: u32) {
        (self.drawn_stale, self.warned) = (true, None);
        if self.unsent_held.insert(slot) {
            self.unsent.push_back(slot);
        }
    }

    fn push(&mut self, target: &mut dyn Target) -> io::Result<()> {
        if self.sent.is_some() && target.keyed() != self.sent {
            self.sent = None;
        }
        if self.uploaded() {
            return Ok(());
        }
        let generation = next_generation();
        if self.sent.is_none() {
            target.begin(generation);
            (self.sent, self.drawn_stale) = (Some(generation), true);
            self.unsent.clear();
            self.unsent_held.clear();
            let slots: Vec<u32> = self.areas.keys().copied().collect();
            slots.into_iter().for_each(|slot| self.owe(slot));
        }
        while let Some(&slot) = self.unsent.front() {
            target.set(generation, slot, &self.names_of(slot))?;
            self.sent = Some(generation);
            self.unsent.pop_front();
            self.unsent_held.remove(&slot);
        }
        if self.sent != Some(self.drawn.generation) || (self.drawn_stale && self.unsent.is_empty()) {
            let warnings = if self.unsent.is_empty() { self.warnings_held() } else { self.drawn.warnings.clone() };
            let drawn = Arc::new(Placements { country: Vec::new(), warnings, generation: self.sent.unwrap_or_default(), near: None });
            self.drawn = Arc::clone(&drawn);
            self.landed_drawn = self.made_token.as_ref().map(|token| token.as_ref().map(|_| drawn));
            self.drawn_stale = !self.unsent.is_empty();
        }
        Ok(())
    }

    fn exchange(&mut self, wait: bool) -> io::Result<()> {
        if let Some(receive) = &self.made {
            loop {
                let made = if wait && !self.fed.arrived() { self.retiring.receive(receive)? } else { receive.try_recv().ok() };
                let Some(made) = made else { break };
                if made.epoch == self.epoch {
                    self.fed.receive(made.stamp, made);
                }
            }
        }
        let mut due = Vec::new();
        self.fed.take(|made| due.push(made));
        for made in due {
            self.take(made);
        }
        Ok(())
    }

    fn take(&mut self, made: Landed<Made>) {
        self.made_token = Some(made.as_ref().map(|_| ()));
        let made = made.into_inner();
        if made.whole {
            self.areas = made.areas.into_iter().filter_map(|(slot, area)| Some((slot, area?))).collect();
            (self.drawn_stale, self.sent, self.warned) = (true, None, None);
            self.unsent.clear();
            self.unsent_held.clear();
        } else {
            for (slot, area) in made.areas {
                match area {
                    Some(area) => self.areas.insert(slot, area),
                    None => self.areas.remove(&slot),
                };
                self.owe(slot);
            }
        }
        self.refused = made.refused;
        for (held, more) in self.marked.iter_mut().zip(made.marked) {
            *held += more;
        }
        self.worker_held = made.held;
        let mut since = None;
        while self.applied_at.front().is_some_and(|&(hour, _)| made.stamp.is_some_and(|stamp| hour <= stamp)) {
            since = since.or(self.applied_at.pop_front().map(|(_, frame)| frame));
        }
        let frames = since.map_or(1, |since| self.frame.saturating_sub(since) + 1);
        let latency = &mut self.latency;
        latency.in_flight_peak = latency.in_flight_peak.max(made.placed.len() as u64);
        for ns in made.placed {
            let bucket = match frames {
                1 => 0,
                2 => 1,
                3..=4 => 2,
                5..=8 => 3,
                9..=16 => 4,
                17..=32 => 5,
                _ => 6,
            };
            latency.buckets[bucket] += 1;
            latency.worst_frames = latency.worst_frames.max(frames);
            let us = ns / 1000;
            latency.worker_areas += 1;
            latency.worker_us += us;
            latency.worker_worst_us = latency.worker_worst_us.max(us);
            latency.worker_buckets[match us {
                0..1_000 => 0,
                1_000..4_000 => 1,
                4_000..16_000 => 2,
                16_000..33_000 => 3,
                _ => 4,
            }] += 1;
        }
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        let mut differing = 0;
        let mut first = None;
        let mut note = |what: String| {
            differing += 1;
            first.get_or_insert(what);
        };
        if self.refused != other.refused {
            note(format!("refused: {:?} against {:?}", self.refused, other.refused));
        }
        for slot in self.areas.keys().chain(other.areas.keys()).collect::<BTreeSet<_>>() {
            let (a, b) = (self.areas.get(slot), other.areas.get(slot));
            let same = match (a, b) {
                (Some(a), Some(b)) => (&a.members, &a.tag, &a.text, &a.placed) == (&b.members, &b.tag, &b.text, &b.placed),
                (None, None) => true,
                _ => false,
            };
            if !same {
                note(format!("area of slot {slot}: {:?} against {:?}", a.map(|area| (area.members.len(), &area.tag, &area.text, area.placed.len())), b.map(|area| (area.members.len(), &area.tag, &area.text, area.placed.len()))));
            }
        }
        (differing, first)
    }
}

impl Layer for Names {
    fn retirement(&self) -> Option<Retirement> {
        let stop = self.retiring.handle();
        let modes = self.modes.as_ref().and_then(Layer::retirement);
        Some(Box::new(move || {
            stop();
            if let Some(modes) = modes {
                modes();
            }
        }))
    }

    fn name(&self) -> &'static str {
        "names"
    }

    fn placed(&mut self, view: &viewer_layer_core::layers::View) -> Option<u64> {
        let cutoff = self.content.names_cutoff();
        let drawn = view.camera.is_none() || view.camera_height >= cutoff || view.target.is_some_and(|target| target.height >= cutoff);
        if drawn != self.placing {
            self.placing = drawn;
            if let Some(jobs) = &self.jobs {
                let _ = jobs.send(Job::Placing(drawn));
            }
        }
        Some(0)
    }

    fn views(&mut self, store: &viewer_layer_core::playhead_host::views::Store) -> io::Result<()> {
        match &mut self.modes {
            Some(modes) => modes.views(store),
            None => Ok(()),
        }
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let refuse = |why: String| io::Error::new(io::ErrorKind::Unsupported, format!("The names layer is refused: {why}"));
        let kind = kind(&*self.content, &view.map_mode);
        if let Some(modes) = &mut self.modes {
            modes.inputs(view, inputs)?;
        }
        let kind = match kind {
            Ok(kind) => kind,
            Err(why) => {
                self.leave();
                self.kind = Kind::Nothing;
                return Err(refuse(why));
            }
        };
        if kind != self.kind || view.map_mode != self.mode {
            if kind != self.kind || kind == Kind::Whole {
                self.leave();
            }
            self.restart = kind == Kind::Countries && self.kind != Kind::Countries;
            (self.kind, self.mode) = (kind, view.map_mode.clone());
        }
        match kind {
            Kind::Nothing => return Ok(()),
            Kind::Whole if self.modes.is_none() => return Err(refuse(format!("map mode {} writes names over areas that are not the countries', which are made from rows only in the window", view.map_mode.token()))),
            Kind::Whole => return Ok(()),
            Kind::Countries => {}
        }
        for table in CORE.into_iter().chain(NAMING) {
            inputs.table(table, true)?;
        }
        self.columns = Some(Columns {
            controller: inputs.number(TableId::Provinces, "controller")?,
            link_slot: inputs.number(TableId::CountryLinks, "canonical_slot")?,
            country_slot: inputs.number(TableId::Countries, "canonical_slot")?,
            tag: inputs.text(TableId::Countries, "tag")?,
            cosmetic: inputs.text(TableId::CountryAppearance, "cosmetic_tag")?,
            name_tag: inputs.text(TableId::CountryAppearance, "name_tag")?,
            leader: inputs.text(TableId::CountryAppearance, "leader_ideology")?,
            overlord: inputs.number(TableId::CountryAppearance, "overlord")?,
            ruling_party: inputs.number(TableId::Politics, "ruling_party")?,
            token: inputs.text(TableId::PoliticalParties, "token")?,
            autonomy: inputs.text(TableId::Autonomy, "state")?,
        });
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        match self.kind {
            Kind::Countries => {
                self.restart = false;
                self.kept.clear();
                self.start(mirrors)?;
                let marked = self.marked;
                self.exchange(true)?;
                self.marked = marked;
                Ok(())
            }
            Kind::Whole => {
                self.kept.clear();
                self.modes.as_mut().map_or(Ok(()), |modes| modes.compose(mirrors))
            }
            Kind::Nothing => {
                self.kept.clear();
                Ok(())
            }
        }
    }

    fn viewed(&mut self, _view: &View, mirrors: &Mirrors) {
        if self.restart && self.kind == Kind::Countries && CORE.into_iter().chain(NAMING).all(|table| mirrors.get(table).is_some()) {
            self.restart = false;
            let _ = self.start(mirrors);
        }
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        self.last_epoch = step.epoch();
        while self.kept.front().is_some_and(|kept| kept.last() <= step.last()) {
            self.kept.pop_front();
        }
        match self.kind {
            Kind::Countries => {
                self.fed.applied(step);
                self.applied_at.push_back((step.last(), self.frame));
            }
            Kind::Whole => {
                if let Some(modes) = &mut self.modes {
                    modes.apply(step, mirrors);
                }
            }
            Kind::Nothing => {}
        }
    }

    fn wants_ahead(&self) -> bool {
        self.kind == Kind::Countries
    }

    fn void_ahead(&mut self, mirrors: &Mirrors, _kept: Option<Hour>) {
        let (Some(columns), Some(jobs)) = (self.columns, &self.jobs) else { return };
        if self.kind != Kind::Countries || !CORE.into_iter().chain(NAMING).all(|table| mirrors.get(table).is_some()) {
            return;
        }
        self.kept.clear();
        self.epoch += 1;
        self.fed.void();
        let tables = Box::new(Tables::from_mirrors(mirrors, &columns.shapes()));
        let _ = jobs.send(Job::Reset { epoch: self.epoch, tables, columns, stamp: self.fed.applied_hour(), placing: self.placing });
    }

    fn ahead(&mut self, steps: &[Arc<Step>]) {
        let Some(jobs) = &self.jobs else { return };
        let through = self.fed.through();
        for step in self.fed.hand(steps) {
            let _ = jobs.send(Job::Step { epoch: self.epoch, step: Arc::clone(step) });
            if through.is_none_or(|through| step.last() > through) {
                self.kept.push_back(Arc::clone(step));
            }
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.frame += 1;
        match self.kind {
            Kind::Countries => self.exchange(settling.waits())?,
            Kind::Whole => {
                if let Some(modes) = &mut self.modes {
                    modes.settle(mirrors, settling)?;
                }
                self.take_whole();
            }
            Kind::Nothing => {}
        }
        Ok(())
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        if self.kind == Kind::Countries {
            self.exchange(false)?;
        }
        let pushed = self.push_any(&mut OnScene(gpu));
        if viewer_runtime::view_record::on() {
            let kind = match self.kind {
                Kind::Nothing => 0,
                Kind::Countries => 1,
                Kind::Whole => 2,
            };
            viewer_runtime::view_record::set("names layer", "kind", kind);
            viewer_runtime::view_record::set("names layer", "drawn_generation", self.drawn.generation);
            viewer_runtime::view_record::set("names layer", "drawn_names", self.drawn.country.len() as u64);
            viewer_runtime::view_record::set("names layer", "pending", u64::from(self.whole_pending.is_some()));
        }
        pushed
    }

    fn uploaded(&self) -> bool {
        match self.kind {
            Kind::Countries => self.sent.is_some() && self.unsent.is_empty() && !self.drawn_stale,
            Kind::Whole => self.whole_pending.is_none(),
            Kind::Nothing => true,
        }
    }

    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> Published {
        match self.kind {
            Kind::Countries => self.fed.published(),
            Kind::Whole => self.modes.as_ref().map_or(Published::Nothing, Layer::published),
            Kind::Nothing => Published::Current,
        }
    }

    fn held(&self) -> usize {
        self.areas.values().map(area_bytes).sum::<usize>()
            + self.worker_held
            + self.unsent.capacity() * 4
            + self.unsent_held.capacity() * 8
            + self.applied_at.capacity() * 16
            + self.modes.as_ref().map_or(0, Layer::held)
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn drawn(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Option<Landed<Arc<Placements>>> {
    if !side.draws("names") {
        return None;
    }
    let layer = side.parts()?.1.iter().find_map(|layer| layer.any().downcast_ref::<Names>())?;
    layer.landed_drawn.clone()
}

pub fn summary(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> serde_json::Value {
    let Some(layer) = side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<Names>())) else { return serde_json::Value::Null };
    let latency = layer.latency;
    let [under_1, to_4, to_16, to_33, more] = latency.worker_buckets;
    let [one, two, four, eight, sixteen, thirty_two, beyond] = latency.buckets;
    serde_json::json!({
        "placement_threads": PLACERS,
        "areas_placed": latency.worker_areas,
        "placement_us_mean": (latency.worker_areas > 0).then(|| latency.worker_us as f64 / latency.worker_areas as f64),
        "placement_us_worst": latency.worker_worst_us,
        "placements_by_ms": {"under 1": under_1, "1-4": to_4, "4-16": to_16, "16-33": to_33, "33 or more": more},
        "areas_on_the_worker_peak": latency.in_flight_peak,
        "frames_change_to_layer": {"1": one, "2": two, "3-4": four, "5-8": eight, "9-16": sixteen, "17-32": thirty_two, "more": beyond},
        "frames_change_to_layer_worst": latency.worst_frames,
    })
}

#[cfg(test)]
mod tests;
