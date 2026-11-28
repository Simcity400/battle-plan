
use super::meshes::{self, Chosen};
use viewer_layer_core::layers::{RetirementFlag, Settling};
use viewer_layer_core::playhead_host::{timeline::{self, Mark}, views::{self, Need}};
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::{DecodedFrame, discovering};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use viewer_runtime::memo::TableKey;
use viewer_foundation::perspective::Choice;
use viewer_unit_models::{
    trade_routes::ConvoyLine,
    unit_models::{KeptChoices, PreparedModels, Told},
};

const WHOLE: Duration = Duration::from_secs(10);

const LOOK: Duration = Duration::from_millis(8);

const NAMED: &[TableId] = &[
    TableId::Clock,
    TableId::Provinces,
    TableId::States,
    TableId::VariantDefinitions,
    TableId::VariantUpgrades,
    TableId::AvailableEquipmentVariants,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::CountryLocalSubunits,
    TableId::Politics,
    TableId::PoliticalParties,
    TableId::Factions,
    TableId::FactionMembers,
    TableId::LexicalTokens,
    TableId::ScriptVariables,
    TableId::ScriptVariableNames,
    TableId::SimulationFeatures,
    TableId::Ships,
    TableId::ShipModels,
    TableId::TaskForces,
    TableId::TaskForceMapMotion,
    TableId::TaskForceCounterInputs,
    TableId::TaskForceMissions,
    TableId::NavalCombats,
    TableId::NavalCombatDetails,
    TableId::NavalCombatShips,
    TableId::Raids,
    TableId::RaidTargets,
    TableId::RaidCountries,
    TableId::RaidCountryInputs,
    TableId::RaidCooldowns,
    TableId::RaidEquipment,
    TableId::RaidAirSites,
    TableId::RaidPathDistances,
    TableId::RaidSpeedInputs,
    TableId::AceRangeInputs,
    TableId::AirBases,
    TableId::AirWings,
    TableId::WingEquipment,
    TableId::NavalBases,
    TableId::Nukes,
    TableId::ProvinceBuildings,
    TableId::StateFlags,
    TableId::Stockpiles,
    TableId::StockpileArchetypes,
    TableId::VariantModules,
    TableId::VariantStats,
    TableId::CountryCommandPower,
    TableId::HumanControllers,
    TableId::Players,
    TableId::ReservedCountryNaming,
    TableId::VolunteerHosts,
    TableId::TradeRoutes,
    TableId::ConvoyClients,
    TableId::ResourceOrigins,
    TableId::ResourceExchanges,
    TableId::RegionalConvoyEfficiency,
];

pub fn tables() -> Vec<TableId> {
    let mut tables: Vec<TableId> = NAMED.iter().chain(viewer_foundation::perspective::VISION_TABLES).copied().collect();
    tables.sort_unstable_by_key(|table| table.index());
    tables.dedup();
    tables
}

fn declared(learned: &[TableId]) -> Vec<TableId> {
    let mut tables = tables();
    tables.extend_from_slice(learned);
    tables.sort_unstable_by_key(|table| table.index());
    tables.dedup();
    tables
}

pub trait Select: Send + Sync {
    fn prepare(&self, frame: &DecodedFrame, perspective: &Choice, convoys: Option<&[ConvoyLine]>, naval: bool, kept: &mut Kept) -> io::Result<PreparedModels>;

