
mod fold;

use viewer_layer_core::{layers::{Carried, Settling, Due, Fed, Gpu, Inputs, Layer, List, Num, Published, Retirement, RetirementFlag, View}, owned::{Shape, Table}};
use viewer_layer_core::playhead_host::mirror::{Mirror, Mirrors};
use fold::Fold;
use map_snow::{Refresh, SnowCells, SnowMud, Summary, winter::Winter};
use playhead::Step;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::VecDeque,
    io,
    ops::Range,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::Instant,
};
use viewer_runtime::{landed::{self, Landed}, presentation::Continuity};

const BAND: u32 = 8;
const RUN: u32 = 8;

#[derive(Clone, Debug, PartialEq)]
enum Changed {
    Unknown,
    Bands(Vec<Range<u32>>),
}

impl Default for Changed {
    fn default() -> Self {
        Self::Bands(Vec::new())
    }
}

impl Changed {
    fn of_cells(cells: &[u32], width: u32) -> Self {
        let mut changed = Self::default();
        for &cell in cells {
            let column = cell % width;
            changed.mark(cell / width / BAND, column..column + 1);
        }
        changed
    }

    fn mark(&mut self, band: u32, columns: Range<u32>) {
        let Self::Bands(bands) = self else { return };
        if columns.start >= columns.end {
            return;
        }
        let at = band as usize;
        if bands.len() <= at {
            bands.resize(at + 1, 0..0);
        }
        let held = &mut bands[at];
        *held = if held.start >= held.end { columns } else { held.start.min(columns.start)..held.end.max(columns.end) };
    }

    fn join(&mut self, other: Changed) {
        match other {
            Self::Unknown => *self = Self::Unknown,
            Self::Bands(other) => {
                for (band, columns) in other.into_iter().enumerate() {
                    self.mark(band as u32, columns);
                }
            }
        }
    }

    fn bands(&self, height: u32) -> usize {
        match self {
            Self::Unknown => height.div_ceil(BAND) as usize,
            Self::Bands(bands) => bands.iter().filter(|columns| columns.start < columns.end).count(),
        }
    }

    fn run_in(&self, [width, height]: [u32; 2], shown: Option<&Shown>) -> Option<(Range<u32>, Range<u32>)> {
        let bands = height.div_ceil(BAND);
        let columns = |band: u32| {
            let held = match self {
                Self::Unknown => 0..width,
                Self::Bands(held) => held.get(band as usize).cloned().unwrap_or(0..0),
            };
            match shown {
                Some(shown) if !shown.shows(band, &held) => 0..0,
                _ => held,
            }
        };
        let first = (0..bands).find(|&band| !columns(band).is_empty())?;
        let mut last = first;
        let mut bound = columns(first);
        while last + 1 < bands && last + 1 < first + RUN && !columns(last + 1).is_empty() {
            last += 1;
            let next = columns(last);
            bound = bound.start.min(next.start)..bound.end.max(next.end);
        }
        Some((first * BAND..((last + 1) * BAND).min(height), bound.start.min(width)..bound.end.min(width)))
    }

