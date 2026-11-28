
use super::{Columns, Sounds, TABLES};
use viewer_layer_core::{layers::{Carried, Retirement, RetirementFlag, Settling, Fed as Products, Inputs, Layer, Published, Scope, Scoped, Sight, View}, playhead_host::mirror::{Mirror, Mirrors}};
use playhead::Step;
use replay_data::Hour;
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, mpsc},
    thread,
};
use viewer_audio::battle::{self, Battles};
use viewer_runtime::landed::{self, Landed};

#[expect(clippy::large_enum_variant, reason = "frames are the channel's traffic and move whole into the worker; boxing them would allocate for every frame")]
enum Job {
    Reset { epoch: u64, model: Box<Sounds>, mirrors: Box<Mirrors> },
    Frame { epoch: u64, stamp: Option<Hour>, steps: Vec<Arc<Step>>, declared: Option<Option<Columns>> },
}

struct Made {
    epoch: u64,
    stamp: Option<Hour>,
    answers: usize,
    battles: Option<Arc<Battles>>,
    marked: [usize; 3],
    made: u64,
    scoped: Scoped,
    held: usize,
}

struct Work {
    model: Box<Sounds>,
    mirrors: Box<Mirrors>,
}

impl Work {
    fn take(&mut self, steps: Vec<Arc<Step>>, declared: Option<Option<Columns>>) {
        if let Some(columns) = declared {
            (self.model.columns, self.model.whole) = (columns, true);
        }
        for step in steps {
            self.mirrors.apply(&step);
            self.model.apply_step(&step);
        }
    }

    fn made(&mut self, epoch: u64, stamp: Option<Hour>, answers: usize) -> Made {
        let held = self.model.held() + self.mirrors.iter().map(Mirror::bytes).sum::<usize>();
        let battles = self.model.published.lock().unwrap_or_else(|p| p.into_inner()).as_ref().cloned();
        Made { epoch, stamp, answers, battles, marked: self.model.marked, made: self.model.made, scoped: std::mem::take(&mut self.model.scoped), held }
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
            Job::Reset { epoch: of, mut model, mirrors } => {
                epoch = of;
                let _ = model.compose(&mirrors);
                work.insert(Work { model, mirrors }).made(epoch, None, 1)
            }
            Job::Frame { epoch: of, .. } if of != epoch => continue,
            Job::Frame { stamp, steps, declared, .. } => {
                let Some(held) = &mut work else { continue };
                held.take(steps, declared);
                let (mut stamp, mut answers) = (stamp, 1);
                while let Ok(queued) = jobs.try_recv() {
                    match queued {
                        Job::Frame { epoch: of, stamp: at, steps, declared } if of == epoch => {
                            held.take(steps, declared);
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

pub struct SoundsFed {
    retiring: RetirementFlag,
    spec: Sounds,
    jobs: Option<mpsc::Sender<Job>>,
    made: Option<landed::Receiver<Made>>,
    epoch: u64,
    owed: usize,
    products: Products<Made>,
    waiting: VecDeque<Arc<Step>>,
    declared: bool,
    battles: Option<Landed<Option<Arc<Battles>>>>,
    marked: [usize; 3],
    made_battles: u64,
    scoped: Scoped,
    worker_held: usize,
    stopped: bool,
}

impl SoundsFed {
    pub fn new(sounds: Sounds) -> Self {
        Self {
            retiring: RetirementFlag::default(),
            spec: sounds,
            jobs: None,
            made: None,
            epoch: 0,
            owed: 0,
            products: Products::default(),
            waiting: VecDeque::new(),
            declared: false,
            battles: None,
            marked: [0; 3],
            made_battles: 0,
            scoped: Scoped::default(),
            worker_held: 0,
            stopped: false,
        }
    }

    pub fn battles(&self) -> Option<&Arc<Battles>> {
        self.battles.as_deref().and_then(Option::as_ref)
    }

    pub fn landed_battles(&self) -> Option<Landed<Option<Arc<Battles>>>> {
        self.battles.clone()
    }

    pub fn marked(&self) -> [usize; 3] {
        self.marked
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn made(&self) -> u64 {
        self.made_battles
    }

    pub fn fresh(&self) -> Sounds {
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
        self.products.take(|made| due.push(made));
        for made in due {
            self.battles = Some(made.as_ref().map(|made| made.battles.clone()));
            let made = made.into_inner();
            (self.marked, self.made_battles, self.worker_held) = (made.marked, made.made, made.held);
            self.scoped.extend(made.scoped);
        }
        if self.stopped {
            self.battles = None;
        }
        Ok(())
    }
}

impl Layer for SoundsFed {
    fn retirement(&self) -> Option<Retirement> {
        Some(self.retiring.handle())
    }

    fn name(&self) -> &'static str {
        "sounds"
    }

    fn placed(&mut self, view: &View) -> Option<u64> {
        self.spec.placed(view)
    }

    fn inputs(&mut self, view: &View, inputs: &mut Inputs<'_>) -> io::Result<()> {
        self.declared = true;
        self.spec.inputs(view, inputs)
    }

    fn compose(&mut self, mirrors: &Mirrors) -> io::Result<()> {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel();
            let (send, made) = landed::channel();
            thread::Builder::new().name("battle sounds".into()).spawn(move || {
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
        let mut model = Box::new(self.fresh());
        (model.mix_inline, model.marked) = (true, self.marked);
        self.declared = false;
        self.epoch += 1;
        self.owed = 0;
        self.products.compose(mirrors.iter().find_map(Mirror::head));
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
        let declared = std::mem::take(&mut self.declared).then_some(self.spec.columns);
        if !steps.is_empty() || declared.is_some() {
            self.send(Job::Frame { epoch: self.epoch, stamp: applied, steps, declared });
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

    fn scope(&mut self, sight: &Sight<'_>) -> Option<Scope> {
        let hearing = self.spec.audio.as_deref().and_then(battle::hearing);
        Some(self.scoped.take(sight, hearing))
    }

    fn held(&self) -> usize {
        self.worker_held + self.waiting.capacity() * 8 + self.scoped.bytes()
    }

    fn any(&self) -> &dyn std::any::Any {
        self
    }
}