    fn choose_meshes(&self, frame: &DecodedFrame, ours: u32, kept: (&KeptChoices, Option<&BTreeSet<u64>>), stacks: &HashMap<u64, meshes::Asked>, asked: &HashSet<u64>, told: &Told, parts: &mut dyn FnMut(&'static str, u64)) -> io::Result<Chosen>;
}


#[derive(Default)]
pub struct Kept {
    state: Option<Box<dyn std::any::Any + Send>>,
    parts: Vec<(&'static str, u64)>,
}

impl Kept {
    pub fn state_and_timings<T: Default + Send + 'static>(&mut self) -> (&mut T, &mut Vec<(&'static str, u64)>) {
        let state = self.state.get_or_insert_with(|| Box::<T>::default());
        if !state.is::<T>() {
            *state = Box::<T>::default();
        }
        (state.downcast_mut().expect("the worker's preparation state type"), &mut self.parts)
    }
}

#[derive(Clone)]
struct Asked {
    perspective: Choice,
    convoys: Option<(u64, Arc<Vec<ConvoyLine>>)>,
    naval: bool,
}

type Key = (Choice, Option<u64>, bool);

impl Asked {
    fn key(&self) -> Key {
        (self.perspective.clone(), self.convoys.as_ref().map(|&(seq, _)| seq), self.naval)
    }
}

#[derive(Clone)]
struct Made {
    generation: u64,
    stamp: u64,
    hour: Hour,
    key: Key,
    stands: Hour,
    frame: Option<Arc<DecodedFrame>>,
    prepared: Arc<PreparedModels>,
    refusal: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub jobs: u64,
    pub busy_ns: u64,
    pub parts: Vec<(&'static str, u64)>,
}

impl Counts {
    fn add(&mut self, busy_ns: u64, parts: Vec<(&'static str, u64)>) {
        (self.jobs, self.busy_ns) = (self.jobs + 1, self.busy_ns + busy_ns);
        for (name, ns) in parts {
            match self.parts.iter_mut().find(|(held, _)| *held == name) {
                Some((_, sum)) => *sum += ns,
                None => self.parts.push((name, ns)),
            }
        }
    }
}

#[derive(Default)]
struct Held {
    asked: Option<Asked>,
    generation: u64,
    made: viewer_runtime::landed::PublishedKeyed<(u64, u64, Hour), Made>,
    published: u64,
    answered: viewer_runtime::landed::Published<Option<Hour>>,
    working: bool,
    tables: Vec<TableId>,
    counts: Counts,
    stop: bool,
}

struct Shared {
    held: Mutex<Held>,
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn answered(&self, stands: Hour, reader: &views::Reader) {
        let mut held = self.lock();
        let advanced = held.answered.advance(stands);
        held.working = false;
        drop(held);
        self.done.notify_all();
        if advanced {
            reader.notify_progress();
        }
    }

