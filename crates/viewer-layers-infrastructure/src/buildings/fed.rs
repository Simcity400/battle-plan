
use std::time::Instant;
use super::{Buildings, Declared, FromLayer, Inspected, Install, NOT_SLOTTED, READ, Seen, Upper};
use viewer_layer_core::{layers::{Carried, Retirement, RetirementFlag, Change, Changed, Settling, Fed as Products, Gpu, Inputs, Layer, Published, Scope, Scoped, Sight, Upstream, View}, playhead_host::mirror::{Mirror, Mirrors}};
use playhead::Step;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_runtime::landed;

type SceneInputs = ([u8; 32], Option<(i64, i64)>);

struct VisionCopy {
    state: (Option<u32>, bool, u64),
    refused: Option<String>,
    bytes: Vec<Option<u8>>,
    changed: Changed,
}

impl Seen for VisionCopy {
    fn ours(&self) -> u32 {
        self.state.0.unwrap_or_default()
    }

    fn byte(&self, province: u32) -> Option<u8> {
        self.bytes.get(province as usize).copied().flatten()
    }
}

impl Upper for VisionCopy {
    fn state(&self) -> (Option<u32>, bool, u64) {
        self.state
    }

    fn refused(&self) -> Option<String> {
        self.refused.clone()
    }

    fn seen(&self) -> &dyn Seen {
        self
    }

    fn moved(&self) -> Change<'_> {
        self.changed.read()
    }
}

enum Seeing {
    Absent,
    Seen { state: (Option<u32>, bool, u64), refused: Option<String>, whole: bool, bytes: Vec<(u32, Option<u8>)>, moved: Vec<u64> },
}

enum Job {
    Reset { epoch: u64, model: Option<Box<Buildings>>, mirrors: Box<Mirrors>, declared: Declared, seeing: Option<Seeing>, given: Option<(Choice, Arc<Vision>)>, scene: Option<SceneInputs> },
    Frame { epoch: u64, stamp: Option<Hour>, declared: Option<Declared>, steps: Vec<Arc<Step>>, seeing: Option<Seeing>, given: Option<(Choice, Arc<Vision>)>, scene: Option<SceneInputs> },
    Inspect(mpsc::Sender<Inspected>),
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    answers: usize,
    install: Option<Install>,
    scene: Option<SceneInputs>,
    refused: Option<String>,
    error: Option<String>,
    warnings: Vec<String>,
    marked: [usize; 3],
    causes: [u64; super::CAUSES.len()],
    uploads: [u64; 6],
    upload_ns: [u64; 3],
    scoped: Scoped,
    held: usize,
}

struct Work {
    model: Box<Buildings>,
    mirrors: Box<Mirrors>,
    vision: Option<VisionCopy>,
    scene: Option<SceneInputs>,
}

impl Work {
    fn take(&mut self, declared: Option<Declared>, steps: Vec<Arc<Step>>, seeing: Option<Seeing>, given: Option<(Choice, Arc<Vision>)>, scene: Option<SceneInputs>) {
        if let Some(declared) = declared {
            self.model.declare(declared);
        }
        for step in steps {
            self.mirrors.apply(&step);
            self.model.apply(&step, &self.mirrors);
        }
        match seeing {
            None => {}
            Some(Seeing::Absent) => self.vision = None,
            Some(Seeing::Seen { state, refused, whole, bytes, moved }) => {
                let size = self.model.fog_seen.len();
                let copy = match (&mut self.vision, whole) {
                    (Some(copy), false) => copy,
                    (held, _) => held.insert(VisionCopy { state, refused: None, bytes: vec![None; size], changed: Changed::default() }),
                };
                (copy.state, copy.refused) = (state, refused);
                for (province, byte) in bytes {
                    if let Some(held) = copy.bytes.get_mut(province as usize) {
                        *held = byte;
                    }
                }
                if whole {
                    copy.changed.everything();
                }
                moved.into_iter().for_each(|province| copy.changed.mark("province", province));
            }
        }
        if let Some((choice, vision)) = given {
            self.model.set_vision(choice, vision);
        }
        if scene.is_some() {
            self.scene = scene;
        }
    }

    fn settle(&mut self, compose: bool) -> io::Result<Option<Install>> {
        let upper = self.vision.as_ref().map(|copy| copy as &dyn Upper);
        if compose {
            self.model.compose_upper(&self.mirrors, upper)?;
        } else {
            self.model.settle_dirty(&self.mirrors, upper)?;
        }
        if let Some(copy) = &mut self.vision {
            copy.changed.clear();
        }
        let Some((bank, thresholds)) = self.scene else { return Ok(None) };
        self.model.note_thresholds(thresholds);
        self.model.make(bank, thresholds)
    }

