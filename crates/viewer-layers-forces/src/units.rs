use viewer_layer_core::counter_rows::{Row, Rows};

pub mod meshes;
pub mod models;

use viewer_layer_core::{layers::{Carried, Change, Settling, Dirty, Gpu, Inputs, Layer, List, Num, Published, Retirement, RetirementFlag, Upstream, View}, vision::Vision as VisionLayer};
use viewer_layer_core::playhead_host::mirror::{Cell, Mirror, Mirrors};
use playhead::Step;
use replay_columnar::manifest::Status;
use replay_data::rows::TableId;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use viewer_gpu::{
    camera::{MapDefines, MapSize, Placement},
    province_admission::Admission,
    province_schedule::ProvinceSet,
};
use viewer_unit_reads::counter_move::{DivisionRoute, Position, RouteWorld, UnitRoute};
use viewer_foundation::{perspective::{Choice, Sight, Vision}, unit_rules::STANDSTILL};
use viewer_models::entity_animation::{ATTACK, DEFEND};
use viewer_map_modes::map_mode::visibility;
use viewer_unit_history::unit_history::Target;
use viewer_unit_models::unit_models::{self, LandStack, Unplaced};

const NONE: u32 = u32::MAX;

type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

pub trait Ground: Send + Sync {
    fn anchor(&self, province: u32, slot: u32) -> Option<[f32; 3]>;
    fn world(&self) -> Option<RouteWorld<'_>>;
    fn transport_scale(&self) -> f32;
    fn bounds(&self) -> &BTreeMap<u32, [f32; 4]>;
    fn units_cutoff(&self) -> f32;
    fn ships_cutoff(&self) -> f32;
    fn map(&self) -> (MapSize, MapDefines);
    fn unit_models(&self) -> Option<&viewer_unit_models::unit_models::UnitModelAssets>;
}