    fn written(&mut self, rows: Range<u32>, [width, height]: [u32; 2]) {
        if let Self::Unknown = self {
            *self = Self::Bands(vec![0..width; height.div_ceil(BAND) as usize]);
        }
        let Self::Bands(bands) = self else { return };
        for band in rows.start / BAND..rows.end.div_ceil(BAND) {
            if let Some(columns) = bands.get_mut(band as usize) {
                *columns = 0..0;
            }
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Unknown => 0,
            Self::Bands(bands) => bands.capacity() * 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Shown {
    rows: Range<u32>,
    columns: [Range<u32>; 2],
}

impl Shown {
    fn of(
        camera: &viewer_gpu::camera::Camera,
        viewport: [u32; 2],
        [width, height]: [u32; 2],
    ) -> Option<Self> {
        let ground = camera.visible_ground(viewport, MARGIN)?;
        let block = map_snow::SNOW_MUD_BLOCK as f32;
        let cells = |from: f32, to: f32| {
            let (from, to) = ((from / block).floor(), (to / block).ceil() + 1.0);
            (from.is_finite() && to.is_finite() && to > from).then_some((from as i64, to as i64))
        };
        let (first, last) = cells(ground[1], ground[3])?;
        let rows = first.clamp(0, i64::from(height)) as u32..last.clamp(0, i64::from(height)) as u32;
        let (first, last) = cells(ground[0], ground[2])?;
        let (span, width) = (last - first, i64::from(width));
        if span >= width {
            return None;
        }
        let start = first.rem_euclid(width);
        let columns = if start + span <= width {
            [start as u32..(start + span) as u32, 0..0]
        } else {
            [start as u32..width as u32, 0..(start + span - width) as u32]
        };
        Some(Self { rows, columns })
    }

    fn shows(&self, band: u32, columns: &Range<u32>) -> bool {
        let rows = band * BAND..(band + 1) * BAND;
        rows.start < self.rows.end
            && self.rows.start < rows.end
            && self
                .columns
                .iter()
                .any(|shown| columns.start < shown.end && shown.start < columns.end)
    }
}

const MARGIN: f32 = (BAND * map_snow::SNOW_MUD_BLOCK) as f32;

#[derive(Clone, Copy, Debug, Default)]
struct Figures {
    grids: u64,
    unshown: u64,
    bands: u64,
    unknown: u64,
    pieces: u64,
    written: u64,
    bytes: u64,
    ns: u64,
    wholes: u64,
    wholes_ns: u64,
}

trait Target {
    fn takes_parts(&self, grid: &SnowMud, continuity: Continuity) -> bool;
    fn whole(&mut self, grid: &Arc<SnowMud>, continuity: Continuity) -> io::Result<()>;
    fn begin(&mut self, grid: &SnowMud, continuity: Continuity) -> io::Result<()>;
    fn rect(&mut self, grid: &SnowMud, rows: Range<u32>, columns: Range<u32>) -> io::Result<()>;
    fn finish(&mut self, grid: &Arc<SnowMud>, continuity: Continuity) -> io::Result<()>;
}

struct OnScene<'a, 'b>(&'a mut Gpu<'b>);

impl Target for OnScene<'_, '_> {
    fn takes_parts(&self, grid: &SnowMud, continuity: Continuity) -> bool {
        self.0.scene.snow_mud_takes_parts(grid, continuity)
    }

    fn whole(&mut self, grid: &Arc<SnowMud>, continuity: Continuity) -> io::Result<()> {
        let gpu = &mut *self.0;
        gpu.scene.set_hour_layers(gpu.device, gpu.queue, viewer_world::scene::HourLayers { snow_mud: Some((grid, continuity)), ..Default::default() })
    }

    fn begin(&mut self, grid: &SnowMud, continuity: Continuity) -> io::Result<()> {
        self.0.scene.begin_snow_mud_parts(grid, continuity)
    }

    fn rect(&mut self, grid: &SnowMud, rows: Range<u32>, columns: Range<u32>) -> io::Result<()> {
        let gpu = &mut *self.0;
        gpu.scene.write_snow_mud_rect(gpu.queue, grid, rows, columns)
    }

    fn finish(&mut self, grid: &Arc<SnowMud>, continuity: Continuity) -> io::Result<()> {
        let gpu = &mut *self.0;
        gpu.scene.finish_snow_mud_parts(gpu.device, gpu.queue, grid, continuity)
    }
}

pub struct Content {
    cells: SnowCells,
    winter: Winter,
    size: viewer_gpu::camera::MapSize,
    defines: viewer_gpu::camera::MapDefines,
    volumes: Vec<Vec<u32>>,
    boundary: Vec<u32>,
    on_boundary: Vec<bool>,
}

impl Content {
    pub fn new(cells: SnowCells, winter: Winter, size: viewer_gpu::camera::MapSize, defines: viewer_gpu::camera::MapDefines) -> Self {
        let mut volumes = vec![Vec::new(); cells.provinces()];
        for (volume, records) in winter.records.iter().enumerate() {
            for record in records {
                if let Some(held) = volumes.get_mut(record.province as usize)
                    && held.last() != Some(&(volume as u32))
                {
                    held.push(volume as u32);
                }
            }
        }
        let [width, height] = cells.dimensions();
        let boundary: Vec<u32> = if width > 0 && height > 0 { map_snow::boundary(width, height).collect() } else { Vec::new() };
        let mut on_boundary = vec![false; cells.cells()];
        for &cell in &boundary {
            on_boundary[cell as usize] = true;
        }
        Self { cells, winter, size, defines, volumes, boundary, on_boundary }
    }

    fn bytes(&self) -> usize {
        self.volumes.capacity() * 24 + self.volumes.iter().map(|held| held.capacity() * 4).sum::<usize>() + self.boundary.capacity() * 4 + self.on_boundary.capacity()
    }
}

#[derive(Clone, Copy)]
struct Columns {
    snow: Num<i64>,
    display_snow: Num<u8>,
    display_mud: Num<u8>,
    display: List,
}

impl Columns {
    fn shape(&self) -> Shape {
        Shape::new(TableId::WeatherProvinces, &[self.snow.column, self.display_snow.column, self.display_mud.column], &[(self.display.column, false)])
    }
}

enum Job {
    Reset { epoch: u64, table: Option<Box<Table>>, columns: Columns, stamp: Option<Hour> },
    Step { epoch: u64, step: Arc<Step> },
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    grid: Result<Option<Arc<SnowMud>>, String>,
    changed: Changed,
    refused: Option<String>,
    marked: [u64; 3],
    held: usize,
    ns: u64,
}

fn superseded(next: Option<&Job>, epoch: u64, due: &Due) -> bool {
    match next {
        Some(Job::Reset { .. }) => true,
        Some(Job::Step { epoch: of, step }) => *of == epoch && due.reached(step.last()),
        None => false,
    }
}

fn run(content: Arc<Content>, jobs: Receiver<Job>, made: landed::Sender<Made>, due: Due) {
    let mut work: Option<(Fold, Option<Table>, [u64; 3], Option<Hour>)> = None;
    let mut epoch = 0;
    while let Ok(job) = jobs.recv() {
        let mut queue: VecDeque<Job> = std::iter::once(job).chain(jobs.try_iter()).collect();
        let mut started = Instant::now();
        while let Some(job) = queue.pop_front() {
            match job {
                Job::Reset { epoch: next, table, columns, stamp } => {
                    epoch = next;
                    work = Some((Fold::new(Arc::clone(&content), columns), table.map(|table| *table), [0; 3], stamp));
                }
                Job::Step { epoch: of, step } if of == epoch => {
                    let Some((fold, table, _, stamp)) = &mut work else { continue };
                    *stamp = (*stamp).max(Some(step.last()));
                    if let Some(table) = table {
                        if step.gap() {
                            table.gap();
                        } else if let Some(changes) = step.table(TableId::WeatherProvinces.index())
                            && let Some(delta) = table.step(changes)
                        {
                            fold.apply(&delta, table);
                        }
                    }
                    drop(step);
                    if superseded(queue.front(), epoch, &due) {
                        continue;
                    }
                }
                Job::Step { .. } => continue,
            }
            let Some((fold, table, reported, stamp)) = &mut work else { continue };
            fold.settle(table.as_ref());
            let (grid, changed) = match fold.grid() {
                Ok((grid, Some(cells))) => (Ok(grid), Changed::of_cells(&cells, content.cells.dimensions()[0].max(1))),
                Ok((grid, None)) => (Ok(grid), Changed::Unknown),
                Err(error) => (Err(error.to_string()), Changed::Unknown),
            };
            let held = fold.held() + table.as_ref().map_or(0, Table::bytes);
            let now = fold.marked();
            let marked = std::array::from_fn(|at| now[at] - reported[at]);
            *reported = now;
            let sent = Made { epoch, stamp: *stamp, grid, changed, refused: fold.refused(), marked, held, ns: started.elapsed().as_nanos() as u64 };
            if made.send(sent).is_err() {
                return;
            }
            started = Instant::now();
        }
    }
}

pub struct Snow {
    retiring: RetirementFlag,
    content: Result<Arc<Content>, String>,
    columns: Option<Columns>,
    jobs: Option<Sender<Job>>,
    made: Option<landed::Receiver<Made>>,
    epoch: u64,
    fed: Fed<Made>,
    grid: Result<Option<Arc<SnowMud>>, String>,
    landed_summary: Option<Landed<Summary>>,
    refused: Option<String>,
    marked: [u64; 3],
    worker_held: usize,
    in_view: Option<Shown>,
    shown: Option<Arc<SnowMud>>,
    reset: bool,
    owed: Changed,
    parting: Option<Continuity>,
    figures: Figures,
    spent: [u64; 3],
    worker: (u64, u64),
}

impl Snow {
    pub fn new(content: Result<Arc<Content>, String>) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            content,
            columns: None,
            jobs: None,
            made: None,
            epoch: 0,
            fed: Fed::default(),
            grid: Ok(None),
            landed_summary: None,
            refused: None,
            marked: [0; 3],
            worker_held: 0,
            in_view: None,
            shown: None,
            reset: true,
            owed: Changed::default(),
            parting: None,
            figures: Figures::default(),
            spent: [0; 3],
            worker: (0, 0),
        }
    }

    pub fn fresh(&self) -> Self {
        Self { columns: self.columns, ..Self::new(self.content.clone()) }
    }

    pub fn refused(&self) -> Option<String> {
        self.refused.clone()
    }

    fn drawn(&self) -> Option<&SnowMud> {
        self.grid.as_ref().ok().and_then(Option::as_deref)
    }

    pub fn texels(&self) -> Vec<[u8; 4]> {
        self.drawn().map_or_else(Vec::new, |grid| grid.texels.to_vec().chunks_exact(4).map(|texel| [texel[0], texel[1], texel[2], texel[3]]).collect())
    }

    pub fn unknown(&self) -> Vec<u8> {
        self.drawn().map_or_else(Vec::new, |grid| grid.unknown.to_vec())
    }

    pub fn province_snow(&self) -> impl Iterator<Item = Option<i64>> {
        self.drawn().map_or_else(Vec::new, |grid| grid.province_snow.to_vec()).into_iter()
    }

    pub fn factors(&self) -> Vec<Option<f32>> {
        self.drawn().and_then(|grid| grid.post_factors.as_ref()).map_or_else(Vec::new, |factors| factors.to_vec())
    }

    pub fn refresh(&self) -> Option<Refresh> {
        self.drawn().and_then(|grid| grid.refresh)
    }

    pub fn summary(&self) -> Option<Summary> {
        self.drawn().map(|grid| grid.summary)
    }

    pub fn landed_summary(&self) -> Option<Landed<Summary>> {
        self.landed_summary.clone()
    }

    pub fn marked(&self) -> [u64; 3] {
        self.marked
    }

    pub fn spent(&self) -> [u64; 3] {
        self.spent.map(|ns| ns / 1000)
    }

    pub fn worker(&self) -> (u64, u64) {
        (self.worker.0 / 1000, self.worker.1)
    }

    pub fn content_bytes(&self) -> usize {
        self.content.as_ref().map_or(0, |content| content.bytes())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn made_through(&self) -> Option<Hour> {
        self.fed.made_through()
    }

    fn take(&mut self, made: Landed<Made>) {
        self.landed_summary = made.as_ref().map(|made| made.grid.as_ref().ok().and_then(Option::as_deref).map(|grid| grid.summary)).transpose();
        let made = made.into_inner();
        if let Ok(Some(grid)) = &made.grid {
            self.figures.grids += 1;
            self.figures.bands += made.changed.bands(grid.height) as u64;
            self.figures.unknown += u64::from(made.changed == Changed::Unknown);
            self.owed.join(made.changed);
        } else {
            self.owed = Changed::Unknown;
        }
        (self.grid, self.refused, self.worker_held) = (made.grid, made.refused, made.held);
        for (held, made) in self.marked.iter_mut().zip(made.marked) {
            *held += made;
        }
        self.worker.0 += made.ns;
        self.worker.1 += 1;
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

    fn push(&mut self, target: &mut dyn Target) -> io::Result<()> {
        let grid = match &self.grid {
            Err(why) => return Err(io::Error::other(why.clone())),
            Ok(grid) => grid.clone(),
        };
        let started = Instant::now();
        let Some(grid) = grid.filter(|_| !self.uploaded()) else {
            self.spent[2] += started.elapsed().as_nanos() as u64;
            return Ok(());
        };
        let pushed = self.push_grid(target, &grid);
        self.spent[2] += started.elapsed().as_nanos() as u64;
        pushed
    }

    fn push_grid(&mut self, target: &mut dyn Target, grid: &Arc<SnowMud>) -> io::Result<()> {
        let size = [grid.width, grid.height];
        if self.parting.is_some_and(|run| run == Continuity::Continue && (grid.refresh.is_some() || self.shown.as_ref().is_some_and(|shown| [shown.width, shown.height] != size))) {
            (self.parting, self.reset) = (None, true);
        }
        if self.parting.is_none() {
            let continuity = if self.reset { Continuity::Reset } else { Continuity::Continue };
            if !target.takes_parts(grid, continuity) {
                let started = Instant::now();
                target.whole(grid, continuity)?;
                (self.figures.wholes, self.figures.wholes_ns) = (self.figures.wholes + 1, self.figures.wholes_ns + Instant::now().duration_since(started).as_nanos() as u64);
                (self.shown, self.reset, self.owed) = (Some(Arc::clone(grid)), false, Changed::default());
                return Ok(());
            }
            if self.reset {
                self.owed = Changed::Unknown;
            }
            target.begin(grid, continuity)?;
            (self.parting, self.reset) = (Some(continuity), false);
        }
        while let Some((rows, columns)) = self.owed.run_in(size, self.in_view.as_ref()) {
            let started = Instant::now();
            let bytes = rows.len() * columns.len() * 4;
            target.rect(grid, rows.clone(), columns)?;
            self.figures.pieces += 1;
            self.figures.written += u64::from(rows.end.div_ceil(BAND) - rows.start / BAND);
            self.owed.written(rows, size);
            self.figures.bytes += bytes as u64;
            self.figures.ns += Instant::now().duration_since(started).as_nanos() as u64;
        }
        let continuity = self.parting.take().unwrap_or(Continuity::Continue);
        target.finish(grid, continuity)?;
        self.figures.unshown += self.owed.bands(grid.height) as u64;
        self.shown = Some(Arc::clone(grid));
        Ok(())
    }
}

pub fn summary(side: &viewer_layer_core::playhead_host::MapSide<impl viewer_layer_core::playhead_host::PlaybackProducts>) -> serde_json::Value {
    let Some(layer) = side.parts().and_then(|(_, layers)| layers.iter().find_map(|layer| layer.any().downcast_ref::<Snow>())) else { return serde_json::Value::Null };
    let figures = layer.figures;
    let per = |sum: u64, count: u64| (count > 0).then(|| sum as f64 / count as f64);
    serde_json::json!({
        "grids_taken": figures.grids,
        "bands_changed_a_grid_mean": per(figures.bands, figures.grids),
        "grids_not_known_against_the_last": figures.unknown,
        "pieces_written": figures.pieces,
        "bytes_a_piece_mean": per(figures.bytes, figures.pieces),
        "us_a_piece_mean": per(figures.ns, figures.pieces * 1000),
        "bands_written": figures.written,
        "us_a_band_written_mean": per(figures.ns, figures.written * 1000),
        "bands_left_outside_the_view": figures.unshown,
        "grids_taken_whole": figures.wholes,
        "us_a_whole_grid_mean": per(figures.wholes_ns, figures.wholes * 1000),
    })
}

impl Layer for Snow {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        "snow"
    }

    fn placed(&mut self, view: &viewer_layer_core::layers::View) -> Option<u64> {
        let size = self.content.as_ref().map(|content| content.cells.dimensions()).unwrap_or([0, 0]);
        self.in_view = match (view.camera, self.content.as_ref()) {
            (Some(placement), Ok(content)) if view.viewport != [0, 0] && size != [0, 0] => {
                Shown::of(&placement.on(content.size, content.defines), view.viewport, size)
            }
            _ => None,
        };
        Some(0)
    }

    fn inputs(&mut self, _view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if let Err(why) = &self.content {
            return Err(io::Error::other(format!("The snow layer is refused: its content cannot be read: {why}")));
        }
        let table = TableId::WeatherProvinces;
        inputs.table(table, true)?;
        self.columns = Some(Columns {
            snow: inputs.number(table, "snow")?,
            display_snow: inputs.number(table, "display_snow")?,
            display_mud: inputs.number(table, "display_mud")?,
            display: inputs.list(table, "display")?,
        });
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        let (Ok(content), Some(columns)) = (&self.content, self.columns) else { return Ok(()) };
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = landed::channel();
            let (content, due) = (Arc::clone(content), self.fed.due());
            std::thread::Builder::new().name("snow fold".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                run(content, receive, send, due)
            })?;
            (self.jobs, self.made) = (Some(jobs), Some(made));
        }
        self.epoch += 1;
        self.fed.compose(mirrors.get(TableId::WeatherProvinces).and_then(Mirror::head));
        let table = mirrors.get(TableId::WeatherProvinces).map(|mirror| Box::new(Table::from_mirror(mirror, &columns.shape())));
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Reset { epoch: self.epoch, table, columns, stamp: None });
        }
        (self.shown, self.reset, self.owed, self.parting) = (None, true, Changed::Unknown, None);
        self.exchange(true)?;
        Ok(())
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        if self.columns.is_some() {
            self.fed.applied(step);
        }
    }

    fn wants_ahead(&self) -> bool {
        true
    }

    fn void_ahead(&mut self, mirrors: &Mirrors, _kept: Option<Hour>) {
        let (Some(columns), Some(jobs)) = (self.columns, &self.jobs) else { return };
        self.epoch += 1;
        self.fed.void();
        let table = mirrors.get(TableId::WeatherProvinces).map(|mirror| Box::new(Table::from_mirror(mirror, &columns.shape())));
        let _ = jobs.send(Job::Reset { epoch: self.epoch, table, columns, stamp: self.fed.applied_hour() });
    }

    fn ahead(&mut self, steps: &[Arc<Step>]) {
        let started = Instant::now();
        if let Some(jobs) = &self.jobs {
            for step in self.fed.hand(steps) {
                let _ = jobs.send(Job::Step { epoch: self.epoch, step: Arc::clone(step) });
            }
        }
        self.spent[0] += started.elapsed().as_nanos() as u64;
    }

    fn settle(&mut self, _mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        let started = Instant::now();
        self.exchange(settling.waits())?;
        self.spent[1] += started.elapsed().as_nanos() as u64;
        Ok(())
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        let _push = replay_frame::cpu::scope("snow.push", Some("snow"));
        self.push(&mut OnScene(gpu))
    }

    fn uploaded(&self) -> bool {
        match (&self.grid, &self.shown) {
            (Ok(Some(grid)), Some(shown)) => Arc::ptr_eq(grid, shown) && self.owed.run_in([grid.width, grid.height], self.in_view.as_ref()).is_none(),
            (Ok(Some(_)), None) => false,
            _ => true,
        }
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> Published {
        self.fed.published()
    }

    fn held(&self) -> usize {
        self.worker_held + self.owed.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests;
