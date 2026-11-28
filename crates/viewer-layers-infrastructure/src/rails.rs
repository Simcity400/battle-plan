
use viewer_layer_core::layers::{Carried, Settling, Changed, Dirty, Gpu, Inputs, Layer, Num, Number, View};
use viewer_layer_core::playhead_host::mirror::{Mirror, Mirrors};
use eframe::wgpu;
use playhead::{Step, changes::Changes};
use replay_columnar::manifest::Status;
use replay_data::rows::TableId;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io,
    ops::Range,
    sync::Arc,
};
use viewer_rail::railways::{Bridge, PreparedRailGeometry, RailNetwork, RailRun};

const VERTEX: u64 = std::mem::size_of::<[f32; 6]>() as u64;

trait Target {
    fn reserve(&mut self, vertices: u32, kept: u32) -> io::Result<()>;
    fn write(&mut self, at: u32, vertices: &[[f32; 6]]);
    fn publish(&mut self, runs: &[Range<u32>]) -> io::Result<()>;
    fn bridges(&mut self, bridges: &BTreeMap<(u32, u32), Bridge>) -> io::Result<()>;
}

#[derive(Debug, Default)]
struct Placed {
    provinces: Vec<u32>,
    runs: Vec<Range<u32>>,
    rooms: Vec<u32>,
    free: BTreeSet<(u32, u32)>,
    end: u32,
    capacity: u32,
}

impl Placed {
    fn place(&mut self, province: u32, len: u32, live: impl FnOnce() -> u32) -> (u32, Option<u32>) {
        let at = match self.provinces.binary_search(&province) {
            Ok(at) if self.rooms[at] >= len => {
                let start = self.runs[at].start;
                self.runs[at] = start..start + len;
                return (start, None);
            }
            Ok(at) => {
                self.free.insert((self.rooms[at], self.runs[at].start));
                self.provinces.remove(at);
                self.runs.remove(at);
                self.rooms.remove(at);
                at
            }
            Err(at) => at,
        };
        let (start, room, grow) = match self.free.range((len, 0)..).next().copied() {
            Some((room, start)) => {
                self.free.remove(&(room, start));
                if room - len >= 64 {
                    self.free.insert((room - len, start + len));
                    (start, len, None)
                } else {
                    (start, room, None)
                }
            }
            None => {
                let start = self.end;
                self.end += len;
                let grow = (self.end > self.capacity).then(|| {
                    let want = self.end.max(live());
                    self.capacity = want.saturating_add(want / 4).max(self.capacity.saturating_mul(2));
                    self.capacity
                });
                (start, len, grow)
            }
        };
        self.provinces.insert(at, province);
        self.runs.insert(at, start..start + len);
        self.rooms.insert(at, room);
        (start, grow)
    }

    fn remove(&mut self, province: u32) -> bool {
        let Ok(at) = self.provinces.binary_search(&province) else { return false };
        self.free.insert((self.rooms[at], self.runs[at].start));
        self.provinces.remove(at);
        self.runs.remove(at);
        self.rooms.remove(at);
        true
    }

    fn bytes(&self) -> usize {
        self.provinces.capacity() * 4 + self.runs.capacity() * 8 + self.rooms.capacity() * 4 + self.free.len() * 24
    }
}

const STAGED: usize = 16 * 1024;

static CHECKED_STRIPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Default)]
struct Strips {
    buffer: Option<wgpu::Buffer>,
    held: Vec<u32>,
    placed: Vec<Range<u32>>,
    sent: usize,
    capacity: u32,
}