pub type StackKey = (u32, u32, Option<[u32; 2]>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub transport: bool,
    pub template: u64,
    pub oldest: u64,
    pub roster_owner: u32,
    pub expeditionary_owner: u32,
    pub status: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickDivision {
    pub id: u64,
    pub owner: u32,
    pub roster: Option<u32>,
    pub kind: Option<u32>,
    pub hop: Option<u32>,
    pub unrouted: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    Unplaced(Unplaced),
    Status,
    Unseen,
}

#[derive(Clone, Debug)]
struct Placing {
    province: u32,
    owner: u32,
    oldest: u64,
    route: Option<DivisionRoute>,
    combat: Option<u32>,
    training: Option<bool>,
    uv_moving: bool,
    scale: f32,
}

#[derive(Clone, Copy, Debug)]
struct Scalars {
    province: u32,
    owner: u32,
    oldest: u64,
    combat: Option<u32>,
    training: Option<bool>,
    uv_moving: bool,
    scale: f32,
}

impl Scalars {
    fn with(self, route: Option<DivisionRoute>) -> Placing {
        let Self { province, owner, oldest, combat, training, uv_moving, scale } = self;
        Placing { province, owner, oldest, route, combat, training, uv_moving, scale }
    }
}

impl Placing {
    fn same(&self, other: &Scalars, route: Option<&DivisionRoute>) -> bool {
        let routes = match (&self.route, route) {
            (None, None) => true,
            (Some(a), Some(b)) => same_route(a, b),
            _ => false,
        };
        routes
            && (self.province, self.owner, self.oldest, self.combat, self.training, self.uv_moving, self.scale.to_bits())
                == (other.province, other.owner, other.oldest, other.combat, other.training, other.uv_moving, other.scale.to_bits())
    }
}

fn same_route(a: &DivisionRoute, b: &DivisionRoute) -> bool {
    (&a.path, a.eta, a.naval_transfer, a.retreat, a.combat_role, a.combat_count, a.stack_kind, a.kind_resolved, a.idle)
        == (&b.path, b.eta, b.naval_transfer, b.retreat, b.combat_role, b.combat_count, b.stack_kind, b.kind_resolved, b.idle)
}

#[derive(Clone, Debug)]
pub struct Figure {
    pub selection: Selection,
    pub admitted: bool,
    pub placed: Option<Result<(Target, Position), Withheld>>,
    last: Option<(Target, Position)>,
    placing: Placing,
}

impl Figure {
    fn drawn(&self) -> Option<&(Target, Position)> {
        match &self.placed {
            Some(Ok(placed)) => Some(placed),
            Some(Err(_)) => None,
            None => self.last.as_ref(),
        }
    }
}

#[derive(Debug)]
struct Stack {
    key: StackKey,
    members: Vec<u32>,
    live: bool,
    admitted: bool,
    figure: Option<Figure>,
    asked: Option<u64>,
}

#[derive(Clone, Debug)]
struct Member {
    stack: u32,
    at: u32,
    id: u64,
    sighted: Option<(u32, u32)>,
    read_at: u32,
    seen: bool,
    stale: bool,
    roster: Option<u32>,
    pick: Option<PickRoute>,
}

impl Default for Member {
    fn default() -> Self {
        Self { stack: NONE, at: 0, id: 0, sighted: None, read_at: NONE, seen: false, stale: true, roster: None, pick: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PickRoute {
    kind: Option<u32>,
    hop: Option<u32>,
    unrouted: bool,
}

#[derive(Clone, Copy)]
enum Seer<'a> {
    Layer(&'a VisionLayer),
    Given(&'a Vision),
}

impl Seer<'_> {
    fn sees(self, unit: u64, owner: u32, province: u32) -> (bool, u32) {
        match self {
            Seer::Layer(vision) => (vision.sees_land(unit, owner, province).is_some_and(|sight| sight != Sight::None), vision.sight_province(unit, province)),
            Seer::Given(vision) => (vision.sees_land(unit, owner, province) != Sight::None, province),
        }
    }
}

struct Window {
    select: Arc<dyn models::Select>,
    views: Option<viewer_layer_core::playhead_host::views::Views>,
    tables: Vec<TableId>,
    meshes: Vec<TableId>,
}

#[derive(Clone, Copy)]
struct Columns {
    province: Num<u32>,
    owner: Num<u32>,
    template: Num<u64>,
    roster: Num<u32>,
    expeditionary: Num<u32>,
    target: Num<u32>,
    retreat: Num<u8>,
    naval_transfer: Num<u8>,
    combat_count: Num<u32>,
    path_total: Num<u32>,
    path: List,
    alternate_total: Num<u32>,
    alternate_path: List,
    combat_role: Num<u8>,
    combat_province: Num<u32>,
    alternate_template: Num<u64>,
    exercising: Num<u8>,
    attackers: List,
    defenders: List,
    link_slot: Num<u32>,
}

fn rows(mirror: Option<&Mirror>) -> Option<&Mirror> {
    mirror.filter(|mirror| matches!(mirror.status(), Some(Status::Complete | Status::Truncated { .. })))
}

fn decode<const N: usize>(bytes: &[u8], out: &mut Vec<u64>) {
    out.clear();
    out.extend(bytes.chunks_exact(N).map(|item| {
        let mut word = [0; 8];
        word[..N].copy_from_slice(item);
        u64::from_le_bytes(word)
    }));
}

fn path_of(mirror: &Mirror, column: List, slot: u32, out: &mut Vec<u32>) {
    out.clear();
    if let Cell::Value(bytes) = mirror.list(column, slot) {
        out.extend(bytes.chunks_exact(4).map(|item| u32::from_le_bytes([item[0], item[1], item[2], item[3]])));
    }
}

const TABLES: [TableId; 6] = [TableId::Divisions, TableId::DivisionMovement, TableId::ArmyIconInputs, TableId::DivisionStatus, TableId::LandCombats, TableId::CountryLinks];

pub struct Units {
    ground: Arc<dyn Ground>,
    retiring: RetirementFlag,
    window: Option<Window>,
    models: Option<models::Models>,
    meshes: Option<meshes::Meshes>,
    asking: FastMap<u64, (u32, meshes::Asked)>,
    touched: Vec<u32>,
    asks: Vec<meshes::Ask>,
    to_upload: bool,
    models_new: bool,
    uploaded: Vec<Target>,
    staged: Option<(viewer_unit_history::unit_history::Field, Vec<Target>)>,
    reading: bool,
    given: Mutex<Option<(Choice, Arc<Vision>)>>,
    fresh_vision: AtomicBool,
    vision: Option<(Choice, Arc<Vision>)>,
    choice: Option<Choice>,
    sight_epoch: Option<u64>,
    sights: Dirty,
    readers: FastMap<u32, Vec<u32>>,
    sighted: usize,
    columns: Option<Columns>,
    refused: Option<String>,
    members: Vec<Member>,
    stacks: Vec<Stack>,
    free: Vec<u32>,
    by_key: FastMap<StackKey, u32>,
    combat: FastMap<u64, u32>,
    combat_scratch: FastMap<u64, u32>,
    combat_ordered: Vec<(u128, u32)>,
    combat_changed: Vec<u64>,
    links: FastMap<u32, u32>,
    divisions: Dirty,
    moved: Dirty,
    route_rows: Rows,
    icon_rows: Rows,
    status_rows: Rows,
    every: bool,
    dirty_stacks: Dirty,
    figures: Dirty,
    camera: Option<(Placement, [u32; 2])>,
    admission: Option<Admission>,
    camera_dirty: bool,
    showing: bool,
    province_known: ProvinceSet,
    province_in: ProvinceSet,
    deferred: Vec<u64>,
    deferred_count: usize,
    combat_dirty: bool,
    links_dirty: bool,
    statuses: HashMap<usize, Status>,
    frame: u64,
    marked: [usize; 3],
    path: Vec<u32>,
    alternate: Vec<u32>,
    items: Vec<u64>,
    #[cfg(any(test, feature = "test-support"))]
    placed: usize,
    #[cfg(any(test, feature = "test-support"))]
    applied_reads: u64,
}

const NOT_DRAWN: &str = "no land unit model is drawn at this view";

impl Units {
    pub fn new(ground: Arc<dyn Ground>) -> Self {
        Self {
            ground,
            retiring: RetirementFlag::default(),
            window: None,
            models: None,
            meshes: None,
            asking: FastMap::default(),
            touched: Vec::new(),
            asks: Vec::new(),
            to_upload: false,
            models_new: false,
            uploaded: Vec::new(),
            staged: None,
            reading: false,
            given: Mutex::new(None),
            fresh_vision: AtomicBool::new(false),
            vision: None,
            choice: None,
            sight_epoch: None,
            sights: Dirty::default(),
            readers: FastMap::default(),
            sighted: 0,
            columns: None,
            refused: Some("not composed yet".into()),
            members: Vec::new(),
            stacks: Vec::new(),
            free: Vec::new(),
            by_key: FastMap::default(),
            combat: FastMap::default(),
            combat_scratch: FastMap::default(),
            combat_ordered: Vec::new(),
            combat_changed: Vec::new(),
            links: FastMap::default(),
            divisions: Dirty::default(),
            moved: Dirty::default(),
            route_rows: Rows::default(),
            icon_rows: Rows::default(),
            status_rows: Rows::default(),
            every: false,
            dirty_stacks: Dirty::default(),
            figures: Dirty::default(),
            camera: None,
            admission: None,
            camera_dirty: false,
            showing: false,
            province_known: ProvinceSet::default(),
            province_in: ProvinceSet::default(),
            deferred: Vec::new(),
            deferred_count: 0,
            combat_dirty: false,
            links_dirty: false,
            statuses: HashMap::new(),
            frame: 0,
            marked: [0; 3],
            path: Vec::new(),
            alternate: Vec::new(),
            items: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            placed: 0,
            #[cfg(any(test, feature = "test-support"))]
            applied_reads: 0,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn placed(&self) -> usize {
        self.placed
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn applied_reads(&self) -> u64 {
        self.applied_reads
    }

    pub fn fresh(&self) -> Self {
        let fresh = Self { reading: self.reading, columns: self.columns, camera: self.camera, camera_dirty: true, showing: self.showing, ..Self::new(Arc::clone(&self.ground)) };
        if let Some((choice, vision)) = &self.vision {
            fresh.set_vision(choice.clone(), Arc::clone(vision));
        }
        fresh
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        *self.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((choice, vision));
        self.fresh_vision.store(true, Ordering::Release);
    }

    pub fn reading_vision(ground: Arc<dyn Ground>) -> Self {
        Self { reading: true, ..Self::new(ground) }
    }

    pub fn in_window(mut self, select: Arc<dyn models::Select>) -> Self {
        self.window = Some(Window { select, views: None, tables: Vec::new(), meshes: Vec::new() });
        self.reading = true;
        self
    }

    pub fn prepared(&self) -> Option<&viewer_runtime::landed::Landed<Arc<viewer_unit_models::unit_models::PreparedModels>>> {
        self.models.as_ref()?.prepared.as_ref().map(|(_, prepared)| prepared)
    }

    pub fn warnings(&self) -> Vec<String> {
        let refusal = self.refused().or_else(|| self.models.as_ref().and_then(|models| models.refused.as_deref()));
        let mut lines = match (self.prepared(), refusal) {
            (Some(prepared), _) => prepared.report.warnings(),
            (None, Some(why)) => return vec![format!("Unit models: not drawn: {why}")],
            (None, None) if self.drawn() => return vec!["Unit models: loading at the playback head".to_owned()],
            (None, None) => return Vec::new(),
        };
        if let Some(meshes) = &self.meshes {
            let mut withheld: BTreeMap<String, usize> = BTreeMap::new();
            for why in meshes.withheld.values() {
                *withheld.entry(why.clone()).or_default() += 1;
            }
            let bounds = self.ground.bounds();
            for stack in self.stacks.iter().filter(|stack| stack.admitted && !bounds.contains_key(&stack.key.0)) {
                if stack.figure.as_ref().is_some_and(|figure| matches!(figure.placed, Some(Ok(_)))) {
                    *withheld.entry(format!("province {} has no content bounds for model admission", stack.key.0)).or_default() += 1;
                }
            }
            let mut land: Vec<String> = withheld.into_iter().map(|(why, count)| format!("Unit models: no model for {count} stacks: {why}.")).collect();
            land.extend(meshes.refused.as_ref().map(|why| format!("Unit models: land figures not drawn: {why}")));
            land.append(&mut lines);
            lines = land;
        }
        lines
    }

    pub fn perspective(&self) -> Option<&Choice> {
        self.vision.as_ref().map(|(choice, _)| choice).or(self.choice.as_ref())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn sighted(&self) -> usize {
        self.sighted
    }

    pub fn drawn(&self) -> bool {
        self.showing
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn movement_positions(&self) -> impl Iterator<Item = (viewer_runtime::unit_handle::Unit, &Position, f32, f32)> {
        let land = self.stacks.iter().filter(|stack| stack.admitted).filter_map(|stack| stack.figure.as_ref()?.drawn()).map(|(target, position)| unit_models::land_movement(target, position));
        let ships = self.prepared().into_iter().flat_map(|prepared| prepared.ship_movement_positions());
        land.chain(ships)
    }

    pub fn figures(&self) -> BTreeMap<StackKey, &Figure> {
        self.stacks.iter().filter_map(|stack| Some((stack.key, stack.figure.as_ref()?))).collect()
    }

    pub fn pick_divisions(&self) -> HashMap<u32, Vec<PickDivision>> {
        let provinces: std::collections::HashSet<u32> =
            self.stacks.iter().filter(|stack| stack.figure.is_some()).map(|stack| stack.key.0).collect();
        let mut slots: Vec<u32> = self
            .stacks
            .iter()
            .filter(|stack| provinces.contains(&stack.key.0))
            .flat_map(|stack| stack.members.iter().copied())
            .collect();
        slots.sort_unstable();
        let mut divisions: HashMap<u32, Vec<PickDivision>> = HashMap::new();
        for slot in slots {
            let Some(member) = self.members.get(slot as usize) else { continue };
            let Some((province, owner)) = member.sighted else { continue };
            let route = member.pick;
            divisions.entry(province).or_default().push(PickDivision {
                id: member.id,
                owner,
                roster: member.roster,
                kind: route.and_then(|route| route.kind),
                hop: route.and_then(|route| route.hop),
                unrouted: route.map(|route| route.unrouted),
            });
        }
        divisions
    }

    pub fn stack_count(&self) -> usize {
        self.by_key.len()
    }

    pub fn front(&self, size: f32) -> Option<([f32; 2], usize)> {
        let mut cells: HashMap<(i32, i32), usize> = HashMap::new();
        for stack in self.by_key.values().map(|&stack| &self.stacks[stack as usize]) {
            let Some(placing) = stack.figure.as_ref().map(|figure| &figure.placing) else { continue };
            if placing.combat.is_none() && placing.route.as_ref().is_none_or(|route| route.path.is_empty()) {
                continue;
            }
            let Some([x, _, z]) = self.ground.anchor(stack.key.0, STANDSTILL) else { continue };
            *cells.entry(((x / size).floor() as i32, (z / size).floor() as i32)).or_default() += 1;
        }
        cells.into_iter().max_by_key(|&(cell, count)| (count, std::cmp::Reverse(cell))).map(|((x, z), count)| ([(x as f32 + 0.5) * size, (z as f32 + 0.5) * size], count))
    }

    pub fn cutoff(&self) -> f32 {
        self.ground.units_cutoff()
    }

    pub fn admitted(&self) -> usize {
        self.by_key.values().filter(|&&stack| self.stacks[stack as usize].admitted).count()
    }

    pub fn admits(&self, province: u32) -> bool {
        self.admission.as_ref().is_none_or(|admission| admission.admits_province(self.ground.bounds(), province))
    }

    pub fn deferred(&self) -> usize {
        self.deferred_count
    }

    fn province_admitted(&mut self, province: u32) -> bool {
        if self.admission.is_none() {
            return true;
        }
        if self.province_known.insert(province) && self.admits(province) {
            self.province_in.insert(province);
        }
        self.province_in.contains(province)
    }

    fn wanted(&mut self, slot: u32, divisions: Option<&Mirror>) -> bool {
        let old = self.members.get(slot as usize).filter(|member| member.stack != NONE).map(|member| self.stacks[member.stack as usize].key.0);
        let new = divisions.filter(|divisions| divisions.live(slot)).and_then(|divisions| divisions.number(self.columns().province, slot).value()).filter(|&province| province != 0);
        old.is_some_and(|province| self.province_admitted(province)) || new.is_some_and(|province| self.province_admitted(province))
    }

    fn mark_moved(&mut self, slot: u32, divisions: Option<&Mirror>) {
        if self.admission.is_none() || self.wanted(slot, divisions) {
            self.mark_division(slot);
            return;
        }
        self.defer(slot);
    }

    fn defer(&mut self, slot: u32) {
        let (word, bit) = (slot as usize / 64, 1u64 << (slot % 64));
        if word >= self.deferred.len() {
            self.deferred.resize(word + 1, 0);
        }
        if self.deferred[word] & bit == 0 {
            self.deferred[word] |= bit;
            self.deferred_count += 1;
        }
    }

    fn readmit(&mut self, mirrors: &Mirrors) {
        self.camera_dirty = false;
        let (size, defines) = self.ground.map();
        self.admission = self.camera.filter(|(_, viewport)| *viewport != [0, 0]).map(|(placement, viewport)| Admission::new(placement.on(size, defines), viewport));
        self.province_known.clear();
        self.province_in.clear();
        if self.deferred_count > 0 {
            let divisions = mirrors.get(TableId::Divisions);
            let mut deferred = std::mem::take(&mut self.deferred);
            for (at, word) in deferred.iter_mut().enumerate() {
                let mut left = *word;
                while left != 0 {
                    let bit = left.trailing_zeros();
                    left &= left - 1;
                    let slot = at as u32 * 64 + bit;
                    if self.wanted(slot, divisions) {
                        *word &= !(1 << bit);
                        self.deferred_count -= 1;
                        self.mark_division(slot);
                    }
                }
            }
            self.deferred = deferred;
        }
        for at in 0..self.stacks.len() {
            if !self.stacks[at].live {
                continue;
            }
            let stack = at as u32;
            let admitted = self.admits(self.stacks[at].key.0);
            if self.stacks[at].admitted != admitted {
                self.touched.push(stack);
                self.to_upload = true;
                self.stacks[at].admitted = admitted;
            }
            if let Some(figure) = &mut self.stacks[at].figure {
                figure.admitted = admitted;
                if admitted && figure.placed.is_none() {
                    self.figures.mark(stack, self.frame);
                }
            }
        }
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    fn columns(&self) -> Columns {
        self.columns.expect("the units layer is declared before it composes")
    }

    fn mark_division(&mut self, slot: u32) {
        if self.divisions.mark(slot, self.frame) {
            self.marked[0] += 1;
        }
    }

    fn mark_stack_of(&mut self, slot: u32) {
        let Some(member) = self.members.get_mut(slot as usize) else { return };
        member.stale = true;
        let stack = member.stack;
        if stack != NONE {
            self.mark_stack(stack);
        }
    }

    fn mark_stack(&mut self, stack: u32) {
        if self.dirty_stacks.mark(stack, self.frame) {
            self.marked[1] += 1;
        }
    }

    fn mark_all(&mut self, mirrors: &Mirrors) {
        self.members.clear();
        self.stacks.clear();
        self.free.clear();
        self.by_key.clear();
        self.divisions.clear();
        self.moved.clear();
        [&mut self.route_rows, &mut self.icon_rows, &mut self.status_rows].into_iter().for_each(Rows::clear);
        self.every = false;
        self.dirty_stacks.clear();
        self.figures.clear();
        self.sights.clear();
        self.readers.clear();
        self.camera_dirty = true;
        self.deferred.clear();
        self.deferred_count = 0;
        self.asking.clear();
        self.touched.clear();
        self.asks.clear();
        if let Some(meshes) = &mut self.meshes {
            meshes.clear();
        }
        self.statuses = mirrors.iter().filter_map(|mirror| mirror.status().map(|status| (mirror.table().index(), status))).collect();
        self.combat_dirty = true;
        self.combat_scratch.clear();
        self.combat_ordered.clear();
        self.combat_changed.clear();
        self.links_dirty = true;
        if let Some(divisions) = rows(mirrors.get(TableId::Divisions)) {
            for slot in divisions.rows() {
                self.divisions.mark(slot, self.frame);
            }
        }
    }

    fn mark_sight(&mut self, slot: u32) {
        if self.sights.mark(slot, self.frame) {
            self.sighted += 1;
        }
    }

    fn mark_sights(&mut self) {
        for slot in 0..self.members.len() as u32 {
            if self.members[slot as usize].stack != NONE {
                self.mark_sight(slot);
            }
        }
    }

    fn take_upstream(&mut self, vision: &VisionLayer, upstream: &Upstream<'_>, mirrors: &Mirrors) {
        let moved = self.choice.as_ref() != Some(vision.choice());
        if moved {
            self.choice = Some(vision.choice().clone());
        }
        if moved || self.sight_epoch != Some(vision.sight_epoch()) {
            self.sight_epoch = Some(vision.sight_epoch());
            self.mark_sights();
            return;
        }
        let Change::Marked(changed) = upstream.changed::<VisionLayer>() else {
            self.mark_sights();
            return;
        };
        for &province in changed.of("province") {
            for &slot in self.readers.get(&(province as u32)).into_iter().flatten() {
                if self.sights.mark(slot, self.frame) {
                    self.sighted += 1;
                }
            }
        }
        let units = changed.of("unit");
        if let Some(divisions) = mirrors.get(TableId::Divisions).filter(|_| !units.is_empty()) {
            for &unit in units {
                if let Some(slot) = divisions.slot(u128::from(unit)).filter(|&slot| self.members.get(slot as usize).is_some_and(|member| member.stack != NONE)) {
                    self.mark_sight(slot);
                }
            }
        }
    }

    fn index(&mut self, slot: u32, read_at: u32) {
        let old = std::mem::replace(&mut self.members[slot as usize].read_at, read_at);
        if old == read_at {
            return;
        }
        if let Some(readers) = self.readers.get_mut(&old) {
            if let Some(at) = readers.iter().position(|&held| held == slot) {
                readers.swap_remove(at);
            }
            if readers.is_empty() {
                self.readers.remove(&old);
            }
        }
        if read_at != NONE {
            self.readers.entry(read_at).or_default().push(slot);
        }
    }

    fn resight(&mut self, slot: u32, seer: Seer<'_>) {
        let Some(&Member { stack, id, sighted: Some((province, owner)), .. }) = self.members.get(slot as usize) else { return };
        if stack == NONE {
            return;
        }
        let (seen, read_at) = seer.sees(id, owner, province);
        self.index(slot, read_at);
        if std::mem::replace(&mut self.members[slot as usize].seen, seen) != seen {
            self.mark_stack(stack);
        }
    }

    fn check(&self, mirrors: &Mirrors, layer: Option<&VisionLayer>) -> Result<(), String> {
        if self.reading {
            let vision = layer.ok_or("The units layer is refused: the vision layer is not registered before it")?;
            if let Some(why) = vision.refused() {
                return Err(format!("The units layer is refused: the vision layer has no vision: {why}"));
            }
        } else if self.vision.is_none() {
            return Err("The units layer is refused: it is given no vision of the viewing country yet".into());
        }
        let links = mirrors.get(TableId::CountryLinks).ok_or("CountryLinks is not read yet")?;
        if let Some(why) = links.not_recorded() {
            return Err(format!("{why}; the core tables Provinces, CountryLinks and Countries carry the political map itself, so this hour cannot be drawn"));
        }
        if mirrors.get(TableId::DivisionStatus).and_then(Mirror::status) == Some(Status::NotRecorded) {
            return Err("The units layer is refused: DivisionStatus is not recorded here, and the DivisionModelInputs witness a recording without it carries is not read".into());
        }
        Ok(())
    }

    fn read_links(&mut self, mirrors: &Mirrors) {
        let columns = self.columns();
        self.links.clear();
        if let Some(links) = mirrors.get(TableId::CountryLinks) {
            for slot in links.rows() {
                if let (Some(key), Some(canonical)) = (links.key(slot), links.number(columns.link_slot, slot).value()) {
                    self.links.insert(key as u32, canonical);
                }
            }
        }
    }

    fn read_combat(&mut self, mirrors: &Mirrors) {
        let columns = self.columns();
        self.combat_scratch.clear();
        if let Some(combats) = rows(mirrors.get(TableId::LandCombats)) {
            self.combat_ordered.clear();
            self.combat_ordered.extend(combats.rows().filter_map(|slot| Some((combats.key(slot)?, slot))));
            self.combat_ordered.sort_unstable();
            for &(_, slot) in &self.combat_ordered {
                for (column, side) in [(columns.attackers, ATTACK), (columns.defenders, DEFEND)] {
                    if let Cell::Value(bytes) = combats.list(column, slot) {
                        decode::<8>(bytes, &mut self.items);
                        for &id in &self.items {
                            self.combat_scratch.insert(id, side);
                        }
                    }
                }
            }
        }
        std::mem::swap(&mut self.combat, &mut self.combat_scratch);
        let divisions = mirrors.get(TableId::Divisions);
        self.combat_changed.clear();
        self.combat_changed.extend(
            self.combat_scratch
                .iter()
                .filter(|(id, side)| self.combat.get(id) != Some(side))
                .map(|(&id, _)| id)
                .chain(self.combat.keys().filter(|id| !self.combat_scratch.contains_key(id)).copied()),
        );
        self.combat_changed.sort_unstable();
        let mut changed = std::mem::take(&mut self.combat_changed);
        for &id in &changed {
            if let Some(slot) = divisions.and_then(|divisions| divisions.slot(u128::from(id))) {
                self.mark_stack_of(slot);
            }
        }
        changed.clear();
        self.combat_changed = changed;
        self.combat_scratch.clear();
        let keep = self.combat.len();
        let ceiling = keep.max(64).saturating_mul(4);
        if self.combat.capacity() > ceiling {
            self.combat.shrink_to(keep);
        }
        if self.combat_scratch.capacity() > ceiling {
            self.combat_scratch.shrink_to(keep);
        }
        self.combat_ordered.clear();
    }

    fn signature(&mut self, mirrors: &Mirrors, id: u64, province: u32) -> (Option<[u32; 2]>, Option<PickRoute>) {
        let columns = self.columns();
        let Some(movement) = rows(mirrors.get(TableId::DivisionMovement)) else { return (None, None) };
        let Some(slot) = movement.slot(u128::from(id)) else { return (None, None) };
        let number = |column: Num<u32>| movement.number(column, slot).value().unwrap_or(0);
        let byte = |column: Num<u8>| movement.number(column, slot).value().unwrap_or(0);
        let (mut path, mut alternate) = (std::mem::take(&mut self.path), std::mem::take(&mut self.alternate));
        path_of(movement, columns.path, slot, &mut path);
        path_of(movement, columns.alternate_path, slot, &mut alternate);
        let unit = UnitRoute {
            class: viewer_runtime::unit_class::CounterClass::Division,
            combat_role: byte(columns.combat_role),
            combat_count: number(columns.combat_count),
            combat_province: number(columns.combat_province),
            target: number(columns.target),
            naval_transfer: byte(columns.naval_transfer) != 0,
            path_total: number(columns.path_total),
            path: &path,
            alternate_total: number(columns.alternate_total),
            alternate_path: &alternate,
        };
        let ground = &*self.ground;
        let anchor = |province| ground.anchor(province, STANDSTILL);
        let kind = ground.world().and_then(|world| unit.stack_kind(province, world.map, &anchor));
        let display = unit.display_path();
        let signature = kind.is_some().then(|| display.first().map(|&next| [next, display.get(1).copied().unwrap_or(0)])).flatten();
        let pick = PickRoute {
            kind,
            hop: if unit.path_total == 0 { Some(0) } else { path.first().copied() },
            unrouted: unit.naval_transfer && unit.path_total == 0,
        };
        (self.path, self.alternate) = (path, alternate);
        (signature, Some(pick))
    }

    fn route(&mut self, mirrors: &Mirrors, id: u64, province: u32) -> Option<DivisionRoute> {
        let columns = self.columns();
        let movement = rows(mirrors.get(TableId::DivisionMovement))?;
        let slot = movement.slot(u128::from(id))?;
        let number = |column: Num<u32>| movement.number(column, slot).value().unwrap_or(0);
        let byte = |column: Num<u8>| movement.number(column, slot).value().unwrap_or(0);
        let (mut path, mut alternate) = (std::mem::take(&mut self.path), std::mem::take(&mut self.alternate));
        path_of(movement, columns.path, slot, &mut path);
        path_of(movement, columns.alternate_path, slot, &mut alternate);
        let unit = UnitRoute {
            class: viewer_runtime::unit_class::CounterClass::Division,
            combat_role: byte(columns.combat_role),
            combat_count: number(columns.combat_count),
            combat_province: number(columns.combat_province),
            target: number(columns.target),
            naval_transfer: byte(columns.naval_transfer) != 0,
            path_total: number(columns.path_total),
            path: &path,
            alternate_total: number(columns.alternate_total),
            alternate_path: &alternate,
        };
        let ground = &*self.ground;
        let anchor = |province| ground.anchor(province, STANDSTILL);
        let placement = unit.resolve(province, ground.world().map(|world| world.map), &anchor);
        let route = DivisionRoute::of_placement(&unit, byte(columns.retreat) != 0, None, placement);
        (self.path, self.alternate) = (path, alternate);
        Some(route)
    }

    fn place_division(&mut self, slot: u32, mirrors: &Mirrors, seer: Seer<'_>) {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.placed += 1;
        }
        let columns = self.columns();
        let at = slot as usize;
        if self.members.len() <= at {
            self.members.resize(at + 1, Member::default());
        }
        let divisions = rows(mirrors.get(TableId::Divisions));
        let row = divisions.filter(|divisions| divisions.live(slot)).and_then(|divisions| {
            let id = divisions.key(slot)? as u64;
            let province = divisions.number(columns.province, slot).value().unwrap_or(0);
            let owner = divisions.number(columns.owner, slot).value().unwrap_or(0);
            let roster = divisions.number(columns.roster, slot).value();
            Some((id, province, owner, roster))
        });
        let key = match row {
            Some((id, province, owner, roster)) if province != 0 => match self.links.get(&owner).copied() {
                Some(owner_slot) => {
                    let (signature, route) = self.signature(mirrors, id, province);
                    let roster = roster.and_then(|raw| self.links.get(&raw).copied());
                    (self.members[at].pick, self.members[at].roster) = (route, roster);
                    let member = &self.members[at];
                    if member.id != id || member.sighted != Some((province, owner_slot)) || member.read_at == NONE {
                        let (seen, read_at) = seer.sees(id, owner_slot, province);
                        let member = &mut self.members[at];
                        (member.id, member.sighted, member.seen) = (id, Some((province, owner_slot)), seen);
                        self.index(slot, read_at);
                    }
                    Some((province, owner_slot, signature))
                }
                None => None,
            },
            _ => None,
        };
        if key.is_none() {
            (self.members[at].pick, self.members[at].roster) = (None, None);
            self.index(slot, NONE);
        }
        self.members[at].stale = true;
        let held = self.members[at].stack;
        if held != NONE && Some(self.stacks[held as usize].key) == key {
            self.mark_stack(held);
            return;
        }
        if held != NONE {
            self.leave(slot);
            self.mark_stack(held);
        }
        if let Some(key) = key {
            let stack = match self.by_key.get(&key) {
                Some(&stack) => stack,
                None => {
                    let fresh = Stack { key, members: Vec::new(), live: true, admitted: self.admits(key.0), figure: None, asked: None };
                    let stack = match self.free.pop() {
                        Some(stack) => {
                            self.stacks[stack as usize] = fresh;
                            stack
                        }
                        None => {
                            self.stacks.push(fresh);
                            self.stacks.len() as u32 - 1
                        }
                    };
                    self.by_key.insert(key, stack);
                    stack
                }
            };
            let members = &mut self.stacks[stack as usize].members;
            self.members[at].stack = stack;
            self.members[at].at = members.len() as u32;
            members.push(slot);
            self.mark_stack(stack);
        }
    }

    fn leave(&mut self, slot: u32) {
        let member = &mut self.members[slot as usize];
        let (stack, at) = (member.stack, member.at as usize);
        member.stack = NONE;
        let members = &mut self.stacks[stack as usize].members;
        members.swap_remove(at);
        if let Some(&moved) = members.get(at) {
            self.members[moved as usize].at = at as u32;
        }
    }

    fn place_stack(&mut self, stack: u32, mirrors: &Mirrors) {
        let columns = self.columns();
        let at = stack as usize;
        if at >= self.stacks.len() || !self.stacks[at].live {
            return;
        }
        if self.stacks[at].members.is_empty() {
            let key = self.stacks[at].key;
            self.by_key.remove(&key);
            self.stacks[at].live = false;
            self.stacks[at].admitted = false;
            self.stacks[at].figure = None;
            if let Some(oldest) = self.stacks[at].asked.take() {
                self.gone(oldest, stack);
            }
            self.free.push(stack);
            return;
        }
        let Some(divisions) = mirrors.get(TableId::Divisions) else { return };
        let (province, owner, _) = self.stacks[at].key;
        let held = &self.members;
        let members = self.stacks[at].members.iter().map(|&slot| (held[slot as usize].id, slot));
        let Some((oldest, slot)) = members.clone().filter(|&(_, slot)| held[slot as usize].seen).min() else {
            let Some((oldest, slot)) = members.min() else { return };
            let number = |column: Num<u32>| divisions.number(column, slot).value().unwrap_or(0);
            let selection = Selection {
                transport: false,
                template: divisions.number(columns.template, slot).value().unwrap_or(0),
                oldest,
                roster_owner: number(columns.roster),
                expeditionary_owner: number(columns.expeditionary),
                status: false,
            };
            let placing = Placing { province, owner, oldest, route: None, combat: None, training: None, uv_moving: false, scale: 0.0 };
            let admitted = self.stacks[at].admitted;
            self.stacks[at].figure = Some(Figure { selection, admitted, placed: Some(Err(Withheld::Unseen)), last: None, placing });
            self.touched.push(stack);
            return;
        };
        let unchanged = self.stacks[at].figure.as_ref().is_some_and(|held| held.selection.oldest == oldest && !matches!(held.placed, Some(Err(Withheld::Unseen))));
        if unchanged && !self.members[slot as usize].stale {
            return;
        }
        self.members[slot as usize].stale = false;
        let route = self.route(mirrors, oldest, province);
        let route = route.as_ref();
        let alternate = rows(mirrors.get(TableId::ArmyIconInputs))
            .and_then(|icons| icons.slot(u128::from(oldest)).and_then(|row| icons.number(columns.alternate_template, row).value()))
            .filter(|&template| template != 0);
        let number = |column: Num<u32>| divisions.number(column, slot).value().unwrap_or(0);
        let transport = route.is_some_and(|route| route.naval_transfer);
        let status = rows(mirrors.get(TableId::DivisionStatus)).and_then(|statuses| statuses.slot(u128::from(oldest)).map(|row| statuses.number(columns.exercising, row)));
        let movement = rows(mirrors.get(TableId::DivisionMovement)).and_then(|movement| movement.slot(u128::from(oldest)).map(|row| (movement, row)));
        let uv_moving = movement.is_some_and(|(movement, row)| {
            movement.number(columns.path_total, row).value().unwrap_or(0) > 0 && movement.number(columns.target, row).value().unwrap_or(0) == 0
        });
        let selection = Selection {
            transport,
            template: alternate.unwrap_or_else(|| divisions.number(columns.template, slot).value().unwrap_or(0)),
            oldest,
            roster_owner: number(columns.roster),
            expeditionary_owner: number(columns.expeditionary),
            status: status.is_some(),
        };
        let scalars = Scalars {
            province,
            owner,
            oldest,
            combat: self.combat.get(&oldest).copied(),
            training: status.and_then(Cell::value).map(|exercising| exercising != 0),
            uv_moving,
            scale: if transport { self.ground.transport_scale() } else { 1.0 },
        };
        let admitted = self.stacks[at].admitted;
        let kept = self.stacks[at].figure.as_ref().is_some_and(|held| matches!(held.placed, Some(Ok(_)) | Some(Err(Withheld::Unplaced(_)))) && held.placing.same(&scalars, route));
        let placing = (!kept).then(|| scalars.with(route.cloned()));
        let held = self.stacks[at].figure.take();
        let figure = self.refigured(stack, held, (selection, admitted, transport), placing);
        self.stacks[at].figure = Some(figure);
        self.touched.push(stack);
    }

    fn refigured(&mut self, stack: u32, held: Option<Figure>, (selection, admitted, transport): (Selection, bool, bool), placing: Option<Placing>) -> Figure {
        match (held, placing) {
            (Some(held), None) => Figure { selection, admitted, ..held },
            (held, placing) => {
                let placing = placing.expect("a placing not kept is made");
                let last = held.as_ref().and_then(Figure::drawn).cloned();
                self.placed_anew(stack, selection, admitted, transport, placing, last)
            }
        }
    }

    fn record_view(&self) {
        use viewer_runtime::view_record::{add, set};
        set("units", "stacks_admitted", self.asking.len() as u64);
        let figures = || self.by_key.values().filter_map(|&stack| self.stacks.get(stack as usize)).filter(|held| held.admitted).filter_map(|held| held.figure.as_ref());
        set("units", "figures_unseen", figures().filter(|figure| matches!(figure.placed, Some(Err(Withheld::Unseen)))).count() as u64);
        set("units", "figures_placing", figures().filter(|figure| figure.placed.is_none()).count() as u64);
        let Some(meshes) = &self.meshes else { return };
        let unready = self
            .asking
            .iter()
            .filter(|&(oldest, &(stack, _))| {
                !meshes.meshes.contains_key(oldest)
                    && !meshes.withheld.contains_key(oldest)
                    && self.stacks.get(stack as usize).is_some_and(|held| held.admitted && held.figure.as_ref().is_some_and(|figure| matches!(figure.placed, Some(Ok(_)))))
            })
            .count();
        add("units", "entered_unready", unready as u64);
        meshes.record_view();
    }

    fn ask_meshes(&mut self) {
        let mut touched = std::mem::take(&mut self.touched);
        touched.sort_unstable();
        touched.dedup();
        for &stack in &touched {
            let at = stack as usize;
            if at >= self.stacks.len() || !self.stacks[at].live {
                continue;
            }
            let held = &self.stacks[at];
            let bounded = self.ground.bounds().contains_key(&held.key.0);
            if bounded && held.admitted && held.asked.is_some() && held.figure.as_ref().is_some_and(|figure| figure.placed.is_none()) {
                continue;
            }
            let wants = held.figure.as_ref().filter(|figure| bounded && held.admitted && matches!(figure.placed, Some(Ok(_)))).map(|figure| {
                let Selection { transport, template, oldest, roster_owner, expeditionary_owner, .. } = figure.selection;
                (oldest, (figure.placing.province, unit_models::SelectionKey { transport, template, oldest, roster_owner, expeditionary_owner }))
            });
            let asked = std::mem::replace(&mut self.stacks[at].asked, wants.map(|(oldest, _)| oldest));
            if let Some(oldest) = asked.filter(|&oldest| wants.is_none_or(|(wanted, _)| wanted != oldest)) {
                self.gone(oldest, stack);
            }
            if let Some((oldest, asked)) = wants
                && self.asking.insert(oldest, (stack, asked)).is_none_or(|(_, before)| before != asked)
            {
                self.asks.push(meshes::Ask::Stack(oldest, asked));
            }
        }
        touched.clear();
        self.touched = touched;
    }

    fn gone(&mut self, oldest: u64, stack: u32) {
        if self.asking.get(&oldest).is_some_and(|&(held, _)| held == stack) {
            self.asking.remove(&oldest);
            self.asks.push(meshes::Ask::Gone(oldest));
        }
    }

    fn placed_anew(&mut self, stack: u32, selection: Selection, admitted: bool, transport: bool, placing: Placing, last: Option<(Target, Position)>) -> Figure {
        match () {
            () if !transport && !selection.status => Figure { selection, admitted, placed: Some(Err(Withheld::Status)), last: None, placing },
            () => {
                if admitted {
                    self.figures.mark(stack, self.frame);
                }
                Figure { selection, admitted, placed: None, last, placing }
            }
        }
    }

    fn place_figure(&mut self, stack: u32) {
        let at = stack as usize;
        let live = at < self.stacks.len() && self.stacks[at].live && self.stacks[at].admitted;
        let Some(figure) = self.stacks.get(at).filter(|_| live).and_then(|held| held.figure.as_ref()).filter(|figure| figure.placed.is_none()) else { return };
        let placed = self.figure(&figure.placing).map_err(Withheld::Unplaced);
        self.marked[2] += 1;
        if let Some(figure) = &mut self.stacks[at].figure {
            (figure.placed, figure.last) = (Some(placed), None);
        }
        self.touched.push(stack);
    }

    fn figure(&self, placing: &Placing) -> Result<(Target, Position), Unplaced> {
        let ground = &*self.ground;
        let world = ground.world();
        let anchor = |province, slot| ground.anchor(province, slot);
        let stack = LandStack {
            province: placing.province,
            owner: placing.owner,
            oldest: placing.oldest,
            route: placing.route.as_ref(),
            combat: placing.combat,
            training: placing.training,
            uv_moving: placing.uv_moving,
            scale: placing.scale,
        };
        let (target, position) = unit_models::land_figure(&stack, world.as_ref(), &anchor)?;
        Ok((unit_models::drawn(target, &position), position))
    }

    fn settle_dirty(&mut self, mirrors: &Mirrors, layer: Option<&VisionLayer>) {
        self.settle_marks(mirrors, layer);
        self.frame += 1;
    }

    fn settle_marks(&mut self, mirrors: &Mirrors, layer: Option<&VisionLayer>) {
        if !self.showing || self.columns.is_none() || TABLES.iter().any(|&table| mirrors.get(table).is_none()) {
            return;
        }
        let mut renewed = false;
        if !self.reading && self.fresh_vision.swap(false, Ordering::AcqRel) {
            let given = self.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
            renewed = !matches!((&self.vision, &given), (Some((_, held)), Some((_, new))) if Arc::ptr_eq(held, new));
            self.vision = given;
        }
        if let Err(why) = self.check(mirrors, layer) {
            if self.refused.as_ref() != Some(&why) {
                self.mark_all(mirrors);
                self.divisions.clear();
                self.refused = Some(why);
            }
            self.sights.clear();
            return;
        }
        let given = self.vision.as_ref().map(|(_, vision)| Arc::clone(vision));
        let seer = if self.reading { layer.map(Seer::Layer) } else { given.as_deref().map(Seer::Given) };
        let Some(seer) = seer else { return };
        if self.refused.take().is_some() {
            self.mark_all(mirrors);
        } else if renewed {
            self.mark_sights();
        }
        if self.camera_dirty {
            self.readmit(mirrors);
        }
        if self.links_dirty {
            self.links_dirty = false;
            self.read_links(mirrors);
        }
        if self.combat_dirty {
            self.combat_dirty = false;
            self.read_combat(mirrors);
        }
        self.resolve(mirrors);
        let mut dirty = std::mem::take(&mut self.divisions);
        let _ = dirty.settle(|slot| {
            self.place_division(slot, mirrors, seer);
            Ok(())
        });
        self.divisions = dirty;
        let mut dirty = std::mem::take(&mut self.sights);
        let _ = dirty.settle(|slot| {
            self.resight(slot, seer);
            Ok(())
        });
        self.sights = dirty;
        let mut dirty = std::mem::take(&mut self.dirty_stacks);
        let _ = dirty.settle(|stack| {
            self.place_stack(stack, mirrors);
            Ok(())
        });
        self.dirty_stacks = dirty;
        let mut dirty = std::mem::take(&mut self.figures);
        let _ = dirty.settle(|stack| {
            self.place_figure(stack);
            Ok(())
        });
        self.figures = dirty;
    }

    fn compose_from(&mut self, mirrors: &Mirrors, layer: Option<&VisionLayer>) {
        if self.columns.is_none() {
            return;
        }
        let (marked, sighted) = (self.marked, self.sighted);
        self.refused = Some(if self.showing { "composing".into() } else { NOT_DRAWN.to_owned() });
        if let Some(models) = &mut self.models {
            models.composed();
        }
        self.settle_dirty(mirrors, layer);
        (self.marked, self.sighted) = (marked, sighted);
    }

    fn mark_every(&mut self, divisions: Option<&Mirror>) {
        self.deferred.clear();
        self.deferred_count = 0;
        let mut all: Vec<u32> = divisions.map(|divisions| divisions.rows().collect()).unwrap_or_default();
        all.extend((0..self.members.len() as u32).filter(|&slot| self.members[slot as usize].stack != NONE));
        all.into_iter().for_each(|slot| self.mark_division(slot));
    }

    fn resolve(&mut self, mirrors: &Mirrors) {
        let divisions = mirrors.get(TableId::Divisions);
        if std::mem::take(&mut self.every) {
            self.mark_every(divisions);
            self.moved.clear();
        }
        for (table, route) in [(TableId::DivisionMovement, true), (TableId::ArmyIconInputs, false), (TableId::DivisionStatus, false)] {
            let mirror = mirrors.get(table);
            let held = match table {
                TableId::DivisionMovement => &mut self.route_rows,
                TableId::ArmyIconInputs => &mut self.icon_rows,
                _ => &mut self.status_rows,
            };
            let mut rows = std::mem::take(held);
            rows.settle(|row| {
                let key = match row {
                    Row::Gone(key) => Some(key),
                    Row::At(slot) => mirror.and_then(|mirror| mirror.key(slot)),
                };
                match key.and_then(|key| divisions?.slot(key)) {
                    Some(slot) if route => self.mark_moved(slot, divisions),
                    Some(slot) => self.mark_stack_of(slot),
                    None => {}
                }
            });
            match table {
                TableId::DivisionMovement => self.route_rows = rows,
                TableId::ArmyIconInputs => self.icon_rows = rows,
                _ => self.status_rows = rows,
            }
        }
        let mut moved = std::mem::take(&mut self.moved);
        let _ = moved.settle(|slot| {
            if self.admission.is_none() || self.wanted(slot, divisions) {
                self.divisions.mark(slot, self.frame);
            } else {
                self.defer(slot);
            }
            Ok(())
        });
        self.moved = moved;
    }

    fn rows_carried(&self) -> usize {
        self.moved.len() + self.route_rows.len() + self.icon_rows.len() + self.status_rows.len() + usize::from(self.every)
    }

    fn land(&self, meshes: &HashMap<u64, usize>) -> Vec<Target> {
        self.stacks
            .iter()
            .filter(|stack| stack.admitted)
            .filter_map(|stack| stack.figure.as_ref()?.drawn())
            .filter_map(|(target, _)| meshes.get(&target.identity).map(|&mesh| Target { mesh, ..*target }))
            .collect()
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        let mut differing = 0;
        let mut first = None;
        let mut note = |what: String| {
            differing += 1;
            first.get_or_insert(what);
        };
        if self.refused() != other.refused() {
            note(format!("refused: {:?} against {:?}", self.refused(), other.refused()));
        }
        let admitted_only = self.admission.is_some() || other.admission.is_some();
        let (mut mine, mut theirs) = (self.figures(), other.figures());
        if admitted_only {
            mine.retain(|_, figure| figure.admitted);
            theirs.retain(|_, figure| figure.admitted);
        }
        if !admitted_only && self.stack_count() != other.stack_count() {
            note(format!("{} stacks against {}", self.stack_count(), other.stack_count()));
        }
        for (key, figure) in &mine {
            match theirs.get(key) {
                None => note(format!("stack {key:?} is not composed")),
                Some(other) => {
                    if figure.selection != other.selection {
                        note(format!("stack {key:?}: selection {:?} against {:?}", figure.selection, other.selection));
                    }
                    if figure.admitted != other.admitted {
                        note(format!("stack {key:?}: admitted {} against {}", figure.admitted, other.admitted));
                    }
                    let placed = |figure: &Figure| figure.placed.as_ref().map(|placed| placed.as_ref().map(|(target, _)| *target).map_err(|why| *why));
                    if figure.admitted && placed(figure) != placed(other) {
                        note(format!("stack {key:?}: figure {:?} against {:?}", placed(figure), placed(other)));
                    }
                }
            }
        }
        for key in theirs.keys().filter(|key| !mine.contains_key(key)) {
            note(format!("composed stack {key:?} is not held"));
        }
        (differing, first)
    }

    fn place(&mut self, view: &View) {
        let camera = view.camera.map(|camera| (camera, view.viewport));
        if camera != self.camera {
            self.camera = camera;
            self.camera_dirty = true;
        }
        let showing = view.camera.is_none_or(|camera| camera.height < self.ground.units_cutoff()) && visibility::of(&view.map_mode).land_models;
        let movers = view.camera.is_none_or(|camera| camera.height < self.ground.ships_cutoff().max(self.ground.units_cutoff()));
        if showing {
            self.showing = true;
            self.start_models();
            if let Some(window) = &self.window
                && let Some(views) = &window.views
                && self.meshes.is_none()
            {
                self.meshes = Some(meshes::Meshes::start(Arc::clone(&window.select), views, &window.meshes));
            }
            return;
        }
        self.showing = false;
        self.refused = Some(NOT_DRAWN.to_owned());
        self.members = Vec::new();
        self.stacks = Vec::new();
        self.free = Vec::new();
        self.by_key = FastMap::default();
        self.combat = FastMap::default();
        self.combat_scratch = FastMap::default();
        self.combat_ordered = Vec::new();
        self.combat_changed = Vec::new();
        self.links = FastMap::default();
        self.divisions = Dirty::default();
        self.moved = Dirty::default();
        (self.route_rows, self.icon_rows, self.status_rows, self.every) = (Rows::default(), Rows::default(), Rows::default(), false);
        self.staged = None;
        if movers && self.models.is_some() {
            self.to_upload |= !self.uploaded.is_empty();
        } else {
            self.uploaded = Vec::new();
        }
        if let (Some(window), Some(meshes)) = (&mut self.window, &self.meshes) {
            window.meshes = meshes.tables();
        }
        (self.meshes, self.asking, self.touched, self.asks) = (None, FastMap::default(), Vec::new(), Vec::new());
        self.dirty_stacks = Dirty::default();
        self.figures = Dirty::default();
        self.sights = Dirty::default();
        self.readers = FastMap::default();
        self.deferred = Vec::new();
        self.deferred_count = 0;
        (self.province_known, self.province_in, self.statuses) =
            (ProvinceSet::default(), ProvinceSet::default(), HashMap::new());
        (self.choice, self.sight_epoch) = (None, None);
        self.admission = None;
        if movers {
            self.start_models();
        } else if let (Some(window), Some(models)) = (&mut self.window, self.models.take()) {
            window.tables = models.tables();
            self.to_upload = false;
        }
    }

    fn start_models(&mut self) {
        if let Some(window) = &self.window
            && let Some(views) = &window.views
            && self.models.is_none()
        {
            self.models = Some(models::Models::new(Arc::clone(&window.select), views, &window.tables));
        }
    }

    fn mark_land(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) {
        let layer = upstream.get::<VisionLayer>().filter(|_| self.reading);
        if let Some(vision) = layer {
            self.take_upstream(vision, upstream, mirrors);
        }
        let before = self.marked;
        self.settle_dirty(mirrors, layer);
        self.to_upload |= self.marked != before;
        if self.meshes.is_some() {
            self.ask_meshes();
            self.hand_meshes(upstream);
        } else {
            self.touched.clear();
        }
    }

    fn hand_meshes(&mut self, upstream: &Upstream<'_>) {
        let layer = upstream.get::<VisionLayer>().filter(|_| self.reading);
        let ours = layer.and_then(VisionLayer::ours).or_else(|| self.vision.as_ref().map(|(_, vision)| vision.ours()));
        if let (Some(meshes), Some(ours), None) = (&mut self.meshes, ours, &self.refused) {
            let asks = std::mem::take(&mut self.asks);
            meshes.ask(ours, asks);
        }
    }

    fn join_meshes(&mut self, mirrors: &Mirrors, settling: Settling) {
        let hour = mirrors.get(TableId::Divisions).and_then(Mirror::head);
        if let (Some(meshes), Some(hour), None) = (&mut self.meshes, hour, &self.refused) {
            self.to_upload |= meshes.join(hour, settling, &self.retiring);
            viewer_layer_core::playhead_host::timeline::mark(meshes.maker(), viewer_layer_core::playhead_host::timeline::Mark::Joined);
        }
        if viewer_runtime::view_record::on() {
            self.record_view();
        }
    }

    fn ask_models(&mut self, upstream: &Upstream<'_>) {
        let convoys = upstream.get::<crate::convoys::Convoys>().filter(|convoys| convoys.refused().is_none()).map(|convoys| (convoys.product_seq(), &convoys.product().convoys[..]));
        let ships = upstream.get::<crate::ships::Ships>();
        let layered = upstream.get::<VisionLayer>().filter(|_| self.reading && !self.showing).map(VisionLayer::choice);
        let choice = layered.or(self.vision.as_ref().map(|(choice, _)| choice)).or(self.choice.as_ref());
        if let (Some(models), Some(choice)) = (&mut self.models, choice) {
            models.ask(choice, convoys, !ships.is_some_and(crate::ships::Ships::stands_in));
        }
    }

    fn join_models(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) {
        let ships = upstream.get::<crate::ships::Ships>();
        if let Some(models) = &mut self.models {
            let head = mirrors.get(TableId::Divisions).and_then(Mirror::head).or_else(|| ships.and_then(crate::ships::Ships::head));
            let arrived = models.join(head, settling, &self.retiring);
            let joined = models.join_ships(ships);
            self.models_new |= arrived | joined;
            self.to_upload |= arrived | joined;
            viewer_layer_core::playhead_host::timeline::mark(models.maker(), viewer_layer_core::playhead_host::timeline::Mark::Joined);
        }
    }
}

impl Layer for Units {
    fn retirement(&self) -> Option<Retirement> {
        self.window.as_ref().map(|_| self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        "units"
    }

    fn placed(&mut self, view: &View) -> Option<u64> {
        self.place(view);
        Some(0)
    }

    fn views(&mut self, store: &viewer_layer_core::playhead_host::views::Store) -> io::Result<()> {
        if let Some(window) = &mut self.window {
            store.use_columns(TableId::VariantDefinitions);
            window.views = Some(store.handle());
        }
        Ok(())
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if self.reading {
            inputs.after("vision");
        }
        self.place(view);
        for table in [TableId::Divisions, TableId::DivisionMovement, TableId::ArmyIconInputs, TableId::DivisionStatus, TableId::LandCombats, TableId::CountryLinks] {
            inputs.table(table, true)?;
        }
        self.columns = Some(Columns {
            province: inputs.number(TableId::Divisions, "province")?,
            owner: inputs.number(TableId::Divisions, "logical_owner")?,
            template: inputs.number(TableId::Divisions, "template")?,
            roster: inputs.number(TableId::Divisions, "roster_owner")?,
            expeditionary: inputs.number(TableId::Divisions, "expeditionary_owner")?,
            target: inputs.number(TableId::DivisionMovement, "target")?,
            retreat: inputs.number(TableId::DivisionMovement, "retreat")?,
            naval_transfer: inputs.number(TableId::DivisionMovement, "naval_transfer")?,
            combat_count: inputs.number(TableId::DivisionMovement, "combat_count")?,
            path_total: inputs.number(TableId::DivisionMovement, "path_total")?,
            path: inputs.list(TableId::DivisionMovement, "path")?,
            alternate_total: inputs.number(TableId::DivisionMovement, "alternate_total")?,
            alternate_path: inputs.list(TableId::DivisionMovement, "alternate_path")?,
            combat_role: inputs.number(TableId::DivisionMovement, "combat_role")?,
            combat_province: inputs.number(TableId::DivisionMovement, "combat_province")?,
            alternate_template: inputs.number(TableId::ArmyIconInputs, "alternate_template")?,
            exercising: inputs.number(TableId::DivisionStatus, "exercising")?,
            attackers: inputs.list(TableId::LandCombats, "attackers")?,
            defenders: inputs.list(TableId::LandCombats, "defenders")?,
            link_slot: inputs.number(TableId::CountryLinks, "canonical_slot")?,
        });
        Ok(())
    }

    fn viewed(&mut self, _view: &View, mirrors: &Mirrors) {
        if self.showing && self.camera_dirty && self.refused.is_none() {
            self.readmit(mirrors);
        }
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.compose_from(mirrors, None);
        Ok(())
    }

    fn compose_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<()> {
        let layer = upstream.get::<VisionLayer>().filter(|_| self.reading && self.showing);
        if let Some(vision) = layer {
            (self.choice, self.sight_epoch) = (Some(vision.choice().clone()), Some(vision.sight_epoch()));
        }
        self.compose_from(mirrors, layer);
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        if step.gap() || !self.showing {
            return;
        }
        #[cfg(any(test, feature = "test-support"))]
        let reads = viewer_layer_core::playhead_host::mirror::probe::reads();
        let columns = self.columns();
        let frame = self.frame;
        for changes in step.tables() {
            let table = TableId::ALL[changes.table()];
            if !TABLES.contains(&table) {
                continue;
            }
            if changes.status().is_some_and(|(status, _)| self.statuses.insert(changes.table(), status) != Some(status)) {
                match table {
                    TableId::CountryLinks => self.links_dirty = true,
                    TableId::LandCombats => self.combat_dirty = true,
                    _ => {}
                }
                self.every = true;
                continue;
            }
            match table {
                TableId::Divisions => {
                    let keyed = [columns.province.column, columns.owner.column, columns.roster.column];
                    let selected = [columns.template.column, columns.expeditionary.column];
                    let mut moved = |slot: u32| {
                        if self.moved.mark(slot, frame) {
                            self.marked[0] += 1;
                        }
                    };
                    for &slot in changes.deleted().slots().iter().chain(changes.inserted().slots()) {
                        moved(slot);
                    }
                    for written in changes.columns().iter().filter(|written| keyed.contains(&written.column())) {
                        written.slots().iter().for_each(|&slot| moved(slot));
                    }
                    for written in changes.columns().iter().filter(|written| selected.contains(&written.column())) {
                        written.slots().iter().for_each(|&slot| self.mark_stack_of(slot));
                    }
                }
                TableId::DivisionMovement => {
                    let read = [
                        columns.target.column,
                        columns.retreat.column,
                        columns.naval_transfer.column,
                        columns.combat_count.column,
                        columns.path_total.column,
                        columns.path.column,
                        columns.alternate_total.column,
                        columns.alternate_path.column,
                        columns.combat_role.column,
                        columns.combat_province.column,
                    ];
                    self.route_rows.take(changes, frame, |column| read.contains(&column));
                }
                TableId::ArmyIconInputs => self.icon_rows.take(changes, frame, |column| column == columns.alternate_template.column),
                TableId::DivisionStatus => self.status_rows.take(changes, frame, |column| column == columns.exercising.column),
                TableId::LandCombats => {
                    let read = [columns.attackers.column, columns.defenders.column];
                    if !changes.deleted().is_empty() || !changes.inserted().is_empty() || changes.columns().iter().any(|written| !written.is_empty() && read.contains(&written.column())) {
                        self.combat_dirty = true;
                    }
                }
                TableId::CountryLinks => {
                    if !changes.deleted().is_empty() || !changes.inserted().is_empty() || changes.columns().iter().any(|written| !written.is_empty() && written.column() == columns.link_slot.column) {
                        self.links_dirty = true;
                        self.every = true;
                    }
                }
                _ => {}
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.applied_reads += viewer_layer_core::playhead_host::mirror::probe::reads() - reads;
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        self.settle_dirty(mirrors, None);
        Ok(())
    }

    fn ask(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<bool> {
        if self.showing {
            self.mark_land(mirrors, upstream);
        }
        self.ask_models(upstream);
        Ok(true)
    }

    fn join(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        if self.showing {
            self.join_meshes(mirrors, settling);
        }
        self.join_models(mirrors, upstream, settling);
        if self.retiring.requested() {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    }

    fn settle_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        <Self as Layer>::ask(self, mirrors, upstream)?;
        self.join(mirrors, upstream, settling)
    }

    fn stage(&mut self) -> io::Result<()> {
        let Some(models) = &self.models else { return Ok(()) };
        let Some((_, prepared)) = models.prepared.as_ref().filter(|_| self.to_upload) else { return Ok(()) };
        let none = HashMap::new();
        let land = if models.refused.is_some() { Vec::new() } else { self.land(self.meshes.as_ref().map_or(&none, |meshes| &meshes.meshes)) };
        if !self.models_new && land == self.uploaded {
            (self.to_upload, self.staged) = (false, None);
            return Ok(());
        }
        let Some(assets) = self.ground.unit_models() else {
            self.to_upload = false;
            return Ok(());
        };
        let field = prepared.field_with_land(assets, land.clone());
        self.staged = Some((field, land));
        Ok(())
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        let Some((field, land)) = self.staged.take() else { return Ok(()) };
        gpu.scene.set_unit_models(gpu.device, &gpu.content.unit_models, field, false)?;
        (self.to_upload, self.models_new, self.uploaded) = (false, false, land);
        Ok(())
    }

    fn uploaded(&self) -> bool {
        !self.to_upload || self.models.as_ref().is_none_or(|models| models.prepared.is_none())
    }

    fn published(&self) -> Published {
        let Some(models) = self.models.as_ref() else { return Published::Current };
        let meshes = self.meshes.as_ref();
        match &models.prepared {
            None if models.waiting() => Published::Nothing,
            _ if models.refused.is_some() && self.uploaded() => Published::Current,
            _ if self.refused.is_none() && meshes.is_some_and(meshes::Meshes::waiting) => Published::Nothing,
            Some((hour, _)) if self.uploaded() && self.carried().entities == 0 => {
                let models = models.stands().unwrap_or(*hour);
                Published::Cadence(meshes.and_then(|meshes| meshes.hour).map_or(models, |meshes| meshes.min(models)))
            }
            _ => Published::Current,
        }
    }

    fn made_at(&self, hour: replay_data::Hour) -> bool {
        self.window.is_none() || self.figures_stand(hour)
    }

    fn carried(&self) -> Carried {
        let entities = self.divisions.len() + self.rows_carried() + self.sights.len() + self.dirty_stacks.len() + self.figures.len() + usize::from(self.combat_dirty) + usize::from(self.links_dirty);
        Carried { entities }
    }

    fn held(&self) -> usize {
        let stacks: usize = self.stacks.iter().map(|stack| stack.members.capacity() * 4 + stack.figure.as_ref().and_then(|figure| figure.placing.route.as_ref()).map_or(0, |route| route.path.capacity() * 4)).sum();
        self.members.capacity() * std::mem::size_of::<Member>()
            + self.stacks.capacity() * std::mem::size_of::<Stack>()
            + stacks
            + self.free.capacity() * 4
            + self.by_key.capacity() * (std::mem::size_of::<StackKey>() + 4 + 8)
            + self.combat.capacity() * 20
            + self.combat_scratch.capacity() * 20
            + self.combat_ordered.capacity() * std::mem::size_of::<(u128, u32)>()
            + self.combat_changed.capacity() * 8
            + self.links.capacity() * 12
            + self.divisions.bytes()
            + self.moved.bytes()
            + self.route_rows.bytes()
            + self.icon_rows.bytes()
            + self.status_rows.bytes()
            + self.uploaded.capacity() * std::mem::size_of::<Target>()
            + self.sights.bytes()
            + self.readers.capacity() * (4 + std::mem::size_of::<Vec<u32>>() + 8)
            + self.readers.values().map(|readers| readers.capacity() * 4).sum::<usize>()
            + self.dirty_stacks.bytes()
            + self.figures.bytes()
            + self.meshes.as_ref().map_or(0, meshes::Meshes::bytes)
            + self.asking.capacity() * (8 + 4 + std::mem::size_of::<meshes::Asked>() + 8)
            + self.touched.capacity() * 4
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    fn any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Units {
    pub fn take_mesh_counts(&self) -> Option<meshes::Counts> {
        self.meshes.as_ref().map(meshes::Meshes::take_counts)
    }

    pub fn take_model_counts(&self) -> Option<models::Counts> {
        self.models.as_ref().map(models::Models::take_counts)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn meshes(&self) -> Option<&HashMap<u64, usize>> {
        self.meshes.as_ref().map(|meshes| &meshes.meshes)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn mesh_hour(&self) -> Option<replay_data::Hour> {
        self.meshes.as_ref().and_then(|meshes| meshes.hour)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn mesh_state(&self) -> String {
        let asking: BTreeMap<u64, u32> = self.asking.iter().map(|(&oldest, &(stack, _))| (oldest, stack)).collect();
        format!("{}; asking {asking:?}", self.meshes.as_ref().map_or_else(|| "no meshes".to_owned(), meshes::Meshes::state))
    }
}

impl Units {
    pub fn figures_stand(&self, hour: replay_data::Hour) -> bool {
        if let Some(models) = &self.models
            && (!models.made_at(hour) || (models.refused.is_none() && models.waiting()))
        {
            return false;
        }
        if self.refused.is_some() || !self.showing {
            return true;
        }
        self.carried().entities == 0
            && (self.models.as_ref().is_none_or(|models| models.refused.is_some())
                || self.meshes.as_ref().is_none_or(|meshes| {
                    !meshes.waiting() && meshes.stand() && meshes.hour.is_none_or(|made| made >= hour)
                }))
    }

    pub fn figures_state(&self) -> String {
        let models = self.models.as_ref();
        let meshes = self.meshes.as_ref();
        format!(
            "carried {}, models {:?} standing {:?} waiting {:?}, meshes {:?} waiting {:?} standing {:?}",
            self.carried().entities,
            models.and_then(|models| models.prepared.as_ref().map(|(made, _)| *made)),
            models.and_then(models::Models::stands),
            models.map(models::Models::waiting),
            meshes.and_then(|meshes| meshes.hour),
            meshes.map(meshes::Meshes::waiting),
            meshes.map(meshes::Meshes::stand),
        )
    }
}

pub fn of(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Option<&Units> {
    let (_, layers) = side.parts()?;
    layers.iter().find_map(|layer| layer.any().downcast_ref::<Units>())
}

pub fn prepared(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> Option<&viewer_runtime::landed::Landed<Arc<viewer_unit_models::unit_models::PreparedModels>>> {
    of(side).and_then(Units::prepared)
}

#[cfg(test)]
mod tests;
