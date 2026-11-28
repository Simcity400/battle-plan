use crate::playhead_host::{units::Units, mirror::Mirror};
use crate::layers::{Carried, Settling, Dirty, Inputs, Layer, Num, Retirement, View};
use crate::playhead_host::mirror::Mirrors;
use playhead::{
    Settings, Step, Timeline,
    pages::{Pages, Run},
};
use replay_columnar::{
    manifest::Status,
    state::{Cell, Kind},
    store::{Store, TableState},
    unit::{BLOCK, BlockPage, Encoder, PartSink, Settings as Writing, reader::Extents},
};
use replay_data::rows::TableId;
use std::{
    io,
    sync::{
        Arc, Mutex, mpsc,
        atomic::Ordering,
    },
};

pub const KINDS: [Kind; 4] = [Kind::I32, Kind::OptionalI64, Kind::U32List, Kind::Bytes];

#[derive(Default)]
struct Memory {
    bytes: Vec<u8>,
    pages: Vec<BlockPage>,
}

impl PartSink for Memory {
    fn frame(&mut self, stored: &[u8]) -> io::Result<u64> {
        let offset = self.bytes.len() as u64;
        self.bytes.extend_from_slice(stored);
        Ok(offset)
    }

    fn block(&mut self, page: BlockPage) -> io::Result<()> {
        self.pages.push(page);
        Ok(())
    }

    fn read_back(&mut self, offset: u64, stored: u32, out: &mut Vec<u8>) -> io::Result<()> {
        out.clear();
        out.extend_from_slice(&self.bytes[offset as usize..offset as usize + stored as usize]);
        Ok(())
    }
}

pub struct Recording {
    bytes: Vec<u8>,
    pub runs: Vec<Run>,
    pub kinds: Vec<Vec<Kind>>,
}

impl Pages for Recording {
    fn extents(&self, _run: usize) -> &dyn Extents {
        &self.bytes
    }

    fn runs(&self) -> &[Run] {
        &self.runs
    }

    fn kinds(&self, table: usize) -> Option<&[Kind]> {
        self.kinds.get(table).map(Vec::as_slice)
    }
}

pub fn record(tables: usize, hours: u32, hour: impl FnMut(u32, usize, &mut Store)) -> (Arc<Recording>, Vec<Vec<TableState>>) {
    record_with(Writing::at(3), tables, hours, hour)
}

pub fn record_with(settings: Writing, tables: usize, hours: u32, mut hour: impl FnMut(u32, usize, &mut Store)) -> (Arc<Recording>, Vec<Vec<TableState>>) {
    let kinds = vec![KINDS.to_vec(); tables];
    let mut store = Store::new(kinds.clone(), vec![false; tables]);
    store.attach(Encoder::new(&kinds, settings).unwrap());
    let mut memory = Memory::default();
    let mut states = vec![Vec::new(); tables];
    store.begin_run(0);
    for at in 0..hours {
        let text = format!("text {at}");
        store.begin_hour(at as i32, &[text.as_bytes()]).unwrap();
        for table in 0..tables {
            hour(at, table, &mut store);
        }
        store.end_hour(&mut memory).unwrap();
        if at % BLOCK == BLOCK - 1 {
            store.end_block();
        }
        for (table, held) in states.iter_mut().enumerate() {
            held.push(store.table_state(table).unwrap().clone());
        }
    }
    if !hours.is_multiple_of(BLOCK) {
        store.end_run(&mut memory).unwrap();
    }
    let run = Run::new(0, memory.pages).unwrap();
    (Arc::new(Recording { bytes: memory.bytes, runs: vec![run], kinds }), states)
}


pub struct Sum {
    pub(crate) column: Option<Num<i32>>,
    pub second: bool,
    pub(crate) refuse_high: bool,
    pub hide_high: bool,
    pub(crate) read: Vec<Option<i32>>,
    pub viewed: Vec<f32>,
    pub(crate) values: Vec<Option<i64>>,
    pub(crate) dirty: Dirty,
    pub(crate) composes: Arc<Mutex<u32>>,
    pub gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    pub entered: Option<mpsc::Sender<()>>,
    pub(crate) settle_gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    pub(crate) settling: Option<mpsc::Sender<()>>,
    pub stop_on_retirement: Option<mpsc::Sender<()>>,
    pub settle_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) settle_stops: bool,
    pub(crate) times_out_at: Option<i32>,
    pub(crate) frame: u64,
}

impl Sum {
    pub fn times_out_at(mut self, hour: i32) -> Self {
        self.times_out_at = Some(hour);
        self
    }

    pub fn new(composes: &Arc<Mutex<u32>>) -> Self {
        Self { column: None, second: false, refuse_high: false, hide_high: false, read: Vec::new(), viewed: Vec::new(), values: Vec::new(), dirty: Dirty::default(), composes: Arc::clone(composes), gate: None, entered: None, settle_gate: None, settling: None, stop_on_retirement: None, settle_armed: None, settle_stops: false, times_out_at: None, frame: 0 }
    }

    pub fn gated(composes: &Arc<Mutex<u32>>) -> (Self, mpsc::Sender<()>) {
        let (release, gate) = mpsc::channel();
        (Self { gate: Some(Arc::new(Mutex::new(gate))), ..Self::new(composes) }, release)
    }