    fn made(&mut self, epoch: u64, stamp: Option<Hour>, answers: usize, result: io::Result<Option<Install>>) -> Made {
        let model = &mut self.model;
        let held = model.held() + self.mirrors.iter().map(Mirror::bytes).sum::<usize>();
        let (install, error) = match result {
            Ok(install) => (install, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Made {
            epoch,
            stamp,
            answers,
            install,
            scene: self.scene,
            refused: model.refused.clone(),
            error,
            warnings: model.warnings(),
            marked: model.marked,
            causes: model.causes,
            uploads: model.uploads,
            upload_ns: model.upload_ns,
            scoped: std::mem::take(&mut model.scoped),
            held,
        }
    }
}

fn run(jobs: mpsc::Receiver<Job>, made: landed::Sender<Made>) {
    let mut work: Option<Work> = None;
    let mut epoch = 0;
    let mut next = None;
    loop {
        let job = match next.take() {
            Some(job) => job,
            None => match jobs.recv() {
                Ok(job) => job,
                Err(_) => return,
            },
        };
        let product = match job {
            Job::Inspect(reply) => {
                if let Some(held) = &work {
                    let _ = reply.send(held.model.inspect());
                }
                continue;
            }
            Job::Reset { epoch: of, model, mirrors, declared, seeing, given, scene } => {
                epoch = of;
                let held = match (work.take(), model) {
                    (_, Some(model)) => Work { model, mirrors, vision: None, scene: None },
                    (Some(held), None) => Work { mirrors, ..held },
                    (None, None) => return,
                };
                let held = work.insert(held);
                held.take(Some(declared), Vec::new(), seeing, given, scene);
                let result = held.settle(true);
                held.made(epoch, None, 1, result)
            }
            Job::Frame { epoch: of, .. } if of != epoch => continue,
            Job::Frame { stamp, declared, steps, seeing, given, scene, .. } => {
                let Some(held) = &mut work else { continue };
                held.take(declared, steps, seeing, given, scene);
                let (mut stamp, mut answers) = (stamp, 1);
                while let Ok(queued) = jobs.try_recv() {
                    match queued {
                        Job::Frame { epoch: of, stamp: at, declared, steps, seeing, given, scene } if of == epoch => {
                            held.take(declared, steps, seeing, given, scene);
                            (stamp, answers) = (at, answers + 1);
                        }
                        Job::Frame { .. } => {}
                        other => {
                            next = Some(other);
                            break;
                        }
                    }
                }
                let result = held.settle(false);
                held.made(epoch, stamp, answers, result)
            }
        };
        if made.send(product).is_err() {
            return;
        }
    }
}

pub struct BuildingsFed {
    retiring: RetirementFlag,
    spec: Buildings,
    jobs: Option<mpsc::Sender<Job>>,
    made: Option<landed::Receiver<Made>>,
    epoch: u64,
    started: bool,
    owed: usize,
    products: Products<Made>,
    waiting: VecDeque<Arc<Step>>,
    declared: Option<Declared>,
    declared_new: bool,
    seen_sent: Option<((Option<u32>, bool, u64), Option<String>)>,
    given: Option<(Choice, Arc<Vision>)>,
    scene: Option<SceneInputs>,
    pending: Option<Install>,
    cold_owed: bool,
    refused: Option<String>,
    error: Option<String>,
    warnings: Vec<String>,
    marked: [usize; 3],
    causes: [u64; super::CAUSES.len()],
    uploads: [u64; 6],
    upload_ns: [u64; 3],
    install_ns: u64,
    scoped: Scoped,
    worker_held: usize,
    stopped: bool,
}

impl BuildingsFed {
    pub fn new(buildings: Buildings) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            spec: buildings,
            jobs: None,
            made: None,
            epoch: 0,
            started: false,
            owed: 0,
            products: Products::default(),
            waiting: VecDeque::new(),
            declared: None,
            declared_new: false,
            seen_sent: None,
            given: None,
            scene: None,
            pending: None,
            cold_owed: false,
            refused: Some("not composed yet".into()),
            error: None,
            warnings: Vec::new(),
            marked: [0; 3],
            causes: [0; super::CAUSES.len()],
            uploads: [0; 6],
            upload_ns: [0; 3],
            install_ns: 0,
            scoped: Scoped::default(),
            worker_held: 0,
            stopped: false,
        }
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        self.spec.set_vision(choice, vision);
    }