    fn stood(&self, generation: u64, build_seq: u64, stands: Hour, frame: &Arc<DecodedFrame>, reader: &views::Reader) {
        let mut held = self.lock();
        if held.generation == generation
            && let Some(mut made) = held.made.as_ref().filter(|made| made.generation == generation && stands > made.stands).cloned()
        {
            let stamp = held.published + 1;
            held.published = stamp;
            (made.stands, made.frame, made.stamp) = (stands, Some(Arc::clone(frame)), stamp);
            held.made.publish((generation, build_seq, made.stands), made);
            reader.notify_progress();
        }
        drop(held);
        self.done.notify_all();
    }
}

fn work(shared: &Shared, select: &dyn Select, reader: &views::Reader, maker: timeline::Maker) {
    let mut kept = Kept::default();
    let mut tables = shared.lock().tables.clone();
    let (mut built, mut standing): (Option<(TableKey, Key)>, Option<(TableKey, Key)>) = (None, None);
    let mut handled: Option<(u64, Key)> = None;
    let mut generation = 0;
    let mut build_seq = 0u64;
    loop {
        let (asked, want) = {
            let held = shared.lock();
            if held.stop {
                return;
            }
            (held.asked.clone(), held.generation)
        };
        if want != generation {
            (generation, built, standing, handled) = (want, None, None, None);
        }
        let Some(asked) = asked else {
            shared.lock().working = false;
            reader.wait_newer(u64::MAX, LOOK);
            continue;
        };
        let key = asked.key();
        let seen = handled.as_ref().map_or(0, |&(published, _)| published);
        let Some((newest, published)) = reader.newest() else {
            reader.wait_newer(seen, LOOK);
            continue;
        };
        let Some((through, seek)) = reader.through_seek(Arc::as_ptr(&newest)) else {
            continue;
        };
        if seek != reader.seek() {
            reader.wait_newer(published, LOOK);
            continue;
        }
        let stands = through.max(newest.hour);
        if handled.as_ref() == Some(&(published, key.clone())) {
            shared.answered(stands, reader);
            reader.wait_newer(published, LOOK);
            continue;
        }
        handled = Some((published, key.clone()));
        timeline::mark(maker, Mark::Rows);
        let at = (TableKey::of(&*newest, &tables), key);
        if built.as_ref() == Some(&at) {
            if standing.as_ref() == Some(&at) {
                shared.stood(generation, build_seq, stands, &newest, reader);
            }
            shared.answered(stands, reader);
            continue;
        }
        let frame = newest;
        let Asked { perspective, convoys, naval } = asked;
        shared.lock().working = true;
        timeline::mark(maker, Mark::BuildBegan);
        let started = Instant::now();
        let (prepared, read) = discovering(|| select.prepare(&frame, &perspective, convoys.as_ref().map(|(_, lines)| &lines[..]), naval, &mut kept));
        timeline::mark(maker, Mark::BuildEnded);
        let busy_ns = started.elapsed().as_nanos() as u64;
        let parts = std::mem::take(&mut kept.parts);
        built = Some(at.clone());
        shared.lock().counts.add(busy_ns, parts);
        let missed: Vec<TableId> = read.into_iter().filter(|table| !tables.contains(table)).collect();
        if !missed.is_empty() {
            tables.extend(missed);
            tables.sort_unstable_by_key(|table| table.index());
            tables.dedup();
            reader.declare_only(&tables);
            let mut held = shared.lock();
            (held.tables, held.working) = (tables.clone(), true);
            shared.done.notify_all();
            drop(held);
            (built, standing, handled) = (None, None, None);
            continue;
        }
        let mut held = shared.lock();
        if held.stop {
            return;
        }
        if held.generation != generation {
            (built, standing, handled) = (None, None, None);
            continue;
        }
        let (prepared, refusal) = match prepared {
            Ok(prepared) => (prepared, None),
            Err(error) => {
                let why = format!("Today's unit models cannot be made from the rows at the head: {error}");
                (PreparedModels::withheld(&why), Some(why))
            }
        };
        let stamp = held.published + 1;
        held.published = stamp;
        build_seq += 1;
        held.made.publish(
            (generation, build_seq, stands),
            Made { generation, stamp, hour: frame.hour, key: at.1.clone(), stands, frame: Some(frame), prepared: Arc::new(prepared), refusal },
        );
        standing = Some(at);
        held.answered.advance(stands);
        held.working = false;
        drop(held);
        shared.done.notify_all();
        reader.notify_progress();
    }
}

pub struct Models {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    reader: Arc<views::Reader>,
    maker: timeline::Maker,
    lines: Option<(u64, Arc<Vec<ConvoyLine>>)>,
    made: Option<viewer_runtime::landed::Landed<Made>>,
    generation: u64,
    shipless: Option<Arc<PreparedModels>>,
    joined: Option<u64>,
    pub prepared: Option<(Hour, viewer_runtime::landed::Landed<Arc<PreparedModels>>)>,
    pub refused: Option<String>,
    asked: Option<Key>,
    prepared_for: Option<Choice>,
}

impl Models {
    pub fn new(select: Arc<dyn Select>, views: &views::Views, learned: &[TableId]) -> Self {
        let tables = declared(learned);
        let reader = Arc::new(views.reader("unit_models", &tables.iter().copied().map(Need::every).collect::<Vec<Need>>()));
        let maker = timeline::maker("unit_models");
        let shared = Arc::new(Shared { held: Mutex::new(Held { tables, ..Held::default() }), done: Condvar::new() });
        let worker = std::thread::Builder::new().name("unit models".into()).spawn({
            let (shared, reader) = (Arc::clone(&shared), Arc::clone(&reader));
            move || {
                viewer_runtime::worker_priority::enter(viewer_runtime::worker_priority::Class::Kept);
                work(&shared, &*select, &reader, maker);
            }
        });
        let (worker, refused) = match worker {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(format!("The unit models' worker cannot start: {error}"))),
        };
        Self { shared, worker, reader, maker, lines: None, made: None, generation: 0, shipless: None, joined: None, prepared: None, refused, asked: None, prepared_for: None }
    }

    pub fn maker(&self) -> timeline::Maker {
        self.maker
    }

    pub fn tables(&self) -> Vec<TableId> {
        self.shared.lock().tables.clone()
    }

    pub fn composed(&mut self) {
        self.generation += 1;
        let mut held = self.shared.lock();
        held.generation = self.generation;
        held.answered.clear();
    }

    pub fn join_ships(&mut self, ships: Option<&crate::ships::Ships>) -> bool {
        let Some((hour, prepared)) = self.prepared.clone() else { return false };
        let (Some(shipless), Some(ships)) = (&self.shipless, ships) else { return false };
        if self.joined == Some(ships.seq()) {
            return false;
        }
        let (sources, report) = ships.handover();
        let joined = shipless.with_ships(sources, &report);
        joined.inherit_convoys(&prepared);
        (self.prepared, self.joined) = (Some((hour, prepared.map(|_| Arc::new(joined)))), Some(ships.seq()));
        true
    }

