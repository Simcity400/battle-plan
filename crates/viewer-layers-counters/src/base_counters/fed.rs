
use super::{BaseCounters, TABLES};
use viewer_layer_core::{layers::{Carried, Retirement, RetirementFlag, Settling, Fed as Products, Inputs, Layer, Published, View}, playhead_host::mirror::{Mirror, Mirrors}};
use playhead::Step;
use replay_data::{Hour, rows::TableId};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
};
use viewer_counters_naval::bases::BaseValues;
use viewer_foundation::perspective::{Choice, Vision};

enum Job {
    Reset { epoch: u64, model: Box<BaseCounters>, mirrors: Box<Mirrors> },
    Frame { epoch: u64, stamp: Option<Hour>, steps: Vec<Arc<Step>>, given: Option<(Choice, Arc<Vision>)> },
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    answers: usize,
    values: Option<Arc<BaseValues>>,
    refused: Option<String>,
    made: usize,
    work: u64,
    held: usize,
}

struct Work {
    model: Box<BaseCounters>,
    mirrors: Box<Mirrors>,
}

impl Work {
    fn take(&mut self, steps: Vec<Arc<Step>>, given: Option<(Choice, Arc<Vision>)>) {
        for step in steps {
            self.mirrors.apply(&step);
            self.model.apply(&step, &self.mirrors);
        }
        if let Some((choice, vision)) = given {
            self.model.set_vision(choice, vision);
        }
    }

    fn made(&self, epoch: u64, stamp: Option<Hour>, answers: usize) -> Made {
        let held = self.model.held() + self.mirrors.iter().map(Mirror::bytes).sum::<usize>();
        Made { epoch, stamp, answers, values: self.model.values.clone(), refused: self.model.refused.clone(), made: self.model.made, work: self.model.work, held }
    }
}

fn run(jobs: mpsc::Receiver<Job>, made: viewer_runtime::landed::Sender<Made>) {
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
            Job::Reset { epoch: of, mut model, mirrors } => {
                epoch = of;
                let _ = model.compose(&mirrors);
                work.insert(Work { model, mirrors }).made(epoch, None, 1)
            }
            Job::Frame { epoch: of, .. } if of != epoch => continue,
            Job::Frame { stamp, steps, given, .. } => {
                let Some(held) = &mut work else { continue };
                held.take(steps, given);
                let (mut stamp, mut answers) = (stamp, 1);
                while let Ok(queued) = jobs.try_recv() {
                    match queued {
                        Job::Frame { epoch: of, stamp: at, steps, given } if of == epoch => {
                            held.take(steps, given);
                            (stamp, answers) = (at, answers + 1);
                        }
                        Job::Frame { .. } => {}
                        reset @ Job::Reset { .. } => {
                            next = Some(reset);
                            break;
                        }
                    }
                }
                held.model.settle_dirty(&held.mirrors);
                held.made(epoch, stamp, answers)
            }
        };
        if made.send(product).is_err() {
            return;
        }
    }
}

pub struct BaseCountersFed {
    retiring: RetirementFlag,
    spec: BaseCounters,
    jobs: Option<mpsc::Sender<Job>>,
    made: Option<viewer_runtime::landed::Receiver<Made>>,
    epoch: u64,
    owed: usize,
    products: Products<Made>,
    waiting: VecDeque<Arc<Step>>,
    values: Option<Arc<BaseValues>>,
    refused: Option<String>,
    units: usize,
    work: u64,
    worker_held: usize,
    stopped: bool,
}

impl BaseCountersFed {
    pub fn new(counters: BaseCounters) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            spec: counters,
            jobs: None,
            made: None,
            epoch: 0,
            owed: 0,
            products: Products::default(),
            waiting: VecDeque::new(),
            values: None,
            refused: Some("not composed yet".into()),
            units: 0,
            work: 0,
            worker_held: 0,
            stopped: false,
        }
    }

    pub fn set_vision(&self, choice: Choice, vision: Arc<Vision>) {
        self.spec.set_vision(choice, vision);
    }

    pub fn perspective(&self) -> Option<(Choice, Arc<Vision>)> {
        self.spec.perspective()
    }

    pub fn refused(&self) -> Option<&str> {
        if self.stopped {
            return Some("the base counters' worker stopped");
        }
        self.refused.as_deref()
    }

    pub fn hidden(&self) -> bool {
        self.spec.hidden()
    }

    pub fn values(&self) -> Option<&BaseValues> {
        self.values.as_deref()
    }

    pub fn published(&self) -> Option<&Arc<BaseValues>> {
        self.values.as_ref().filter(|_| self.refused().is_none() && !self.hidden())
    }

    pub fn made(&self) -> usize {
        self.units
    }

    pub fn work(&self) -> u64 {
        self.work
    }

    pub fn fresh(&self) -> BaseCounters {
        self.spec.fresh()
    }

    fn send(&mut self, job: Job) {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_ok()
        {
            self.owed += 1;
        }
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
            (self.values, self.refused, self.units, self.work, self.worker_held) = (made.values, made.refused, made.made, made.work, made.held);
        }
        if self.stopped {
            self.values = None;
        }
        Ok(())
    }
}

impl Layer for BaseCountersFed {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn held_viewer(&self) -> Option<u32> {
        self.perspective().map(|(_, vision)| vision.ours())
    }

    fn name(&self) -> &'static str {
        "base_counters"
    }

    fn placed(&mut self, view: &viewer_layer_core::layers::View) -> Option<u64> {
        self.spec.placed(view)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        self.spec.inputs(view, inputs)
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = viewer_runtime::landed::channel();
            thread::Builder::new().name("base counters".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                run(receive, send)
            })?;
            (self.jobs, self.made) = (Some(jobs), Some(made));
        }
        let mut copies = Box::<Mirrors>::default();
        for table in TABLES {
            if let Some(mirror) = mirrors.get(table) {
                copies.insert(Box::new(mirror.clone()));
            }
        }
        self.spec.vision_new.store(false, Ordering::Release);
        let model = Box::new(self.fresh());
        self.epoch += 1;
        self.owed = 0;
        self.products.compose(mirrors.get(TableId::Provinces).and_then(Mirror::head));
        self.waiting.clear();
        self.send(Job::Reset { epoch: self.epoch, model, mirrors: copies });
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

    fn settle(&mut self, _mirrors: &Mirrors, settling: Settling) -> io::Result<()> {
        let whole = settling.waits();
        self.exchange(false)?;
        let applied = self.products.applied_hour();
        let mut steps = Vec::new();
        while self.waiting.front().is_some_and(|step| applied.is_some_and(|applied| step.last() <= applied)) {
            steps.extend(self.waiting.pop_front());
        }
        let given = self.spec.vision_new.swap(false, Ordering::AcqRel).then(|| self.spec.perspective()).flatten();
        if !steps.is_empty() || given.is_some() {
            self.send(Job::Frame { epoch: self.epoch, stamp: applied, steps, given });
        }
        self.exchange(whole)
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

    fn held(&self) -> usize {
        self.worker_held + self.waiting.capacity() * 8
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}