impl Strips {
    fn laid(&mut self, runs: &[Range<u32>]) -> io::Result<(usize, Option<Range<usize>>)> {
        let length = runs.iter().map(|run| run.len() + 1).sum::<usize>();
        if self.held.len() < length {
            self.held.try_reserve(length - self.held.len()).map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            self.held.resize(length, u32::MAX);
        }
        let (mut first, mut last, sent) = (length, 0, self.sent);
        let (mut at, mut moved) = (0, false);
        for (index, run) in runs.iter().enumerate() {
            let laid = self.placed.get(index);
            if !moved && laid == Some(run) && at + run.len() + 1 <= sent {
                at += run.len() + 1;
                continue;
            }
            moved |= laid.is_none_or(|laid| laid.len() != run.len());
            for want in run.clone().chain(std::iter::once(u32::MAX)) {
                if at >= sent || self.held[at] != want {
                    self.held[at] = want;
                    (first, last) = (first.min(at), at);
                }
                at += 1;
            }
        }
        self.placed.clear();
        self.placed.try_reserve(runs.len().saturating_sub(self.placed.capacity())).map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        self.placed.extend_from_slice(runs);
        if viewer_runtime::install_check::due(&CHECKED_STRIPS) {
            let whole: Vec<u32> = runs.iter().flat_map(|run| run.clone().chain(std::iter::once(u32::MAX))).collect();
            if let Some(at) = (0..length).find(|&at| self.held[at] != whole[at]) {
                viewer_runtime::install_check::found(
                    "rail strip indices",
                    format_args!("entry {at} of {length} holds {} where the list laid out whole holds {}", self.held[at], whole[at]),
                );
            }
        }
        Ok((length, (first < length).then_some(first..last + 1)))
    }

