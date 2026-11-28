use dashboard_effects::ScriptedEffects;
use job_priority::SpawnCarried as _;
use super::{ContentOptions, LoadContext, politics_content};
use crate::{
    catalogue::{self, Catalogue},
    source::Source,
};
use std::{io, sync::{Arc, Mutex, OnceLock, PoisonError, Weak, atomic::Ordering}};

pub fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new().name(name.into()).spawn_carried(move || job_priority::confined(work)).map(drop)
}

pub type Resume = Box<dyn FnOnce(io::Result<()>) + Send>;

#[derive(Default)]
pub struct Preparation {
    result: OnceLock<io::Result<()>>,
    waiting: Mutex<Vec<Resume>>,
}

impl Preparation {
    pub fn start(self: &Arc<Self>, name: &str, build: impl FnOnce() -> io::Result<()> + Send + 'static) {
        let pending = Arc::clone(self);
        if let Err(error) = spawn(name, move || {
            job_priority::background_thread();
            pending.run(build);
        }) {
            self.publish(Err(error));
        }
    }
    pub fn run(&self, build: impl FnOnce() -> io::Result<()>) {
        self.publish(std::panic::catch_unwind(std::panic::AssertUnwindSafe(build))
            .unwrap_or_else(|_| Err(io::Error::other("Background preparation failed"))));
    }
    pub fn publish(&self, result: io::Result<()>) {
        if self.result.set(result).is_err() { return; }
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap_or_else(PoisonError::into_inner));
        for resume in waiting { resume(self.wait()); }
    }
    pub fn result(&self) -> Option<io::Result<()>> {
        self.result.get().map(|result| result.as_ref().copied().map_err(dashboard_core::duplicate))
    }
    pub fn pending(self: &Arc<Self>) -> io::Result<Option<Arc<Self>>> {
        self.result().map_or_else(|| Ok(Some(Arc::clone(self))), |result| result.map(|()| None))
    }
    pub fn wait(&self) -> io::Result<()> {
        self.result.wait().as_ref().copied().map_err(dashboard_core::duplicate)
    }
    pub fn on_ready(&self, resume: Resume) {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(result) = self.result() {
            drop(waiting);
            resume(result);
        } else {
            waiting.push(resume);
        }
    }
}

pub type Prepare<P> = fn(&Arc<Source>, &P, &LoadContext) -> io::Result<Option<Arc<Preparation>>>;

pub fn together(first: Option<Arc<Preparation>>, second: Option<Arc<Preparation>>) -> io::Result<Option<Arc<Preparation>>> {
    match (first, second) {
        (Some(first), Some(second)) => {
            let ready = Arc::<Preparation>::default();
            let publish = Arc::clone(&ready);
            first.on_ready(Box::new(move |result| match result {
                Ok(()) => second.on_ready(Box::new(move |result| publish.publish(result))),
                Err(error) => publish.publish(Err(error)),
            }));
            ready.pending()
        }
        (first, second) => Ok(first.or(second)),
    }
}

#[derive(Default)]
struct ReadAhead(bool);

impl ReadAhead {
    fn start(&mut self, spawn: impl FnOnce() -> std::io::Result<()>) {
        if !self.0 {
            self.0 = spawn().is_ok();
        }
    }
}

pub fn warm_in_background(source: &Arc<Source>, options: ContentOptions) {
    let state = source.state::<ReadAhead>();
    state.lock().unwrap_or_else(PoisonError::into_inner).start(|| {
        let source = Arc::downgrade(source);
        std::thread::Builder::new()
            .name("page content read-ahead".into())
            .spawn_carried(move || job_priority::confined(|| read(&source, options)))
            .map(drop)
    });
}

