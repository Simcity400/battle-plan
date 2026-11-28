
use viewer_layer_core::layers::{Carried, Settling, Inputs, Layer, List, Num, Published, Retirement, RetirementFlag, Text, Upstream, View};
use viewer_layer_core::playhead_host::mirror::{Cell, Mirror, Mirrors, UNREAD_REASON};
use playhead::{Step, changes::Changes};
use replay_columnar::manifest::Status;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    time::Instant,
};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_gpu::{
    province_map::HeightField,
    ribbon_content::{ArrowType, OrderDraw, OrderField, OrderVertex},
};
use viewer_map_modes::map_mode::visibility;
use viewer_unit_models::trade_routes::{
    self,
    ClientFields,
    Countries,
    Emit,
    ExchangeOrder,
    LinksGap,
    OriginFields,
    RegionalFields,
    RouteFields,
    RouteKey,
    Shaped,
    Stops,
    Tables,
    TradeRoutes,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Origin {
    country: u32,
    state: u32,
    list: u8,
    ordinal: u32,
    bad: bool,
    convoys_assigned: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    handle: u32,
    category: u8,
    owner: u32,
    other: u32,
    route_class: u8,
    route_country: u32,
    route_other: u32,
    bad: bool,
    convoys_assigned: i32,
    owner_ordinal: Option<u32>,
    other_ordinal: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    country: u32,
    class: u8,
    other: u32,
    kind: u8,
    from: u32,
    to: u32,
    path: Vec<u32>,
    source_state: Option<u32>,
    destination_state: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Regional {
    country: u32,
    region: u32,
    efficiency: i64,
}

macro_rules! fields {
    ($trait:ident for $type:ty { $($name:ident: $ret:ty),+ $(,)? }) => {
        impl $trait for $type {
            $(fn $name(&self) -> $ret {
                self.$name
            })+
        }
    };
}

fields!(OriginFields for Origin { country: u32, state: u32, list: u8, ordinal: u32, bad: bool, convoys_assigned: i32 });
fields!(ClientFields for Client {
    handle: u32, category: u8, owner: u32, other: u32, route_class: u8, route_country: u32, route_other: u32,
    bad: bool, convoys_assigned: i32, owner_ordinal: Option<u32>, other_ordinal: Option<u32>,
});
fields!(RegionalFields for Regional { country: u32, region: u32, efficiency: i64 });

impl RouteFields for Route {
    fn country(&self) -> u32 {
        self.country
    }
    fn class(&self) -> u8 {
        self.class
    }
    fn other(&self) -> u32 {
        self.other
    }
    fn kind(&self) -> u8 {
        self.kind
    }
    fn from(&self) -> u32 {
        self.from
    }
    fn to(&self) -> u32 {
        self.to
    }
    fn path(&self) -> &[u32] {
        &self.path
    }
    fn source_state(&self) -> Option<u32> {
        self.source_state
    }
    fn destination_state(&self) -> Option<u32> {
        self.destination_state
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Provinces {
    pub ports: BTreeMap<u32, u32>,
    pub state_of: BTreeMap<u32, u32>,
}

pub type Read<'a> = Result<Tables<'a, Origin, Client, Route, Regional>, String>;

pub trait Routes: Send + Sync {
    fn stops(&self, route: &Route, reverse: bool, provinces: &Provinces) -> Result<Option<Stops>, String>;

    #[expect(clippy::too_many_arguments, reason = "the builder's tables, countries, exchanges, culture, provinces and stops are each the layer's")]
    fn build(
        &self,
        ours: u32,
        tables: Read<'_>,
        countries: &Countries<'_>,
        exchanges: &BTreeMap<u32, ExchangeOrder>,
        culture: &dyn Fn(&Countries<'_>) -> Result<String, String>,
        provinces: Result<&Provinces, String>,
        stops: &mut dyn FnMut(&Route, bool) -> Result<Option<Stops>, String>,
        emit: &mut Emit<'_>,
    ) -> TradeRoutes;
}


type RibbonKey = (String, [u32; 4], [u32; 4], Vec<[i64; 2]>, Vec<u8>);

type Owned = (Vec<Origin>, Vec<Client>, Vec<Route>, Vec<Regional>);

struct Job {
    hour: Option<Hour>,
    ours: u32,
    tables: Result<Owned, String>,
    slots: Arc<BTreeMap<u32, u32>>,
    gap: LinksGap,
    exchanges: BTreeMap<u32, ExchangeOrder>,
    culture: Result<String, String>,
    provinces: Arc<Provinces>,
    provinces_read: Result<(), String>,
    provinces_version: u64,
}

struct Made {
    product: TradeRoutes,
    ours: u32,
    hour: Option<Hour>,
    jobs: usize,
    reused: usize,
    made: usize,
    micros: usize,
    bytes: usize,
}

struct Maker {
    routes: Arc<dyn Routes>,
    stops: HashMap<(RouteKey, bool), (Route, Result<Option<Stops>, String>)>,
    ribbons: HashMap<RibbonKey, Option<Arc<Ribbon>>>,
    provinces_version: u64,
}

impl Maker {
    fn spawn(
        routes: Arc<dyn Routes>,
        #[cfg(test)] sent: Arc<(Mutex<(usize, bool)>, std::sync::Condvar)>,
    ) -> io::Result<(Sender<Job>, viewer_runtime::landed::Receiver<Made>)> {
        let (jobs, receive) = mpsc::channel::<Job>();
        let (send, made) = viewer_runtime::landed::channel::<Made>();
        let mut maker = Self { routes, stops: HashMap::new(), ribbons: HashMap::new(), provinces_version: 0 };
        std::thread::Builder::new().name("convoys".into()).spawn(move || {
            #[cfg(test)]
            struct WorkerExit(Arc<(Mutex<(usize, bool)>, std::sync::Condvar)>);
            #[cfg(test)]
            impl Drop for WorkerExit {
                fn drop(&mut self) {
                    let (state, changed) = &*self.0;
                    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 = true;
                    changed.notify_all();
                }
            }
            #[cfg(test)]
            let _exit = WorkerExit(Arc::clone(&sent));
            viewer_runtime::worker_priority::background_priority();
            while let Ok(mut job) = receive.recv() {
                let mut answered = 1;
                while let Ok(newer) = receive.try_recv() {
                    (job, answered) = (newer, answered + 1);
                }
                if send.send(maker.make(job, answered)).is_err() {
                    return;
                }
                #[cfg(test)]
                {
                    let (state, changed) = &*sent;
                    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0 += answered;
                    changed.notify_all();
                }
            }
        })?;
        Ok((jobs, made))
    }

    fn make(&mut self, job: Job, answered: usize) -> Made {
        let started = Instant::now();
        let hour = job.hour;
        if job.provinces_version != self.provinces_version {
            self.stops.clear();
            self.provinces_version = job.provinces_version;
        }
        let countries = Countries::over(&job.slots, job.gap.clone(), job.ours);
        let tables: Read<'_> = match &job.tables {
            Ok((origins, clients, routes, regional)) => Ok(Tables { origins, clients, routes, regional }),
            Err(why) => Err(why.clone()),
        };
        let culture = |_: &Countries<'_>| job.culture.clone();
        let provinces = job.provinces_read.clone().map(|()| &*job.provinces);
        let (routes, cache) = (&self.routes, &mut self.stops);
        let mut stops = |route: &Route, reverse: bool| {
            let key = (route.country, route.class, route.other);
            match cache.get(&(key, reverse)) {
                Some((held, stops)) if held == route => stops.clone(),
                _ => {
                    let stops = routes.stops(route, reverse, &job.provinces);
                    cache.insert((key, reverse), (route.clone(), stops.clone()));
                    stops
                }
            }
        };
        let (held, mut kept) = (std::mem::take(&mut self.ribbons), HashMap::new());
        let (mut reused, mut made) = (0, 0);
        let mut emit = |field: &mut OrderField, arrow: &ArrowType, shaped: &Shaped, color: [f32; 4], hotspot: [f32; 4], heights: &HeightField<'_>| {
            let key: RibbonKey = (arrow.name.clone(), color.map(f32::to_bits), hotspot.map(f32::to_bits), shaped.controls.clone(), shaped.variants.clone());
            let ribbon = match held.get(&key).or_else(|| kept.get(&key)) {
                Some(ribbon) => {
                    reused += 1;
                    ribbon.clone()
                }
                None => {
                    made += 1;
                    let mut drawn = OrderField::default();
                    let points = trade_routes::emit(&mut drawn, arrow, shaped, color, hotspot, heights);
                    points.map(|points| Arc::new(Ribbon { vertices: drawn.vertices, native: drawn.native_vertices, draw: drawn.draws.pop().expect("a drawn ribbon"), points }))
                }
            };
            kept.insert(key, ribbon.clone());
            let ribbon = ribbon?;
            let base = field.native_vertices.len() as u32;
            field.vertices.extend_from_slice(&ribbon.vertices);
            field.native_vertices.extend_from_slice(&ribbon.native);
            let mut draw = ribbon.draw.clone();
            for strip in &mut draw.native_strips {
                (strip.start, strip.end) = (strip.start + base, strip.end + base);
            }
            field.draws.push(draw);
            Some(ribbon.points.clone())
        };
        let product = routes.build(job.ours, tables, &countries, &job.exchanges, &culture, provinces, &mut stops, &mut emit);
        self.ribbons = kept;
        let ribbons: usize = self.ribbons.values().flatten().map(|ribbon| ribbon.vertices.capacity() * std::mem::size_of::<OrderVertex>() + ribbon.native.capacity() * 32 + ribbon.points.capacity() * 8 + 256).sum();
        let stops: usize = self.stops.values().map(|(route, stops)| route.path.capacity() * 4 + stops.as_ref().ok().and_then(Option::as_ref).map_or(0, |stops| (stops.provinces.capacity() + stops.regions.capacity()) * 4) + 96).sum();
        Made { product, ours: job.ours, hour, jobs: answered, reused, made, micros: started.elapsed().as_micros() as usize, bytes: ribbons + stops }
    }
}

fn links_gap(links: Option<&Mirror>) -> LinksGap {
    match links.and_then(Mirror::status) {
        Some(Status::Complete) => LinksGap::Complete,
        Some(Status::Truncated { missing }) => LinksGap::Truncated { missing },
        Some(Status::Unavailable { .. }) => LinksGap::Unavailable { reason: links.and_then(Mirror::reason).unwrap_or(UNREAD_REASON).to_owned() },
        Some(Status::NotRecorded) | None => LinksGap::NotRecorded { mode: "this".into() },
    }
}

const DEFERRED: [TableId; 5] = [TableId::ResourceOrigins, TableId::ConvoyClients, TableId::TradeRoutes, TableId::RegionalConvoyEfficiency, TableId::ResourceExchanges];

fn deferred_at(table: TableId) -> Option<usize> {
    DEFERRED.iter().position(|&deferred| deferred == table)
}

#[derive(Clone, Debug)]
struct Ribbon {
    vertices: Vec<OrderVertex>,
    native: Vec<viewer_gpu::ribbon_content::Vertex>,
    draw: OrderDraw,
    points: Vec<[f32; 2]>,
}

#[derive(Debug)]
struct Rows<T> {
    keys: Vec<u128>,
    values: Vec<T>,
}

impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self { keys: Vec::new(), values: Vec::new() }
    }
}

impl<T: PartialEq> Rows<T> {
    fn put(&mut self, key: u128, value: T) -> bool {
        if self.keys.last().is_none_or(|&last| last < key) {
            self.keys.push(key);
            self.values.push(value);
            return true;
        }
        match self.keys.binary_search(&key) {
            Ok(at) if self.values[at] == value => false,
            Ok(at) => {
                self.values[at] = value;
                true
            }
            Err(at) => {
                self.keys.insert(at, key);
                self.values.insert(at, value);
                true
            }
        }
    }

    fn remove(&mut self, key: u128) -> bool {
        match self.keys.binary_search(&key) {
            Ok(at) => {
                self.keys.remove(at);
                self.values.remove(at);
                true
            }
            Err(_) => false,
        }
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.values.clear();
    }

    fn get(&self, key: u128) -> Option<&T> {
        self.keys.binary_search(&key).ok().map(|at| &self.values[at])
    }

    fn bytes(&self) -> usize {
        self.keys.capacity() * 16 + self.values.capacity() * std::mem::size_of::<T>()
    }
}

#[derive(Clone, Copy)]
struct Columns {
    origin_list: Num<u8>,
    origin_bad: Num<bool>,
    origin_convoys: Num<i32>,
    origin_ordinal: Num<u32>,
    client_category: Num<u8>,
    client_owner: Num<u32>,
    client_other: Num<u32>,
    client_route_class: Num<u8>,
    client_route_country: Num<u32>,
    client_route_other: Num<u32>,
    client_bad: Num<bool>,
    client_convoys: Num<i32>,
    client_owner_ordinal: Num<u32>,
    client_other_ordinal: Num<u32>,
    route_kind: Num<u8>,
    route_from: Num<u32>,
    route_to: Num<u32>,
    route_path: List,
    route_source: Num<u32>,
    route_destination: Num<u32>,
    efficiency: Num<i64>,
    exchange_resource: Num<u32>,
    exchange_import: Num<u32>,
    exchange_export: Num<u32>,
    link_slot: Num<u32>,
    culture: Text,
    province_state: Num<u32>,
    province_port: Num<i32>,
}

const TABLES: [TableId; 8] = [
    TableId::ResourceOrigins,
    TableId::ConvoyClients,
    TableId::TradeRoutes,
    TableId::RegionalConvoyEfficiency,
    TableId::ResourceExchanges,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::Provinces,
];

fn incomplete(mirror: Option<&Mirror>, table: TableId) -> Result<(), String> {
    let Some(mirror) = mirror else { return Err(format!("{table:?} is not read yet")) };
    let hour = mirror.head().unwrap_or_default();
    match mirror.status() {
        Some(Status::Complete) => Ok(()),
        Some(Status::Truncated { missing }) => Err(format!("hour {hour} {table:?} is truncated ({missing} missing rows)")),
        Some(Status::Unavailable { .. }) => Err(format!("hour {hour} {table:?} is unavailable: {}", mirror.reason().unwrap_or(UNREAD_REASON))),
        Some(Status::NotRecorded) => Err(format!("hour {hour} {table:?} is not recorded at this hour")),
        None => Err(format!("hour {hour} {table:?} is not recorded here: a gap in the recording")),
    }
}

fn path_of(mirror: &Mirror, column: List, slot: u32) -> Vec<u32> {
    match mirror.list(column, slot) {
        Cell::Value(bytes) => bytes.chunks_exact(4).map(|item| u32::from_le_bytes([item[0], item[1], item[2], item[3]])).collect(),
        _ => Vec::new(),
    }
}

pub struct Convoys {
    retiring: RetirementFlag,
    routes: Arc<dyn Routes>,
    given: Mutex<Option<(Choice, Arc<Vision>)>>,
    fresh_vision: AtomicBool,
    vision: Option<(Choice, u32)>,
    columns: Option<Columns>,
    refused: Option<String>,
    origins: Rows<Origin>,
    clients: Rows<Client>,
    trade: Rows<Route>,
    regional: Rows<Regional>,
    exchanges: Rows<ExchangeOrder>,
    links: Arc<BTreeMap<u32, u32>>,
    links_changed: bool,
    provinces: Arc<Provinces>,
    provinces_version: u64,
    behind: [HashSet<u128>; 5],
    viewed: HashSet<u32>,
    links_held: bool,
    referenced: HashSet<RouteKey>,
    serving: HashSet<u32>,
    worker: Option<(Sender<Job>, viewer_runtime::landed::Receiver<Made>)>,
    in_flight: usize,
    #[cfg(test)]
    blocking_waits: usize,
    #[cfg(test)]
    sent: Arc<(Mutex<(usize, bool)>, std::sync::Condvar)>,
    #[cfg(test)]
    jobs_sent: usize,
    worker_bytes: usize,
    moved: bool,
    window: bool,
    product_seq: u64,
    product_hour: Option<Hour>,
    culture: Option<Result<String, String>>,
    pending: Vec<(usize, Vec<u32>, Vec<u128>)>,
    stale: bool,
    product: Arc<TradeRoutes>,
    landed: Option<viewer_runtime::landed::Landed<Arc<TradeRoutes>>>,
    product_for: Option<u32>,
    statuses: HashMap<usize, Status>,
    hidden: bool,
    frame: u64,
    since: Option<u64>,
    marked: [usize; 5],
}

const NOT_DRAWN: &str = "no trade route is drawn in this map mode";

impl Convoys {
    pub fn new(routes: Arc<dyn Routes>) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            routes,
            given: Mutex::new(None),
            fresh_vision: AtomicBool::new(false),
            vision: None,
            columns: None,
            refused: Some("not composed yet".into()),
            origins: Rows::default(),
            clients: Rows::default(),
            trade: Rows::default(),
            regional: Rows::default(),
            exchanges: Rows::default(),
            links: Arc::default(),
            links_changed: false,
            provinces: Arc::default(),
            provinces_version: 0,
            behind: Default::default(),
            viewed: HashSet::new(),
            links_held: false,
            referenced: HashSet::new(),
            serving: HashSet::new(),
            worker: None,
            in_flight: 0,
            #[cfg(test)]
            blocking_waits: 0,
            #[cfg(test)]
            sent: Arc::default(),
            #[cfg(test)]
            jobs_sent: 0,
            worker_bytes: 0,
            moved: false,
            window: false,
            product_seq: 0,
            product_hour: None,
            culture: None,
            pending: Vec::new(),
            stale: false,
            product: Arc::default(),
            landed: None,
            product_for: None,
            statuses: HashMap::new(),
            hidden: false,
            frame: 0,
            since: None,
            marked: [0; 5],
        }
    }

    pub fn fresh(&self) -> Self {
        Self { columns: self.columns, hidden: self.hidden, vision: self.vision.clone(), stale: self.vision.is_some(), ..Self::new(Arc::clone(&self.routes)) }
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        *self.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((choice, vision));
        self.fresh_vision.store(true, Ordering::Release);
    }

    pub fn perspective(&self) -> Option<&Choice> {
        self.vision.as_ref().map(|(choice, _)| choice)
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    fn release(&mut self) {
        let empty = Self::new(Arc::clone(&self.routes));
        let kept = std::mem::replace(self, empty);
        self.retiring = kept.retiring;
        (self.given, self.fresh_vision, self.vision, self.window) = (kept.given, kept.fresh_vision, kept.vision, kept.window);
        (self.product_seq, self.frame, self.marked) = (kept.product_seq, kept.frame, kept.marked);
        self.hidden = true;
        self.refused = Some(NOT_DRAWN.into());
    }

    pub fn in_window(mut self) -> Self {
        self.window = true;
        self
    }

    pub fn product_seq(&self) -> u64 {
        self.product_seq
    }

    pub fn product(&self) -> &TradeRoutes {
        &self.product
    }

    pub fn product_shared(&self) -> Arc<TradeRoutes> {
        Arc::clone(&self.product)
    }

    pub fn shown(&self) -> Option<viewer_runtime::landed::Landed<(u64, Arc<TradeRoutes>)>> {
        let ours = self.vision.as_ref().map(|&(_, ours)| ours)?;
        let landed = self.landed.as_ref().filter(|_| self.product_for == Some(ours))?;
        Some(landed.clone().map(|routes| (self.product_seq, routes)))
    }

    pub fn marked(&self) -> [usize; 5] {
        self.marked
    }

    fn columns(&self) -> Columns {
        self.columns.expect("the convoys layer is declared before it composes")
    }

    fn read_origin(&self, mirror: &Mirror, slot: u32) -> Origin {
        let c = self.columns();
        let key = mirror.key(slot).unwrap_or_default();
        Origin {
            country: key as u32,
            state: (key >> 32) as u32,
            list: mirror.number(c.origin_list, slot).value().unwrap_or_default(),
            ordinal: mirror.number(c.origin_ordinal, slot).value().unwrap_or_default(),
            bad: mirror.number(c.origin_bad, slot).value().unwrap_or_default(),
            convoys_assigned: mirror.number(c.origin_convoys, slot).value().unwrap_or_default(),
        }
    }

    fn read_client(&self, mirror: &Mirror, slot: u32) -> Client {
        let c = self.columns();
        let number = |column: Num<u32>| mirror.number(column, slot).value().unwrap_or_default();
        Client {
            handle: mirror.key(slot).unwrap_or_default() as u32,
            category: mirror.number(c.client_category, slot).value().unwrap_or_default(),
            owner: number(c.client_owner),
            other: number(c.client_other),
            route_class: mirror.number(c.client_route_class, slot).value().unwrap_or_default(),
            route_country: number(c.client_route_country),
            route_other: number(c.client_route_other),
            bad: mirror.number(c.client_bad, slot).value().unwrap_or_default(),
            convoys_assigned: mirror.number(c.client_convoys, slot).value().unwrap_or_default(),
            owner_ordinal: mirror.number(c.client_owner_ordinal, slot).value(),
            other_ordinal: mirror.number(c.client_other_ordinal, slot).value(),
        }
    }

    fn read_route(&self, mirror: &Mirror, slot: u32) -> Route {
        let c = self.columns();
        let key = mirror.key(slot).unwrap_or_default();
        let number = |column: Num<u32>| mirror.number(column, slot).value().unwrap_or_default();
        Route {
            country: (key >> 40) as u32,
            class: (key >> 32) as u8,
            other: key as u32,
            kind: mirror.number(c.route_kind, slot).value().unwrap_or_default(),
            from: number(c.route_from),
            to: number(c.route_to),
            path: path_of(mirror, c.route_path, slot),
            source_state: mirror.number(c.route_source, slot).value(),
            destination_state: mirror.number(c.route_destination, slot).value(),
        }
    }

    fn read_regional(&self, mirror: &Mirror, slot: u32) -> Regional {
        let key = mirror.key(slot).unwrap_or_default();
        Regional { country: (key >> 32) as u32, region: key as u32, efficiency: mirror.number(self.columns().efficiency, slot).value().unwrap_or_default() }
    }

    fn read_exchange(&self, mirror: &Mirror, slot: u32) -> ExchangeOrder {
        let c = self.columns();
        let number = |column: Num<u32>| mirror.number(column, slot).value().unwrap_or_default();
        ExchangeOrder { resource: number(c.exchange_resource), import_ordinal: number(c.exchange_import), export_ordinal: number(c.exchange_export) }
    }

    fn take_row(&mut self, mirrors: &Mirrors, table: TableId, slot: Option<u32>, key: u128) -> bool {
        let Some(mirror) = mirrors.get(table) else { return false };
        if let Some(at) = deferred_at(table) {
            self.behind[at].remove(&key);
        }
        let live = slot.filter(|&slot| mirror.live(slot));
        match (table, live) {
            (TableId::ResourceOrigins, Some(slot)) => self.origins.put(key, self.read_origin(mirror, slot)),
            (TableId::ResourceOrigins, None) => self.origins.remove(key),
            (TableId::ConvoyClients, Some(slot)) => self.clients.put(key, self.read_client(mirror, slot)),
            (TableId::ConvoyClients, None) => self.clients.remove(key),
            (TableId::TradeRoutes, Some(slot)) => self.trade.put(key, self.read_route(mirror, slot)),
            (TableId::TradeRoutes, None) => self.trade.remove(key),
            (TableId::RegionalConvoyEfficiency, Some(slot)) => self.regional.put(key, self.read_regional(mirror, slot)),
            (TableId::RegionalConvoyEfficiency, None) => self.regional.remove(key),
            (TableId::ResourceExchanges, Some(slot)) => self.exchanges.put(key, self.read_exchange(mirror, slot)),
            (TableId::ResourceExchanges, None) => self.exchanges.remove(key),
            (TableId::CountryLinks, Some(slot)) => {
                let canonical = mirror.number(self.columns().link_slot, slot).value();
                let changed = match canonical {
                    Some(canonical) => self.links.get(&(key as u32)) != Some(&canonical) && Arc::make_mut(&mut self.links).insert(key as u32, canonical) != Some(canonical),
                    None => self.links.contains_key(&(key as u32)) && Arc::make_mut(&mut self.links).remove(&(key as u32)).is_some(),
                };
                self.links_changed |= changed;
                changed
            }
            (TableId::CountryLinks, None) => {
                let changed = self.links.contains_key(&(key as u32)) && Arc::make_mut(&mut self.links).remove(&(key as u32)).is_some();
                self.links_changed |= changed;
                changed
            }
            (TableId::Provinces, live) => {
                let c = self.columns();
                let id = key as u32;
                let (state, port) = live.map_or((None, None), |slot| (mirror.number(c.province_state, slot).value(), mirror.number(c.province_port, slot).value().filter(|&port| port > 0)));
                let port = port.map(|port| port as u32);
                if self.provinces.state_of.get(&id).copied() == state && self.provinces.ports.get(&id).copied() == port {
                    return false;
                }
                let provinces = Arc::make_mut(&mut self.provinces);
                let state_changed = match state {
                    Some(state) => provinces.state_of.insert(id, state) != Some(state),
                    None => provinces.state_of.remove(&id).is_some(),
                };
                let port_changed = match port {
                    Some(port) => provinces.ports.insert(id, port) != Some(port),
                    None => provinces.ports.remove(&id).is_some(),
                };
                self.provinces_version += 1;
                state_changed || port_changed
            }
            (TableId::CountryAppearance, _) => {
                self.culture = None;
                true
            }
            _ => false,
        }
    }

    fn names(&self, country: u32) -> bool {
        country != 0 && (self.viewed.contains(&country) || !self.links_held || !self.links.contains_key(&country))
    }

    fn serves(&self, client: &Client) -> bool {
        !matches!(client.category, trade_routes::SUPPLY_TRANSFER | trade_routes::IMPORT | trade_routes::LEND_LEASE | trade_routes::MARKET)
            || self.names(client.owner)
            || self.names(client.other)
    }

    fn touches(&self, table: TableId, key: u128) -> bool {
        match table {
            TableId::ResourceOrigins => self.names(key as u32),
            TableId::ConvoyClients => self.clients.get(key).is_some_and(|client| self.serves(client)),
            TableId::TradeRoutes => self.referenced.contains(&((key >> 40) as u32, (key >> 32) as u8, key as u32)),
            TableId::RegionalConvoyEfficiency => self.names((key >> 32) as u32),
            TableId::ResourceExchanges => self.serving.contains(&((key >> 32) as u32)),
            TableId::CountryAppearance => self.viewed.contains(&(key as u32)),
            _ => true,
        }
    }

    fn read_all(&mut self, mirrors: &Mirrors) {
        self.origins.clear();
        self.clients.clear();
        self.trade.clear();
        self.regional.clear();
        self.exchanges.clear();
        self.links = Arc::default();
        self.links_changed = true;
        self.provinces = Arc::default();
        self.provinces_version += 1;
        self.behind = Default::default();
        self.pending.clear();
        self.statuses = mirrors.iter().filter_map(|mirror| mirror.status().map(|status| (mirror.table().index(), status))).collect();
        for table in TABLES {
            let Some(mirror) = mirrors.get(table) else { continue };
            let mut rows: Vec<(u32, u128)> = mirror.rows().filter_map(|slot| Some((slot, mirror.key(slot)?))).collect();
            rows.sort_unstable_by_key(|&(_, key)| key);
            for (slot, key) in rows {
                self.take_row(mirrors, table, Some(slot), key);
            }
        }
        self.stale = true;
    }

    fn take_behind(&mut self, mirrors: &Mirrors, tables: &[TableId]) {
        for &table in tables {
            let (Some(at), Some(mirror)) = (deferred_at(table), mirrors.get(table)) else { continue };
            let keys: Vec<u128> = self.behind[at].iter().copied().collect();
            for key in keys {
                let slot = mirror.slot(key);
                let wanted = match slot {
                    Some(slot) => self.touches(table, key) || self.would_touch(table, mirror, slot),
                    None => true,
                };
                if wanted {
                    self.take_row(mirrors, table, slot, key);
                }
            }
        }
    }

    fn would_touch(&self, table: TableId, mirror: &Mirror, slot: u32) -> bool {
        if table != TableId::ConvoyClients {
            return false;
        }
        let c = self.columns();
        let number = |column: Num<u32>| mirror.number(column, slot).value().unwrap_or_default();
        let category = mirror.number(c.client_category, slot).value().unwrap_or_default();
        !matches!(category, trade_routes::SUPPLY_TRANSFER | trade_routes::IMPORT | trade_routes::LEND_LEASE | trade_routes::MARKET) || self.names(number(c.client_owner)) || self.names(number(c.client_other))
    }

    fn read_culture(&self, mirrors: &Mirrors, countries: &Countries<'_>) -> Result<String, String> {
        let appearance = mirrors.get(TableId::CountryAppearance);
        incomplete(appearance, TableId::CountryAppearance)?;
        let appearance = appearance.expect("checked above");
        let row = appearance
            .rows()
            .filter_map(|slot| Some((appearance.key(slot)?, slot)))
            .filter(|&(key, _)| countries.viewed(key as u32) == Ok(true))
            .min()
            .ok_or("the viewed country has no CountryAppearance row")?;
        let bytes = match appearance.text(self.columns().culture, row.1) {
            Cell::Value(id) => appearance.string(id).map(<[u8]>::to_vec).unwrap_or_default(),
            _ => Vec::new(),
        };
        String::from_utf8(bytes).map_err(|_| "the viewed country's graphical culture is not text".to_owned())
    }

    fn make(&mut self, mirrors: &Mirrors) {
        self.marked[1] += 1;
        let ours = self.vision.as_ref().map_or(0, |&(_, ours)| ours);
        let links = mirrors.get(TableId::CountryLinks);
        let held = links.is_some_and(|links| matches!(links.status(), Some(Status::Complete | Status::Truncated { .. })));
        let viewed: HashSet<u32> = if held { self.links.iter().filter(|&(_, &slot)| slot == ours).map(|(&raw, _)| raw).collect() } else { HashSet::new() };
        let renamed = held != self.links_held || viewed != self.viewed || std::mem::take(&mut self.links_changed);
        if viewed != self.viewed {
            self.culture = None;
        }
        self.links_held = held;
        self.viewed = viewed;
        if renamed {
            self.take_behind(mirrors, &[TableId::ResourceOrigins, TableId::ConvoyClients, TableId::RegionalConvoyEfficiency]);
        }
        let origins: Vec<Origin> = self.origins.values.iter().filter(|origin| self.names(origin.country)).cloned().collect();
        let clients: Vec<Client> = self.clients.values.iter().filter(|client| self.serves(client)).cloned().collect();
        let regional: Vec<Regional> = self.regional.values.iter().filter(|row| self.names(row.country)).cloned().collect();
        let referenced: HashSet<RouteKey> = origins.iter().map(|origin| (origin.country, trade_routes::ORIGIN_ROUTE_CLASS, origin.state)).chain(clients.iter().map(|client| (client.route_country, client.route_class, client.route_other))).collect();
        let serving: HashSet<u32> = clients.iter().map(|client| client.handle).collect();
        let routes_wanted: Vec<u128> = referenced.difference(&self.referenced).map(|&(country, class, other)| u128::from(country) << 40 | u128::from(class) << 32 | u128::from(other)).collect();
        let newly_serving = serving.difference(&self.serving).next().is_some();
        (self.referenced, self.serving) = (referenced, serving);
        if let (Some(at), Some(mirror)) = (deferred_at(TableId::TradeRoutes), mirrors.get(TableId::TradeRoutes)) {
            for key in routes_wanted {
                if self.behind[at].contains(&key) {
                    self.take_row(mirrors, TableId::TradeRoutes, mirror.slot(key), key);
                }
            }
        }
        if newly_serving {
            self.take_behind(mirrors, &[TableId::ResourceExchanges]);
        }
        let routes: Vec<Route> = self.trade.values.iter().filter(|route| self.referenced.contains(&(route.country, route.class, route.other))).cloned().collect();
        let tables = [TableId::ResourceOrigins, TableId::ConvoyClients, TableId::TradeRoutes, TableId::RegionalConvoyEfficiency].into_iter().try_for_each(|table| incomplete(mirrors.get(table), table)).map(|()| (origins, clients, routes, regional));
        let exchanges: BTreeMap<u32, ExchangeOrder> = match incomplete(mirrors.get(TableId::ResourceExchanges), TableId::ResourceExchanges) {
            Ok(()) => self.exchanges.keys.iter().zip(&self.exchanges.values).map(|(&key, &order)| ((key >> 32) as u32, order)).collect(),
            Err(_) => BTreeMap::new(),
        };
        let slots = if held { Arc::clone(&self.links) } else { Arc::default() };
        if self.culture.is_none() {
            let culture = self.read_culture(mirrors, &Countries::over(&slots, links_gap(links), ours));
            self.culture = Some(culture);
        }
        let job = Job {
            hour: TABLES.iter().find_map(|&table| mirrors.get(table).and_then(Mirror::head)),
            ours,
            tables,
            slots,
            gap: links_gap(links),
            exchanges,
            culture: self.culture.clone().expect("read above"),
            provinces: Arc::clone(&self.provinces),
            provinces_read: incomplete(mirrors.get(TableId::Provinces), TableId::Provinces),
            provinces_version: self.provinces_version,
        };
        if self.worker.is_none() {
            #[cfg(test)]
            let sent = {
                self.sent = Arc::new((Mutex::new((0, false)), std::sync::Condvar::new()));
                self.jobs_sent = 0;
                Arc::clone(&self.sent)
            };
            match Maker::spawn(
                Arc::clone(&self.routes),
                #[cfg(test)]
                sent,
            ) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.refused = Some(format!("The convoys layer is refused: its worker cannot start: {error}"));
                    return;
                }
            }
        }
        if let Some((jobs, _)) = &self.worker
            && jobs.send(job).is_ok()
        {
            self.in_flight += 1;
            #[cfg(test)]
            {
                self.jobs_sent += 1;
            }
            self.moved = false;
        }
    }

    fn exchange(&mut self, wait: bool) -> io::Result<()> {
        #[cfg(test)]
        if wait && self.in_flight > 0 {
            self.blocking_waits += 1;
        }
        let Some((_, made)) = &self.worker else { return Ok(()) };
        while self.in_flight > 0 {
            let next = if wait { self.retiring.receive(made)? } else { made.try_recv().ok() };
            let Some(made) = next else { break };
            self.in_flight = self.in_flight.saturating_sub(made.jobs);
            if self.vision.as_ref().is_none_or(|&(_, ours)| ours != made.ours) {
                continue;
            }
            let (ours, hour, reused, built, micros, bytes) = (made.ours, made.hour, made.reused, made.made, made.micros, made.bytes);
            let routes = made.map(|made| Arc::new(made.product));
            self.product = Arc::clone(&*routes);
            self.landed = Some(routes);
            self.product_for = Some(ours);
            self.product_hour = hour;
            self.product_seq += 1;
            self.marked[2] += reused;
            self.marked[3] += built;
            self.marked[4] += micros;
            self.worker_bytes = bytes;
        }
        self.stale |= self.moved && self.product.report.unavailable.is_some();
        Ok(())
    }

    fn settle_dirty(&mut self, mirrors: &Mirrors, wait: bool) -> io::Result<()> {
        self.frame += 1;
        if self.columns.is_none() {
            return Ok(());
        }
        if self.fresh_vision.swap(false, Ordering::AcqRel) {
            self.vision = self.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take().map(|(choice, vision)| (choice, vision.ours()));
            self.stale = true;
        }
        if self.vision.is_none() {
            self.refused = Some("The convoys layer is refused: it is given no vision of the viewing country yet".into());
            (self.pending, self.stale, self.since) = (Vec::new(), false, None);
            (self.worker, self.in_flight, self.worker_bytes) = (None, 0, 0);
            (self.product, self.landed, self.product_for, self.product_hour) = (Arc::default(), None, None, None);
            return Ok(());
        }
        if TABLES.iter().any(|&table| mirrors.get(table).is_none()) {
            return Ok(());
        }
        if self.refused.take().is_some() {
            self.read_all(mirrors);
        }
        if self.in_flight == 0 && !self.stale && self.pending.is_empty() {
            self.product_hour = TABLES.iter().find_map(|&table| mirrors.get(table).and_then(Mirror::head)).or(self.product_hour);
        }
        for (table, slots, deleted) in std::mem::take(&mut self.pending) {
            let table = TableId::ALL[table];
            let Some(mirror) = mirrors.get(table) else { continue };
            let mut changed = false;
            let behind = deferred_at(table);
            for key in deleted {
                if mirror.slot(key).is_none() {
                    if self.touches(table, key) {
                        changed |= self.take_row(mirrors, table, None, key);
                    } else if let Some(at) = behind {
                        self.behind[at].insert(key);
                    }
                }
            }
            for slot in slots {
                let Some(key) = mirror.key(slot) else { continue };
                if self.touches(table, key) || self.would_touch(table, mirror, slot) {
                    changed |= self.take_row(mirrors, table, Some(slot), key);
                } else if let Some(at) = behind {
                    self.behind[at].insert(key);
                }
            }
            self.stale |= changed;
        }
        self.exchange(wait)?;
        if self.stale {
            self.stale = false;
            self.make(mirrors);
            self.exchange(wait)?;
        }
        self.since = self.stale.then(|| self.since.unwrap_or(self.frame));
        Ok(())
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        differ(&self.product, &other.product, self.refused(), other.refused())
    }
}