    fn of(&mut self, gpu: &Gpu<'_>, runs: &[Range<u32>]) -> io::Result<Option<(wgpu::Buffer, u32)>> {
        let (length, owed) = self.laid(runs)?;
        if length == 0 {
            *self = Self::default();
            return Ok(None);
        }
        let count = u32::try_from(length).map_err(|_| io::Error::other("rail strip indices exceed u32"))?;
        let buffer = match self.buffer.as_ref().filter(|_| self.capacity >= count) {
            Some(buffer) => {
                if let Some(owed) = owed {
                    shader_pipeline::gpu::stats::write_buffer(gpu.queue, buffer, owed.start as u64 * 4, bytemuck::cast_slice(&self.held[owed]));
                }
                self.sent = self.sent.max(length);
                buffer.clone()
            }
            None => {
                self.capacity = count.saturating_add(count / 4);
                let buffer = shader_pipeline::gpu::stats::create_buffer(gpu.device, &wgpu::BufferDescriptor {
                    label: Some("rails layer strip indices"),
                    size: u64::from(self.capacity) * 4,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                shader_pipeline::gpu::stats::write_buffer(gpu.queue, &buffer, 0, bytemuck::cast_slice(&self.held[..length]));
                self.sent = length;
                self.buffer = Some(buffer.clone());
                buffer
            }
        };
        Ok(Some((buffer, count)))
    }

    fn bytes(&self) -> usize {
        self.held.capacity() * 4 + self.placed.capacity() * 8
    }
}

struct OnGpu<'g, 'a> {
    gpu: &'g mut Gpu<'a>,
    buffer: &'g mut Option<wgpu::Buffer>,
    strips: &'g mut Strips,
    staged: Vec<[f32; 6]>,
    at: u32,
}

impl OnGpu<'_, '_> {
    fn flush(&mut self) {
        if let Some(buffer) = self.buffer.as_ref()
            && !self.staged.is_empty()
        {
            shader_pipeline::gpu::stats::write_buffer(self.gpu.queue, buffer, u64::from(self.at) * VERTEX, bytemuck::cast_slice(&self.staged));
        }
        self.staged.clear();
    }
}

impl Target for OnGpu<'_, '_> {
    fn reserve(&mut self, vertices: u32, kept: u32) -> io::Result<()> {
        self.flush();
        let grown = shader_pipeline::gpu::stats::create_buffer(self.gpu.device, &wgpu::BufferDescriptor {
            label: Some("rails layer strips"),
            size: u64::from(vertices) * VERTEX,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if let Some(old) = self.buffer.as_ref()
            && kept > 0
        {
            let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rails layer strips grown") });
            encoder.copy_buffer_to_buffer(old, 0, &grown, 0, u64::from(kept) * VERTEX);
            self.gpu.queue.submit([encoder.finish()]);
        }
        *self.buffer = Some(grown);
        Ok(())
    }

    fn write(&mut self, at: u32, vertices: &[[f32; 6]]) {
        if self.at + self.staged.len() as u32 != at || self.staged.len() + vertices.len() > STAGED {
            self.flush();
            self.at = at;
        }
        self.staged.extend_from_slice(vertices);
    }

    fn publish(&mut self, runs: &[Range<u32>]) -> io::Result<()> {
        self.flush();
        if runs.is_empty() {
            *self.buffer = None;
        }
        let strips = self.strips.of(self.gpu, runs)?;
        let geometry = PreparedRailGeometry::held(self.buffer.clone(), strips);
        drop(self.gpu.scene.set_prepared_railways(self.gpu.device, self.gpu.queue, &self.gpu.content.rail_assets, Arc::new(geometry))?);
        Ok(())
    }

    fn bridges(&mut self, bridges: &BTreeMap<(u32, u32), Bridge>) -> io::Result<()> {
        let field = self.gpu.content.bridges.field_of(bridges.values());
        self.gpu.scene.set_bridges(&self.gpu.content.bridges, field)
    }
}

#[derive(Clone, Copy)]
struct Columns {
    level: Num<i32>,
    node: Num<u8>,
    naval: Num<u8>,
    capital: Num<u16>,
    flags: Num<u8>,
}

struct Source;

impl Source {
    const NODE: u8 = 1;
    const NAVAL: u8 = 2;
    const CAPITAL: u8 = 4;
    const FLAG: u8 = 8;

    fn bit(columns: &Columns, column: usize, bits: Option<u64>) -> Option<(u8, bool)> {
        let bits = bits.unwrap_or(0);
        if column == columns.node.column {
            Some((Self::NODE, u8::from_raw(bits) != 0))
        } else if column == columns.naval.column {
            Some((Self::NAVAL, u8::from_raw(bits) != 0))
        } else if column == columns.capital.column {
            Some((Self::CAPITAL, u16::from_raw(bits) != 0))
        } else if column == columns.flags.column {
            Some((Self::FLAG, u8::from_raw(bits) & 1 != 0))
        } else {
            None
        }
    }

    fn of(columns: &Columns, supply: &Mirror, slot: u32) -> u8 {
        [columns.node.column, columns.naval.column, columns.capital.column, columns.flags.column]
            .into_iter()
            .filter_map(|column| Self::bit(columns, column, supply.cell(column, slot).value()))
            .fold(0, |held, (bit, set)| if set { held | bit } else { held })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    edges: bool,
    supply: bool,
    naval_whole: bool,
}

fn rows(mirror: Option<&Mirror>) -> Option<&Mirror> {
    mirror.filter(|mirror| matches!(mirror.status(), Some(Status::Complete | Status::Truncated { .. })))
}

pub trait Geography: Send + Sync {
    fn cutoff(&self) -> f32;
    fn run(&self, network: &RailNetwork, province: u32) -> io::Result<Option<RailRun>>;
    fn bridge(&self, network: &RailNetwork, key: (u32, u32)) -> Option<Bridge>;
}


fn pair(a: u32, b: u32) -> (u32, u32) {
    if a < b { (a, b) } else { (b, a) }
}

pub struct Rails {
    content: Arc<dyn Geography>,
    columns: Option<Columns>,
    held: Option<Held>,
    shown: bool,
    changed: Changed,
    network: RailNetwork,
    incoming: HashMap<u32, BTreeSet<u32>>,
    runs: BTreeMap<u32, RailRun>,
    bridges: BTreeMap<(u32, u32), Bridge>,
    unsent: BTreeSet<u32>,
    bridges_stale: bool,
    unpublished: bool,
    placed: Placed,
    buffer: Option<wgpu::Buffer>,
    strips_drawn: Strips,
    edge_keys: Vec<u64>,
    edge_marked: HashSet<u64>,
    supply: Dirty,
    supply_gone: Vec<u32>,
    inputs: Vec<u8>,
    ports: Dirty,
    strips: Dirty,
    pairs: BTreeSet<(u32, u32)>,
    pairs_since: Option<u64>,
    edges_since: Option<u64>,
    frame: u64,
    marked: [usize; 3],
}

impl Rails {
    pub fn new(content: Arc<dyn Geography>) -> Self {
        Self {
            content,
            columns: None,
            held: None,
            shown: true,
            changed: Changed::default(),
            network: RailNetwork::default(),
            incoming: HashMap::new(),
            runs: BTreeMap::new(),
            bridges: BTreeMap::new(),
            unsent: BTreeSet::new(),
            bridges_stale: true,
            unpublished: true,
            placed: Placed::default(),
            buffer: None,
            strips_drawn: Strips::default(),
            edge_keys: Vec::new(),
            edge_marked: HashSet::new(),
            supply: Dirty::default(),
            supply_gone: Vec::new(),
            inputs: Vec::new(),
            ports: Dirty::default(),
            strips: Dirty::default(),
            pairs: BTreeSet::new(),
            pairs_since: None,
            edges_since: None,
            frame: 0,
            marked: [0; 3],
        }
    }

    pub fn fresh(&self) -> Self {
        Self { columns: self.columns, shown: self.shown, ..Self::new(Arc::clone(&self.content)) }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn network(&self) -> &RailNetwork {
        &self.network
    }

    pub fn runs(&self) -> &BTreeMap<u32, RailRun> {
        &self.runs
    }

    pub fn bridges(&self) -> &BTreeMap<(u32, u32), Bridge> {
        &self.bridges
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    fn columns(&self) -> Columns {
        self.columns.expect("the rails layer is declared before it composes")
    }

    fn read_held(mirrors: &Mirrors) -> Held {
        Held {
            edges: rows(mirrors.get(TableId::RailEdges)).is_some(),
            supply: rows(mirrors.get(TableId::ProvinceSupply)).is_some(),
            naval_whole: mirrors.get(TableId::NavalBases).is_some_and(|mirror| mirror.status() == Some(Status::Complete)),
        }
    }

    fn mark_edge(&mut self, key: u128) {
        let key = key as u64;
        if self.edge_marked.insert(key) {
            self.edge_keys.push(key);
            self.edges_since.get_or_insert(self.frame);
        }
    }

    fn mark_pair(&mut self, key: (u32, u32)) {
        if self.pairs.insert(key) {
            self.pairs_since.get_or_insert(self.frame);
        }
    }

    fn mark_strip(&mut self, province: u32) {
        self.strips.mark(province, self.frame);
    }

    fn restart(&mut self, mirrors: &Mirrors, held: Held) {
        self.network = RailNetwork::default();
        self.incoming.clear();
        self.unsent.extend(self.placed.provinces.iter().copied());
        self.bridges_stale = true;
        self.runs.clear();
        self.bridges.clear();
        self.edge_keys.clear();
        self.edge_marked.clear();
        self.supply.clear();
        self.supply_gone.clear();
        self.ports.clear();
        self.strips.clear();
        self.pairs.clear();
        self.pairs_since = None;
        self.edges_since = None;
        self.held = Some(held);
        self.inputs.clear();
        if let Some(supply) = mirrors.get(TableId::ProvinceSupply).filter(|_| self.shown)
            && let Some(columns) = self.columns
        {
            for slot in supply.rows() {
                let at = slot as usize;
                if self.inputs.len() <= at {
                    self.inputs.resize(at + 1, 0);
                }
                self.inputs[at] = Source::of(&columns, supply, slot);
            }
        }
        if !held.naval_whole {
            return;
        }
        if held.edges
            && let Some(edges) = mirrors.get(TableId::RailEdges)
        {
            let keys: Vec<u128> = edges.rows().filter_map(|slot| edges.key(slot)).collect();
            keys.into_iter().for_each(|key| self.mark_edge(key));
        }
        if held.supply
            && let Some(supply) = mirrors.get(TableId::ProvinceSupply)
        {
            for slot in supply.rows() {
                self.supply.mark(slot, self.frame);
            }
        }
        if let Some(naval) = mirrors.get(TableId::NavalBases) {
            for slot in naval.rows() {
                if let Some(key) = naval.key(slot) {
                    self.ports.mark(key as u32, self.frame);
                }
            }
        }
    }

    fn edge_level(&self, mirrors: &Mirrors, key: u64) -> Option<i32> {
        let held = self.held?;
        if !held.naval_whole || !held.edges {
            return None;
        }
        let edges = mirrors.get(TableId::RailEdges)?;
        let slot = edges.slot(u128::from(key))?;
        let level = edges.number(self.columns().level, slot).value()?;
        let (from, to) = ((key >> 32) as u32, key as u32);
        (level > 0 && from != to).then_some(level)
    }

    fn settle_edge(&mut self, mirrors: &Mirrors, key: u64) {
        let (from, to) = ((key >> 32) as u32, key as u32);
        let want = self.edge_level(mirrors, key);
        let graph = &mut self.network.graph_mut().graph;
        let have = graph.get(&from).and_then(|neighbours| neighbours.get(&to)).copied();
        if want == have {
            return;
        }
        match want {
            Some(level) => {
                graph.entry(from).or_default().insert(to, level);
                self.incoming.entry(to).or_default().insert(from);
            }
            None => {
                if let Some(neighbours) = graph.get_mut(&from) {
                    neighbours.remove(&to);
                    if neighbours.is_empty() {
                        graph.remove(&from);
                    }
                }
                if let Some(sources) = self.incoming.get_mut(&to) {
                    sources.remove(&from);
                    if sources.is_empty() {
                        self.incoming.remove(&to);
                    }
                }
            }
        }
        self.reach(from);
        self.mark_pair(pair(from, to));
    }

    fn reach(&mut self, province: u32) {
        self.mark_strip(province);
        let before: Vec<u32> = self.incoming.get(&province).map(|set| set.iter().copied().collect()).unwrap_or_default();
        before.into_iter().for_each(|other| self.mark_strip(other));
    }

    fn supply_slot(&self, mirrors: &Mirrors, slot: u32) -> Option<(u32, bool)> {
        let supply = mirrors.get(TableId::ProvinceSupply)?;
        if !supply.live(slot) {
            return None;
        }
        let province = supply.key(slot)? as u32;
        Some((province, self.source(mirrors, province)))
    }

    fn source(&self, mirrors: &Mirrors, province: u32) -> bool {
        let held = self.held.unwrap_or_default();
        let columns = self.columns();
        held.naval_whole
            && held.supply
            && mirrors.get(TableId::ProvinceSupply).and_then(|supply| supply.slot(u128::from(province)).map(|slot| (supply, slot))).is_some_and(|(supply, slot)| {
                let number = |value: Option<u64>| value.is_some_and(|value| value != 0);
                number(supply.number(columns.node, slot).value().map(u64::from))
                    || number(supply.number(columns.naval, slot).value().map(u64::from))
                    || number(supply.number(columns.capital, slot).value().map(u64::from))
                    || supply.number(columns.flags, slot).value().is_some_and(|flags| flags & 1 != 0)
            })
    }

    fn settle_supply(&mut self, province: u32, source: bool) {
        if source == self.network.sources.contains(&province) {
            return;
        }
        let sources = &mut self.network.graph_mut().sources;
        if source {
            sources.insert(province);
        } else {
            sources.remove(&province);
        }
        self.mark_strip(province);
    }

    fn settle_port(&mut self, mirrors: &Mirrors, province: u32) {
        let held = self.held.unwrap_or_default();
        let port = held.naval_whole && mirrors.get(TableId::NavalBases).is_some_and(|naval| naval.slot(u128::from(province)).is_some());
        let ports = &mut self.network.graph_mut().ports;
        if port == ports.contains(&province) {
            return;
        }
        if port {
            ports.insert(province);
        } else {
            ports.remove(&province);
        }
        self.reach(province);
        let mut pairs: Vec<(u32, u32)> = self.network.graph.get(&province).map(|neighbours| neighbours.keys().map(|&other| pair(province, other)).collect()).unwrap_or_default();
        pairs.extend(self.incoming.get(&province).into_iter().flatten().map(|&other| pair(province, other)));
        pairs.into_iter().for_each(|key| self.mark_pair(key));
    }

    fn settle_strip(&mut self, province: u32) -> io::Result<()> {
        let run = self.content.run(&self.network, province)?;
        let held = self.runs.get(&province) == run.as_ref();
        match run {
            Some(run) => {
                self.runs.insert(province, run);
            }
            None => {
                self.runs.remove(&province);
            }
        }
        if !held {
            self.unsent.insert(province);
            self.changed.mark("province", u64::from(province));
        }
        Ok(())
    }

    fn settle_pair(&mut self, key: (u32, u32)) {
        let graph = &self.network.graph;
        let joined = graph.get(&key.0).is_some_and(|neighbours| neighbours.contains_key(&key.1)) || graph.get(&key.1).is_some_and(|neighbours| neighbours.contains_key(&key.0));
        let bridge = joined.then(|| self.content.bridge(&self.network, key)).flatten();
        if self.bridges.get(&key) != bridge.as_ref() {
            self.bridges_stale = true;
        }
        match bridge {
            Some(bridge) => {
                self.bridges.insert(key, bridge);
            }
            None => {
                self.bridges.remove(&key);
            }
        }
    }

    fn settle_dirty(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.frame += 1;
        if !self.shown {
            if self.held.is_some() {
                self.restart(mirrors, Held::default());
            }
            self.held = None;
            return Ok(());
        }
        let held = Self::read_held(mirrors);
        if self.held != Some(held) {
            self.restart(mirrors, held);
        }
        let mut done = 0;
        while done < self.edge_keys.len() {
            let key = self.edge_keys[done];
            self.edge_marked.remove(&key);
            done += 1;
            self.settle_edge(mirrors, key);
        }
        self.edge_keys.drain(..done);
        if self.edge_keys.is_empty() {
            self.edges_since = None;
        }
        for province in std::mem::take(&mut self.supply_gone) {
            let source = self.source(mirrors, province);
            self.settle_supply(province, source);
        }
        let mut dirty = std::mem::take(&mut self.supply);
        dirty.settle(|slot| {
            if let Some((province, source)) = self.supply_slot(mirrors, slot) {
                self.settle_supply(province, source);
            }
            Ok(())
        })?;
        self.supply = dirty;
        let mut dirty = std::mem::take(&mut self.ports);
        dirty.settle(|province| {
            self.settle_port(mirrors, province);
            Ok(())
        })?;
        self.ports = dirty;
        let mut dirty = std::mem::take(&mut self.strips);
        let settled = dirty.settle(|province| self.settle_strip(province));
        self.strips = dirty;
        settled?;
        while let Some(&key) = self.pairs.first() {
            self.pairs.remove(&key);
            self.settle_pair(key);
        }
        if self.pairs.is_empty() {
            self.pairs_since = None;
        }
        Ok(())
    }

    fn apply_edges(&mut self, changes: &Changes, mirror: &Mirror, columns: Columns) {
        for &key in changes.deleted().keys().unwrap_or_default().iter().chain(changes.inserted().keys().unwrap_or_default()) {
            self.mark_edge(key);
            self.marked[0] += 1;
        }
        for written in changes.columns().iter().filter(|written| written.column() == columns.level.column) {
            for &slot in written.slots() {
                if let Some(key) = mirror.key(slot) {
                    self.mark_edge(key);
                    self.marked[0] += 1;
                }
            }
        }
    }

    fn apply_supply(&mut self, changes: &Changes, mirror: &Mirror, columns: Columns) {
        self.supply_gone.extend(changes.deleted().keys().unwrap_or_default().iter().map(|&key| key as u32));
        self.marked[1] += changes.deleted().len();
        for &slot in changes.deleted().slots() {
            if let Some(bits) = self.inputs.get_mut(slot as usize) {
                *bits = 0;
            }
        }
        let mut was = Vec::new();
        for written in changes.columns() {
            if Source::bit(&columns, written.column(), None).is_none() {
                continue;
            }
            written.each(|slot, value| {
                let bits = match value {
                    replay_columnar::store::Value::Number(bits) => Some(bits),
                    _ => None,
                };
                let Some((bit, set)) = Source::bit(&columns, written.column(), bits) else { return };
                let at = slot as usize;
                if self.inputs.len() <= at {
                    self.inputs.resize(at + 1, 0);
                }
                was.push((slot, self.inputs[at]));
                self.inputs[at] = if set { self.inputs[at] | bit } else { self.inputs[at] & !bit };
            });
        }
        was.sort_by_key(|&(slot, _)| slot);
        was.dedup_by_key(|&mut (slot, _)| slot);
        for &slot in changes.inserted().slots() {
            let at = slot as usize;
            if self.inputs.len() <= at {
                self.inputs.resize(at + 1, 0);
            }
            self.inputs[at] = Source::of(&columns, mirror, slot);
        }
        let inputs = &self.inputs;
        let turned = was.into_iter().filter(|&(slot, before)| (before != 0) != (inputs[slot as usize] != 0)).map(|(slot, _)| slot);
        let marks: Vec<u32> = changes.inserted().slots().iter().copied().chain(turned).collect();
        for slot in marks {
            if self.supply.mark(slot, self.frame) {
                self.marked[1] += 1;
            }
        }
    }

    fn apply_ports(&mut self, changes: &Changes) {
        for &key in changes.deleted().keys().unwrap_or_default().iter().chain(changes.inserted().keys().unwrap_or_default()) {
            if self.ports.mark(key as u32, self.frame) {
                self.marked[2] += 1;
            }
        }
    }

    fn push(&mut self, target: &mut dyn Target) -> io::Result<()> {
        if self.bridges_stale {
            target.bridges(&self.bridges)?;
            self.bridges_stale = false;
        }
        let mut moved = std::mem::take(&mut self.unpublished);
        while !self.unsent.is_empty() {
            let Some(province) = self.unsent.pop_first() else { break };
            moved = true;
            match self.runs.get(&province).filter(|run| !run.vertices.is_empty()) {
                Some(run) => {
                    let runs = &self.runs;
                    let (at, grow) = self.placed.place(province, run.vertices.len() as u32, || runs.values().map(|run| run.vertices.len() as u32).sum());
                    if let Some(vertices) = grow {
                        target.reserve(vertices, at)?;
                    }
                    target.write(at, &run.vertices);
                }
                None => {
                    self.placed.remove(province);
                }
            }
        }
        if self.placed.provinces.is_empty() && self.placed.end > 0 {
            self.placed = Placed::default();
        }
        if moved {
            target.publish(&self.placed.runs)?;
        }
        Ok(())
    }

    pub fn differ(&self, other: &Self) -> (u64, Option<String>) {
        let mut differing = 0;
        let mut first = None;
        let mut note = |what: String| {
            differing += 1;
            first.get_or_insert(what);
        };
        if self.network.display() != other.network.display() {
            note("the rail graph, sources or harbours differ".into());
        }
        for province in self.runs.keys().chain(other.runs.keys()).collect::<BTreeSet<_>>() {
            let (a, b) = (self.runs.get(province), other.runs.get(province));
            let same = match (a, b) {
                (Some(a), Some(b)) => (a.segments, a.unanchored, a.samples) == (b.segments, b.unanchored, b.samples) && a.vertices.as_flattened().iter().map(|v| v.to_bits()).eq(b.vertices.as_flattened().iter().map(|v| v.to_bits())),
                (None, None) => true,
                _ => false,
            };
            if !same {
                note(format!("strip of province {province}: {} against {} vertices", a.map_or(0, |run| run.vertices.len()), b.map_or(0, |run| run.vertices.len())));
            }
        }
        for key in self.bridges.keys().chain(other.bridges.keys()).collect::<BTreeSet<_>>() {
            let (a, b) = (self.bridges.get(key), other.bridges.get(key));
            let same = match (a, b) {
                (Some(a), Some(b)) => a.position.map(f32::to_bits) == b.position.map(f32::to_bits) && a.rotation.to_bits() == b.rotation.to_bits() && a.kind == b.kind,
                (None, None) => true,
                _ => false,
            };
            if !same {
                note(format!("bridge of pair {key:?}: {a:?} against {b:?}"));
            }
        }
        (differing, first)
    }
}

impl Layer for Rails {
    fn name(&self) -> &'static str {
        "rails"
    }

    fn placed(&mut self, view: &viewer_layer_core::layers::View) -> Option<u64> {
        Some(u64::from(view.camera_height < self.content.cutoff()))
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        self.shown = view.camera_height < self.content.cutoff() && (view.graphics.rails() || view.graphics.railway_bridges());
        if !self.shown {
            return Ok(());
        }
        for table in [TableId::RailEdges, TableId::ProvinceSupply, TableId::NavalBases] {
            inputs.table(table, true)?;
        }
        self.columns = Some(Columns {
            level: inputs.number(TableId::RailEdges, "level")?,
            node: inputs.number(TableId::ProvinceSupply, "supply_node_level")?,
            naval: inputs.number(TableId::ProvinceSupply, "naval_base_level")?,
            capital: inputs.number(TableId::ProvinceSupply, "capital_country_index")?,
            flags: inputs.number(TableId::ProvinceSupply, "flags")?,
        });
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        let marked = self.marked;
        if self.shown {
            self.held = None;
        }
        self.settle_dirty(mirrors)?;
        self.marked = marked;
        Ok(())
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        if step.gap() || self.columns.is_none() || !self.shown {
            return;
        }
        let columns = self.columns();
        for changes in step.tables() {
            let table = TableId::ALL[changes.table()];
            let Some(mirror) = mirrors.get(table) else { continue };
            match table {
                TableId::RailEdges => self.apply_edges(changes, mirror, columns),
                TableId::ProvinceSupply => self.apply_supply(changes, mirror, columns),
                TableId::NavalBases => self.apply_ports(changes),
                _ => {}
            }
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        self.settle_dirty(mirrors)
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        if self.uploaded() {
            return Ok(());
        }
        let mut buffer = self.buffer.take();
        let mut strips = std::mem::take(&mut self.strips_drawn);
        let pushed = self.push(&mut OnGpu { gpu, buffer: &mut buffer, strips: &mut strips, staged: Vec::new(), at: 0 });
        (self.buffer, self.strips_drawn) = (buffer, strips);
        pushed
    }

    fn uploaded(&self) -> bool {
        self.unsent.is_empty() && !self.bridges_stale && !self.unpublished
    }

    fn carried(&self) -> Carried {
        let entities = self.edge_keys.len() + self.supply.len() + self.supply_gone.len() + self.ports.len() + self.strips.len() + self.pairs.len();
        Carried { entities }
    }

    fn held(&self) -> usize {
        let graph = &self.network.graph;
        let edges: usize = graph.values().map(BTreeMap::len).sum();
        let incoming: usize = self.incoming.values().map(BTreeSet::len).sum();
        self.runs.values().map(|run| run.vertices.capacity() * 24 + 64).sum::<usize>()
            + self.bridges.len() * 40
            + (graph.len() + edges + self.network.sources.len() + self.network.ports.len()) * 32
            + self.incoming.capacity() * 40
            + incoming * 16
            + self.edge_keys.capacity() * 8
            + self.edge_marked.capacity() * 9
            + self.supply.bytes()
            + self.inputs.capacity()
            + self.ports.bytes()
            + self.strips.bytes()
            + self.pairs.len() * 24
            + self.unsent.len() * 24
            + self.placed.bytes()
            + self.strips_drawn.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    fn changed(&self) -> Option<&Changed> {
        Some(&self.changed)
    }

    fn clear_changed(&mut self) {
        self.changed = Changed::default();
    }
}

#[cfg(test)]
pub mod tests;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