    pub fn perspective(&self) -> Option<&Choice> {
        self.given.as_ref().map(|(choice, _)| choice)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    pub fn inspect(&self) -> Option<Inspected> {
        let (reply, answer) = mpsc::channel();
        self.jobs.as_ref()?.send(Job::Inspect(reply)).ok()?;
        answer.recv().ok()
    }

    pub fn fresh(&self) -> Buildings {
        let fresh = self.spec.fresh();
        if let Some((choice, vision)) = &self.given {
            fresh.set_vision(choice.clone(), Arc::clone(vision));
        }
        fresh
    }

    pub fn figures(&self) -> serde_json::Value {
        let mut upload_ns = self.upload_ns;
        upload_ns[2] += self.install_ns;
        super::figures(&self.causes, &self.uploads, &upload_ns)
    }

    fn send(&mut self, job: Job) {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_ok()
        {
            self.owed += 1;
        }
    }

    fn seeing(&mut self, upstream: &Upstream<'_>) -> Option<Seeing> {
        if !self.spec.reads_layer {
            return None;
        }
        let Some(upper) = FromLayer::of(upstream) else { return self.seen_sent.take().map(|_| Seeing::Absent) };
        let (state, refused) = (upper.state(), upper.refused());
        let whole = self.seen_sent.as_ref().is_none_or(|(sent, _)| *sent != state) || matches!(upper.moved(), Change::Whole);
        let fog = &self.spec.fog_seen;
        let moved: Vec<u64> = match upper.moved() {
            Change::Marked(marked) if !whole => marked.of("province").iter().copied().filter(|&province| fog.get(province as usize).is_some_and(|&seen| seen != NOT_SLOTTED)).collect(),
            _ => Vec::new(),
        };
        if !whole && moved.is_empty() && self.seen_sent.as_ref().is_some_and(|(_, sent)| *sent == refused) {
            return None;
        }
        let seen = upper.seen();
        let bytes = if whole { self.spec.fog_provinces.iter().map(|&province| (province, seen.byte(province))).collect() } else { moved.iter().map(|&province| (province as u32, seen.byte(province as u32))).collect() };
        self.seen_sent = Some((state, refused.clone()));
        Some(Seeing::Seen { state, refused, whole, bytes, moved })
    }

    fn take_given(&mut self) -> Option<(Choice, Arc<Vision>)> {
        if !self.spec.fresh_vision.swap(false, Ordering::AcqRel) {
            return None;
        }
        let given = self.spec.given.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()?;
        self.given = Some(given.clone());
        Some(given)
    }

    fn declared_now(&self) -> Declared {
        self.declared.unwrap_or(Declared { columns: self.spec.columns, fog: self.spec.fog })
    }

    fn exchange(&mut self, wait: bool) -> io::Result<()> {
        if let Some(receive) = &self.made {
            loop {
                let made = if wait && self.owed > 0 { self.retiring.receive(receive)?.ok_or(mpsc::TryRecvError::Disconnected) } else { receive.try_recv() };
                let made = match made {
                    Ok(made) => made,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        (self.owed, self.stopped) = (0, true);
                        break;
                    }
                };
                if made.epoch == self.epoch {
                    self.owed -= made.answers;
                    self.products.receive(made.stamp, made);
                }
            }
        }
        let mut due = Vec::new();
        self.products.take(|made| due.push(made.into_inner()));
        for made in due {
            if let Some((product, continues)) = made.install {
                if made.scene == self.scene {
                    if let Some((_, held)) = self.pending.replace((product, continues)) {
                        self.cold_owed |= !held;
                    }
                } else {
                    self.cold_owed |= !continues;
                }
            }
            (self.refused, self.warnings, self.marked) = (made.refused, made.warnings, made.marked);
            (self.causes, self.uploads, self.upload_ns, self.worker_held) = (made.causes, made.uploads, made.upload_ns, made.held);
            self.scoped.extend(made.scoped);
            if let Some(error) = made.error {
                self.error.get_or_insert(error);
            }
        }
        if self.stopped {
            self.pending = None;
        }
        match &self.error {
            Some(error) => Err(io::Error::other(format!("The buildings' worker could not settle: {error}"))),
            None => Ok(()),
        }
    }
}