    pub fn ask(&mut self, perspective: &Choice, convoys: Option<(u64, &[ConvoyLine])>, naval: bool) {
        self.lines = match (convoys, self.lines.take()) {
            (None, _) => None,
            (Some((seq, _)), Some((held, lines))) if held == seq => Some((seq, lines)),
            (Some((seq, lines)), _) => Some((seq, Arc::new(lines.to_vec()))),
        };
        let asked = Asked { perspective: perspective.clone(), convoys: self.lines.clone(), naval };
        self.asked = Some(asked.key());
        self.shared.lock().asked = Some(asked);
        timeline::mark(self.maker, Mark::Asked);
    }

    pub fn join(&mut self, hour: Option<Hour>, settling: Settling, retiring: &RetirementFlag) -> bool {
        if self.worker.is_none() {
            return false;
        }
        let Some(hour) = hour else { return self.take() };
        let until = Instant::now() + settling.waiting(WHOLE);
        let mut arrived = false;
        loop {
            arrived |= self.take();
            if self.made_at(hour) {
                return arrived;
            }
            let held = self.shared.lock();
            let now = Instant::now();
            if retiring.requested() || now >= until || !self.reader.has_rows_at(hour) {
                return arrived;
            }
            if held.made.as_ref().is_some_and(|made| {
                made.generation == self.generation && self.asked.as_ref() == Some(&made.key)
                    && self.made.as_ref().is_none_or(|ours| ours.stamp < made.stamp)
            }) {
                continue;
            }
            let _ = self.shared.done.wait_timeout(held, (until - now).min(RetirementFlag::POLL));
        }
    }

    fn take(&mut self) -> bool {
        let perspective = self.asked.as_ref().map(|key| &key.0);
        let mut arrived = false;
        {
            let held = self.shared.lock();
            if let Some(made) = held.made.as_ref().filter(|made| made.generation == self.generation && perspective == Some(&made.key.0))
                && self.made.as_ref().is_none_or(|ours| ours.stamp < made.stamp)
            {
                arrived = self.made.as_ref().is_none_or(|ours| !Arc::ptr_eq(&ours.prepared, &made.prepared));
                self.refused.clone_from(&made.refusal);
                self.made = held.made.landed_clone();
            }
        }
        if let Some(made) = self.made.take() {
            let made = if let Some(frame) = &made.frame {
                let through = self.reader.through(Arc::as_ptr(frame));
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
        if arrived && let Some(made) = &self.made {
            let prepared = made.as_ref().map(|made| Arc::clone(&made.prepared));
            (self.shipless, self.joined) = ((self.refused.is_none() && !prepared.naval()).then(|| Arc::clone(&*prepared)), None);
            (self.prepared, self.prepared_for) = (Some((made.hour, prepared)), Some(made.key.0.clone()));
        }
        if let Some((hour, prepared)) = self.prepared.clone().filter(|_| self.prepared_for.is_some() && self.prepared_for.as_ref() != perspective) {
            let withheld = PreparedModels::withheld("the models of the viewing country chosen are being made");
            (self.prepared, self.prepared_for, self.shipless, self.joined) = (Some((hour, prepared.map(|_| Arc::new(withheld)))), None, None, None);
            arrived = true;
        }
        arrived
    }

    pub fn take_counts(&self) -> Counts {
        std::mem::take(&mut self.shared.lock().counts)
    }

    pub fn stands(&self) -> Option<Hour> {
        self.made.as_ref().filter(|made| made.generation == self.generation).map(|made| made.stands)
    }

    pub(super) fn made_at(&self, hour: Hour) -> bool {
        (self.worker.is_none() && self.refused.is_some()) || self.made.as_ref().is_some_and(|made| {
            made.generation == self.generation && self.asked.as_ref() == Some(&made.key)
                && self.reader.has_rows_at(hour)
                && (made.stands == hour || (made.refusal.is_none() && made.stands > hour))
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn in_flight(&self) -> usize {
        usize::from(self.shared.lock().working)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn answered(&self) -> Option<Hour> {
        self.shared.lock().answered.as_ref().copied()
    }

    pub fn waiting(&self) -> bool {
        self.shared.lock().working
    }
}

impl Drop for Models {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.done.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
