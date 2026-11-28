use replay_data::Hour;
use std::{sync::Arc, time::Instant};
use viewer_foundation::perspective::{Choice, Vision};
use viewer_runtime::landed::{self, Landed};

#[derive(Default)]
pub struct ShownVision {
    held: Option<(VisionKey, Instant, Result<Landed<Arc<Vision>>, String>)>,
    making: Option<(VisionKey, Instant, u64)>,
    forgotten: u64,
    maker: Option<(std::sync::mpsc::Sender<crate::vision::SeenCopy>, landed::Receiver<Arc<Vision>>)>,
    progress: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone, Debug, PartialEq)]
struct VisionKey {
    applied: Option<Hour>,
    published: crate::layers::Published,
    carried: usize,
    choice: Choice,
}

impl ShownVision {
    pub(super) fn with_progress(progress: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { progress: Some(progress), ..Self::default() }
    }

    pub fn shown(&mut self, layer: Option<&crate::vision::Vision>, applied: Option<Hour>, parts_home: bool, exact: bool) -> Result<Landed<Arc<Vision>>, String> {
        use crate::layers::Layer as _;
        self.take_made();
        let layer = match (parts_home, layer) {
            (true, Some(layer)) => layer,
            (true, None) => return Err("the map draws no vision layer".into()),
            (false, _) => return self.kept(),
        };
        if layer.restarting() {
            return if exact { Err("the vision of this perspective is still being made".into()) } else { self.kept() };
        }
        let key = VisionKey { applied, published: layer.published(), carried: layer.carried().entities, choice: layer.choice().clone() };
        let now = Instant::now();
        let due = self.held.as_ref().is_none_or(|(held, ..)| *held != key);
        if due && self.making.is_none() {
            match layer.shown_copy() {
                Err(why) => self.held = Some((key.clone(), now, Err(why))),
                Ok(copy) => self.make(key.clone(), now, copy),
            }
        }
        match &self.held {
            Some((held, _, value)) if !exact || *held == key => value.clone(),
            Some((_, _, Err(why))) => Err(why.clone()),
            _ => Err("the vision of this step is still being copied out of the layer".into()),
        }
    }

    fn make(&mut self, key: VisionKey, at: Instant, copy: crate::vision::SeenCopy) {
        if self.maker.is_none() {
            let (jobs, receive) = std::sync::mpsc::channel::<crate::vision::SeenCopy>();
            let (send, made) = landed::channel();
            let progress = self.progress.clone();
            let started = std::thread::Builder::new().name("shown vision".into()).spawn(move || {
                viewer_runtime::worker_priority::background_priority();
                let mut before = None;
                for copy in receive {
                    let (seen, made) = copy.make_after(before.as_ref());
                    before = made;
                    if send.send(Arc::new(seen)).is_err() {
                        return;
                    }
                    if let Some(progress) = &progress {
                        progress();
                    }
                }
            });
            if started.is_ok() {
                self.maker = Some((jobs, made));
            }
        }
        match &self.maker {
            Some((jobs, _)) if jobs.send(copy).is_ok() => self.making = Some((key, at, self.forgotten)),
            _ => self.held = Some((key, at, Err("no thread to make the vision on".into()))),
        }
    }

    pub(super) fn take_made(&mut self) {
        if let Some(vision) = self.maker.as_ref().and_then(|(_, made)| made.try_recv().ok()) {
            self.install(vision);
        }
    }

    fn install(&mut self, vision: Landed<Arc<Vision>>) {
        if let Some((key, at, forgotten)) = self.making.take()
            && forgotten == self.forgotten
        {
            self.held = Some((key, at, Ok(vision)));
        }
    }

    pub fn kept(&self) -> Result<Landed<Arc<Vision>>, String> {
        self.held.as_ref().map_or_else(|| Err("the vision is being made".into()), |(_, _, held)| held.clone())
    }

    pub fn kept_for(&self, choice: &Choice) -> Result<Landed<Arc<Vision>>, String> {
        match &self.held {
            Some((key, _, held)) if key.choice == *choice => held.clone(),
            Some(_) => Err("the vision of this perspective is still being copied".into()),
            None => Err("the vision is being made".into()),
        }
    }

    pub fn forget(&mut self) {
        self.held = None;
        self.forgotten += 1;
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_support {
    use super::*;

    pub struct Making {
        pub shown: ShownVision,
        now: fn() -> Instant,
        sent: std::sync::mpsc::Receiver<crate::vision::SeenCopy>,
        made: landed::Sender<Arc<Vision>>,
    }

    impl Making {
        pub fn new(now: fn() -> Instant) -> Self {
            let (jobs, sent) = std::sync::mpsc::channel();
            let (made, receive) = landed::channel();
            Self { shown: ShownVision { maker: Some((jobs, receive)), ..ShownVision::default() }, now, sent, made }
        }

        pub fn ask(&mut self, choice: &Choice, hour: Hour, ours: u32) {
            let key = VisionKey { applied: Some(hour), published: crate::layers::Published::Current, carried: 0, choice: choice.clone() };
            let parts = viewer_foundation::perspective::Parts { ours, ..Default::default() };
            self.shown.make(key, (self.now)(), crate::vision::SeenCopy::of_parts(parts));
        }

        pub fn arrive(&mut self) {
            let copy = self.sent.try_recv().expect("a copy was sent to be made");
            self.made.send(Arc::new(copy.make())).unwrap();
            self.shown.take_made();
        }

        pub fn is_making(&self) -> bool {
            self.shown.making.is_some()
        }
    }
}