    pub fn held_settle(composes: &Arc<Mutex<u32>>) -> (Self, mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (release, gate) = mpsc::channel();
        let (settling, began) = mpsc::channel();
        (Self { settle_gate: Some(Arc::new(Mutex::new(gate))), settling: Some(settling), ..Self::new(composes) }, release, began)
    }

    pub fn settle_stops(composes: &Arc<Mutex<u32>>) -> (Self, mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (layer, release, began) = Self::held_settle(composes);
        (Self { settle_stops: true, ..layer }, release, began)
    }

    pub fn total(&self) -> i64 {
        self.values.iter().flatten().sum()
    }
}

pub fn table(index: usize) -> TableId {
    TableId::ALL[index]
}

pub fn sum_of(mirror: &Mirror, column: Num<i32>) -> i64 {
    mirror.rows().filter_map(|slot| mirror.number(column, slot).value()).map(i64::from).sum()
}

impl Layer for Sum {
    fn retirement(&self) -> Option<Retirement> {
        let stop = self.stop_on_retirement.clone()?;
        Some(Box::new(move || { let _ = stop.send(()); }))
    }

    fn name(&self) -> &'static str {
        if self.second { "second" } else { "sum" }
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        if self.hide_high && view.camera_height >= 1.0 {
            self.column = None;
            return Ok(());
        }
        if self.refuse_high && view.camera_height >= 1.0 {
            return Err(io::Error::other("refused while the camera is high"));
        }
        let at = usize::from(self.second);
        inputs.table(table(at), true)?;
        inputs.index(table(at), 0)?;
        if self.second && view.camera_height >= 1.0 {
            inputs.index(table(at), 1)?;
        }
        self.column = Some(Num::of(table(at), 0));
        Ok(())
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if let Some(entered) = &self.entered {
            let _ = entered.send(());
        }
        if let Some(gate) = &self.gate {
            gate.lock().unwrap().recv().unwrap();
        }
        *self.composes.lock().unwrap() += 1;
        self.dirty.clear();
        let Some(column) = self.column else {
            self.values.clear();
            return Ok(());
        };
        let mirror = mirrors.get(column.table).unwrap();
        self.values = (0..mirror.slots() as u32).map(|slot| if mirror.live(slot) { mirror.number(column, slot).value().map(i64::from) } else { None }).collect();
        Ok(())
    }

    fn apply(&mut self, step: &Step, mirrors: &Mirrors) {
        let Some(column) = self.column else { return };
        let Some(changes) = step.table(column.table.index()) else { return };
        let _ = mirrors;
        let slots = changes.deleted().slots().iter().chain(changes.inserted().slots()).chain(changes.columns().iter().filter(|written| written.column() == 0).flat_map(|written| written.slots()));
        for &slot in slots {
            self.dirty.mark(slot, self.frame);
        }
    }

    fn settle(&mut self, mirrors: &Mirrors, _settling: Settling) -> io::Result<()> {
        if self.settle_armed.as_ref().is_none_or(|armed| armed.load(Ordering::Acquire)) {
            if let Some(settling) = &self.settling {
                let _ = settling.send(());
            }
            if let Some(gate) = self.settle_gate.take() {
                gate.lock().unwrap().recv().unwrap();
            }
        }
        if self.settle_stops {
            return Err(io::Error::other("the test layer settles no further"));
        }
        self.frame += 1;
        let Some(column) = self.column else { return Ok(()) };
        let mirror = mirrors.get(column.table).unwrap();
        if self.times_out_at.is_some() && mirror.head() == self.times_out_at {
            self.times_out_at = None;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the test layer times out once"));
        }
        self.read.push(mirror.head());
        let values = &mut self.values;
        self.dirty.settle(|slot| {
            if values.len() <= slot as usize {
                values.resize(slot as usize + 1, None);
            }
            values[slot as usize] = if mirror.live(slot) { mirror.number(column, slot).value().map(i64::from) } else { None };
            Ok(())
        })
    }


    fn carried(&self) -> Carried {
        Carried { entities: self.dirty.len() }
    }

    fn held(&self) -> usize {
        self.values.capacity() * 16 + self.dirty.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }

    fn viewed(&mut self, view: &View, _mirrors: &Mirrors) {
        self.viewed.push(view.camera_height);
    }
}

pub fn busy(at: u32, table: usize, store: &mut Store) {
    store.status(table, Status::Complete, true, Some(at as i32), true).unwrap();
    let mut changes = store.table(table).unwrap();
    if at == 0 {
        for slot in 0..40 {
            changes.insert_at(slot, 100 + u128::from(slot)).unwrap();
        }
    }
    for slot in 0..40 {
        changes.write(slot, 0, Cell::I32((at as i32) * 3 + slot as i32 + table as i32)).unwrap();
        changes.write(slot, 1, Cell::OptionalI64(Some(i64::from(at)))).unwrap();
    }
}

pub fn units(recording: Arc<Recording>, name: &str) -> Units {
    let kinds = Arc::new(recording.kinds.clone());
    let pages: Arc<dyn Pages> = recording;
    let timeline = Timeline::whole(&*pages).unwrap();
    Units { name: name.to_owned(), pages, timeline, kinds }
}


pub fn quick() -> Settings {
    Settings { step_threads: 1, fetch_threads: 1, ahead: 2, ring: 3, speed: 4.0, rate: 1.0, ..Settings::default() }
}