pub fn differ(ours: &TradeRoutes, theirs: &TradeRoutes, refused: Option<&str>, their_refusal: Option<&str>) -> (u64, Option<String>) {
    let mut differing = 0;
    let mut first = None;
    let mut note = |what: String| {
        differing += 1;
        first.get_or_insert(what);
    };
    if refused != their_refusal {
        note(format!("refused: {refused:?} against {their_refusal:?}"));
    }
    if ours.convoys != theirs.convoys {
        note(format!("convoys: {} lines against {}", ours.convoys.len(), theirs.convoys.len()));
    }
    if ours.report != theirs.report {
        note(format!("report: {:?} against {:?}", ours.report, theirs.report));
    }
    if ours.field.draws.len() != theirs.field.draws.len() || ours.field.trade_route_hovers != theirs.field.trade_route_hovers || ours.field.trade_route_lines != theirs.field.trade_route_lines {
        note(format!("lines: {} draws against {}", ours.field.draws.len(), theirs.field.draws.len()));
    }
    (differing, first)
}

impl Layer for Convoys {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        "convoys"
    }

    fn placed(&mut self, _view: &viewer_layer_core::layers::View) -> Option<u64> {
        Some(0)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if self.window {
            inputs.after("vision");
        }
        if !visibility::of(&view.map_mode).trade_routes {
            self.release();
            return Ok(());
        }
        self.hidden = false;
        for table in TABLES {
            inputs.table(table, true)?;
        }
        self.columns = Some(Columns {
            origin_list: inputs.number(TableId::ResourceOrigins, "list")?,
            origin_bad: inputs.number(TableId::ResourceOrigins, "bad")?,
            origin_convoys: inputs.number(TableId::ResourceOrigins, "convoys_assigned")?,
            origin_ordinal: inputs.number(TableId::ResourceOrigins, "ordinal")?,
            client_category: inputs.number(TableId::ConvoyClients, "category")?,
            client_owner: inputs.number(TableId::ConvoyClients, "owner")?,
            client_other: inputs.number(TableId::ConvoyClients, "other")?,
            client_route_class: inputs.number(TableId::ConvoyClients, "route_class")?,
            client_route_country: inputs.number(TableId::ConvoyClients, "route_country")?,
            client_route_other: inputs.number(TableId::ConvoyClients, "route_other")?,
            client_bad: inputs.number(TableId::ConvoyClients, "bad")?,
            client_convoys: inputs.number(TableId::ConvoyClients, "convoys_assigned")?,
            client_owner_ordinal: inputs.number(TableId::ConvoyClients, "owner_ordinal")?,
            client_other_ordinal: inputs.number(TableId::ConvoyClients, "other_ordinal")?,
            route_kind: inputs.number(TableId::TradeRoutes, "kind")?,
            route_from: inputs.number(TableId::TradeRoutes, "from")?,
            route_to: inputs.number(TableId::TradeRoutes, "to")?,
            route_path: inputs.list(TableId::TradeRoutes, "path")?,
            route_source: inputs.number(TableId::TradeRoutes, "source_state")?,
            route_destination: inputs.number(TableId::TradeRoutes, "destination_state")?,
            efficiency: inputs.number(TableId::RegionalConvoyEfficiency, "efficiency")?,
            exchange_resource: inputs.number(TableId::ResourceExchanges, "resource")?,
            exchange_import: inputs.number(TableId::ResourceExchanges, "import_ordinal")?,
            exchange_export: inputs.number(TableId::ResourceExchanges, "export_ordinal")?,
            link_slot: inputs.number(TableId::CountryLinks, "canonical_slot")?,
            culture: inputs.text(TableId::CountryAppearance, "graphical_culture")?,
            province_state: inputs.number(TableId::Provinces, "state")?,
            province_port: inputs.number(TableId::Provinces, "port_sea")?,
        });
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if self.hidden {
            self.release();
            return Ok(());
        }
        if self.columns.is_none() {
            return Ok(());
        }
        let marked = self.marked;
        self.refused = Some("composing".into());
        self.settle_dirty(mirrors, true)?;
        self.marked = marked;
        Ok(())
    }

    fn compose_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<()> {
        self.take_vision(upstream);
        self.compose(mirrors)
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        if step.gap() || self.columns.is_none() {
            return;
        }
        let c = self.columns();
        let read: [&[usize]; 8] = [
            &[c.origin_list.column, c.origin_bad.column, c.origin_convoys.column, c.origin_ordinal.column],
            &[
                c.client_category.column,
                c.client_owner.column,
                c.client_other.column,
                c.client_route_class.column,
                c.client_route_country.column,
                c.client_route_other.column,
                c.client_bad.column,
                c.client_convoys.column,
                c.client_owner_ordinal.column,
                c.client_other_ordinal.column,
            ],
            &[c.route_kind.column, c.route_from.column, c.route_to.column, c.route_path.column, c.route_source.column, c.route_destination.column],
            &[c.efficiency.column],
            &[c.exchange_resource.column, c.exchange_import.column, c.exchange_export.column],
            &[c.link_slot.column],
            &[c.culture.column],
            &[c.province_state.column, c.province_port.column],
        ];
        for changes in step.tables() {
            let Some(at) = TABLES.iter().position(|table| table.index() == changes.table()) else { continue };
            if mirrors.get(TABLES[at]).is_none() {
                continue;
            }
            if changes.status().is_some_and(|(status, _)| self.statuses.insert(changes.table(), status) != Some(status)) {
                self.stale = true;
            }
            self.mark(changes, read[at]);
        }
        self.moved = true;
        self.stale |= self.product.report.unavailable.is_some();
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.settle_dirty(mirrors, settling.strict())
    }

    fn settle_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        self.take_vision(upstream);
        self.settle_dirty(mirrors, settling.strict())
    }


    fn carried(&self) -> Carried {
        if self.refused.is_some() {
            return Carried::default();
        }
        let entities = self.pending.iter().map(|(_, slots, deleted)| slots.len() + deleted.len()).sum::<usize>() + usize::from(self.stale);
        Carried { entities }
    }

    fn published(&self) -> Published {
        if self.in_flight == 0 {
            return Published::Current;
        }
        self.product_hour.map_or(Published::Nothing, Published::At)
    }

    fn held(&self) -> usize {
        let paths: usize = self.trade.values.iter().map(|route| route.path.capacity() * 4).sum();
        let behind: usize = self.behind.iter().map(|keys| keys.capacity() * 24).sum();
        self.origins.bytes() + self.clients.bytes() + self.trade.bytes() + paths + self.regional.bytes() + self.exchanges.bytes() + self.links.len() * 24 + (self.provinces.ports.len() + self.provinces.state_of.len()) * 24 + behind + self.worker_bytes
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    #[cfg(test)]
    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Convoys {
    #[cfg(test)]
    fn wait_replies_ready(&self) {
        let (state, changed) = &*self.sent;
        let mut held = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while held.0 < self.jobs_sent {
            assert!(!held.1, "the convoys worker stopped before answering all {} jobs ({} answered)", self.jobs_sent, held.0);
            held = changed.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn take_vision(&mut self, upstream: &Upstream<'_>) {
        if self.window
            && let Some(vision) = upstream.get::<viewer_layer_core::vision::Vision>()
        {
            match vision.ours() {
                Some(ours) if self.vision.as_ref().is_none_or(|(choice, held)| *held != ours || choice != vision.choice()) => {
                    self.vision = Some((vision.choice().clone(), ours));
                    self.stale |= self.columns.is_some();
                }
                None => self.vision = None,
                Some(_) => {}
            }
        }
    }

    fn mark(&mut self, changes: &Changes, read: &[usize]) {
        let deleted: Vec<u128> = changes.deleted().keys().unwrap_or_default().to_vec();
        let mut slots: Vec<u32> = changes.inserted().slots().to_vec();
        for written in changes.columns().iter().filter(|written| read.contains(&written.column())) {
            slots.extend_from_slice(written.slots());
        }
        if deleted.is_empty() && slots.is_empty() {
            return;
        }
        self.marked[0] += deleted.len() + slots.len();
        self.since.get_or_insert(self.frame);
        self.pending.push((changes.table(), slots, deleted));
    }
}

#[cfg(test)]
mod tests;