impl Layer for BuildingsFed {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        "buildings"
    }

    fn placed(&mut self, view: &View) -> Option<u64> {
        self.spec.placed(view)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        let result = self.spec.inputs(view, inputs);
        (self.declared, self.declared_new) = (Some(Declared { columns: self.spec.columns, fog: self.spec.fog }), true);
        result
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        self.compose_with(mirrors, &Upstream::new(&[]))
    }

    fn compose_with(&mut self, mirrors: &Mirrors, upstream: &Upstream<'_>) -> io::Result<()> {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = landed::channel();
            thread::Builder::new().name("buildings".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                run(receive, send)
            })?;
            (self.jobs, self.made) = (Some(jobs), Some(made));
        }
        let mut copies = Box::<Mirrors>::default();
        for table in READ {
            if let Some(mirror) = mirrors.get(table) {
                copies.insert(Box::new(mirror.clone()));
            }
        }
        let model = (!std::mem::replace(&mut self.started, true)).then(|| Box::new(self.spec.fresh()));
        self.seen_sent = None;
        let seeing = self.seeing(upstream);
        let given = self.take_given().or_else(|| self.given.clone());
        let declared = self.declared_now();
        self.declared_new = false;
        self.epoch += 1;
        self.owed = 0;
        self.products.compose(mirrors.get(TableId::Provinces).and_then(Mirror::head));
        self.waiting.clear();
        if let Some((_, continues)) = self.pending.take() {
            self.cold_owed |= !continues;
        }
        self.error = None;
        let scene = self.scene;
        self.send(Job::Reset { epoch: self.epoch, model, mirrors: copies, declared, seeing, given, scene });
        self.exchange(true)
    }

    fn apply(&mut self, step: &Step, _mirrors: &Mirrors) {
        self.products.applied(step);
    }

    fn wants_ahead(&self) -> bool {
        true
    }

    fn ahead(&mut self, steps: &[Arc<Step>]) {
        self.waiting.extend(steps.iter().cloned());
    }

    fn void_ahead(&mut self, _mirrors: &Mirrors, kept: Option<Hour>) {
        self.waiting.retain(|step| kept.is_some_and(|kept| step.last() <= kept));
    }

    fn settle(&mut self, mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        self.settle_with(mirrors, &Upstream::new(&[]), settling)
    }

    fn settle_with(&mut self, _mirrors: &Mirrors, upstream: &Upstream<'_>, settling: Settling) -> io::Result<()> {
        let whole = settling.waits();
        self.exchange(false)?;
        let applied = self.products.applied_hour();
        let mut steps = Vec::new();
        while self.waiting.front().is_some_and(|step| applied.is_some_and(|applied| step.last() <= applied)) {
            steps.extend(self.waiting.pop_front());
        }
        let declared = std::mem::take(&mut self.declared_new).then(|| self.declared_now());
        let seeing = self.seeing(upstream);
        let given = self.take_given();
        if declared.is_some() || !steps.is_empty() || seeing.is_some() || given.is_some() {
            self.send(Job::Frame { epoch: self.epoch, stamp: applied, declared, steps, seeing, given, scene: None });
        }
        self.exchange(whole)
    }

    fn upload(&mut self, gpu: &mut Gpu<'_>) -> io::Result<()> {
        if self.declared_now().columns.is_none() {
            return Ok(());
        }
        let thresholds = gpu.scene.city_damage_thresholds();
        match gpu.content.products.display_binding() {
            Ok(bank) if self.scene != Some((bank, thresholds)) => {
                self.scene = Some((bank, thresholds));
                let stamp = self.products.applied_hour();
                self.send(Job::Frame { epoch: self.epoch, stamp, declared: None, steps: Vec::new(), seeing: None, given: None, scene: Some((bank, thresholds)) });
            }
            Ok(_) => {}
            Err(error) if self.refused.is_none() => return Err(error),
            Err(_) => {}
        }
        self.exchange(gpu.whole)?;
        if self.refused.is_some() || self.pending.is_none() {
            return Ok(());
        }
        let (product, continues) = self.pending.take().expect("a product is pending");
        let continues = continues && !std::mem::take(&mut self.cold_owed);
        let started = Instant::now();
        super::install(gpu, (product, continues))?;
        self.install_ns += Instant::now().duration_since(started).as_nanos() as u64;
        Ok(())
    }

    fn uploaded(&self) -> bool {
        self.declared_now().columns.is_none() || self.pending.is_none()
    }

    fn carried(&self) -> Carried {
        Carried::default()
    }

    fn published(&self) -> Published {
        if self.stopped {
            return Published::Nothing;
        }
        self.products.published()
    }

    fn scope(&mut self, sight: &Sight<'_>) -> Option<Scope> {
        Some(self.scoped.take(sight, None))
    }

    fn held(&self) -> usize {
        self.worker_held + self.waiting.capacity() * 8 + self.scoped.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}