fn read(source: &Weak<Source>, mut options: ContentOptions) {
    let retired = |options: &ContentOptions| options.presentation.cancel().load(Ordering::Acquire);
    if retired(&options) {
        return;
    }
    if let Some(source) = source.upgrade() {
        source.resolve_bound(&mut options);
        let _ = super::artwork::prepare_artwork(&source, &LoadContext { content: Some(options.clone()) });
    }
    let steps: [fn(&Source, &ContentOptions, Arc<Catalogue>); 5] = [
        |_, _, catalogue| warm_guis(&catalogue),
        |source, _, catalogue| drop(politics_content::warm(source, catalogue)),
        |_, _, catalogue| {
            if let Ok(content) = catalogue.scripted_content() {
                drop(content.effects());
            }
        },
        |_, _, catalogue| catalogue.read_readers(),
        |source, _, catalogue| {
            if let Some(Ok(content)) = politics_content::warm(source, catalogue) {
                source.in_scope(|| drop(content.power_content()));
            }
        },
    ];
    for step in steps {
        let Some(source) = source.upgrade() else {
            return;
        };
        if retired(&options) {
            return;
        }
        let Some(Ok(catalogue)) = catalogue::bound_catalogue(&source, &options) else {
            return;
        };
        if retired(&options) {
            return;
        }
        step(&source, &options, catalogue);
    }
}

pub(crate) fn warm_guis(catalogue: &Catalogue) {
    if catalogue.content.layers.presentation().shows_visual_mods() {
        content_reader_layers::content::strict(|| drop(catalogue.guis()));
    } else {
        drop(catalogue.guis());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{presentation::Presentation, source::Work};

    fn opened() -> (Arc<Source>, ContentOptions, catalogue::tests::Folder) {
        opened_with(&[])
    }

    fn opened_with(
        files: &[(&str, &str)],
    ) -> (Arc<Source>, ContentOptions, catalogue::tests::Folder) {
        let mut all = vec![
            (
                "common/defines/work.lua",
                "NDefines={NCharacter={OFFICER_CORP_ADVISOR_ENTRIES_IN_MENU={}},\
                 NGame={MAX_SCRIPTED_LOC_RECURSION=23}}",
            ),
            ("common/ideas/work.txt", "ideas={country={mod_a={}}}"),
            ("common/buildings/work.txt", "buildings={mod_building={}}"),
            ("common/terrain/work.txt", "categories={mod_terrain={}}"),
        ];
        all.extend_from_slice(files);
        let (_catalogue, folder) = catalogue::tests::folder_catalogue(&all);
        let frame = crate::test_fixtures::source(1, 100, Some(1));
        let (source, context) = crate::test_fixtures::source_with_content(vec![frame], &folder.0);
        let source = Arc::new(source);
        let content = context.content.unwrap();
        source.bind(
            Some((&content.options, content.allow_unverified)),
            Presentation::default(),
        );
        let options = source.admit(Work::Ahead).unwrap();
        source.content(&options.options).unwrap();
        (source, options, folder)
    }

    #[test]
    fn a_failed_launch_leaves_read_ahead_available_for_retry() {
        let mut state = ReadAhead::default();
        state.start(|| Err(std::io::Error::other("cannot spawn")));
        let mut launches = 0;
        for _ in 0..2 {
            state.start(|| { launches += 1; Ok(()) });
        }
        assert_eq!(launches, 1);
    }

    #[test]
    fn a_reply_with_two_preparations_waits_for_both() {
        let graphics = Arc::<Preparation>::default();
        let names = Arc::<Preparation>::default();
        let ready = together(Some(graphics.clone()), Some(names.clone())).unwrap().unwrap();
        names.publish(Ok(()));
        assert!(ready.result().is_none());
        graphics.publish(Ok(()));
        assert!(ready.result().unwrap().is_ok());
    }

    #[test]
    fn a_failed_preparation_wakes_once_and_is_not_pending_again() {
        let ready = Arc::<Preparation>::default();
        ready.start("failed artwork preparation", || Err(io::Error::other("renderer unavailable")));
        ready.wait().unwrap_err();
        assert!(matches!(ready.pending(), Err(error) if error.to_string() == "renderer unavailable"));
    }

    #[test]
    fn an_opened_sources_page_content_is_read_ahead_for_its_products() {
        let (source, options, _folder) = opened();
        read(&Arc::downgrade(&source), options.clone());
        let catalogue = catalogue::bound_catalogue(&source, &options)
            .unwrap()
            .unwrap();
        let warmed = politics_content::held(&source, &catalogue).expect("read ahead");
        let read = politics_content::politics_content(&source, &options).unwrap();
        assert!(Arc::ptr_eq(&warmed, &read));
        assert!(read.power_content().is_ok());
        assert_eq!(politics_content::content_loads(&source), 2);
        assert!(catalogue.guis_loaded());
        assert!(catalogue.scripted_content().unwrap().effects_read());
        assert!(catalogue.readers_read());
    }

}
