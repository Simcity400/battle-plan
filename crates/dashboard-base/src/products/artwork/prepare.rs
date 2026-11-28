use crate::{
    Source, catalogue::{self, Catalogue},
    presentation::Cancel as _,
    products::{ContentOptions, LoadContext, read_ahead::Preparation},
};
use content_reader_layers::content::ResolvedContent;
use std::{io, marker::PhantomData, sync::{Arc, PoisonError, Weak, atomic::Ordering}};

struct ArtworkReadAhead<Scope>([Option<Held>; 2], PhantomData<fn() -> Scope>);

impl<Scope> Default for ArtworkReadAhead<Scope> {
    fn default() -> Self { Self([None, None], PhantomData) }
}

struct Held {
    content: ContentOptions,
    mount: Weak<ResolvedContent>,
    settings: Option<catalogue::Binding>,
    ready: Arc<Preparation>,
}

impl Held {
    fn matches(&self, content: &ContentOptions) -> bool {
        Arc::ptr_eq(self.content.presentation.binding(), content.presentation.binding())
            && self.content.allow_unverified == content.allow_unverified
            && self.content.options.game_dir == content.options.game_dir
            && self.content.options.user_dir == content.options.user_dir
            && self.content.options.cache_dir == content.options.cache_dir
    }
}

pub fn prepare_artwork(source: &Arc<Source>, load: &LoadContext) -> io::Result<Option<Arc<Preparation>>> {
    prepare::<()>(source, load, false, |_, catalogue| dashboard_text::catalogue::prepare_sprite_renderer(catalogue))
}

pub fn prepare_artwork_for<Scope: 'static>(
    source: &Arc<Source>, load: &LoadContext, artwork: fn(&Source, &Arc<Catalogue>),
) -> io::Result<Option<Arc<Preparation>>> {
    prepare::<Scope>(source, load, true, artwork)
}

fn prepare<Scope: 'static>(
    source: &Arc<Source>, load: &LoadContext, needs_settings: bool, artwork: fn(&Source, &Arc<Catalogue>),
) -> io::Result<Option<Arc<Preparation>>> {
    let Some(content) = &load.content else { return Ok(None) };
    let Some(mount) = source.bound_presented_for(content) else { return Ok(None) };
    let mount = Arc::downgrade(&mount.content);
    let settings = needs_settings.then(|| source.in_scope(|| catalogue::binding(content)));
    let state = source.state::<ArtworkReadAhead<Scope>>();
    let ready = {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = &mut state.0[usize::from(catalogue::reads_recorded(content))];
        if let Some(held) = slot.as_ref().filter(|held| held.matches(content) && held.mount.ptr_eq(&mount) && held.settings == settings) {
            return held.ready.pending();
        }
        let ready = Arc::<Preparation>::default();
        *slot = Some(Held { content: content.clone(), mount, settings, ready: Arc::clone(&ready) });
        ready
    };
    let source = Arc::downgrade(source);
    let content = content.clone();
    ready.start("dashboard artwork preparation", move || {
        let source = source.upgrade().ok_or(io::ErrorKind::Interrupted)?;
        let retired = content.presentation.binding().flag();
        if retired.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        source.in_scope(|| {
            if let Some(Ok(catalogue)) = catalogue::bound_catalogue(&source, &content)
                && !retired.load(Ordering::Acquire)
            {
                artwork(&source, &catalogue);
            }
        });
        if retired.load(Ordering::Acquire) { Err(io::ErrorKind::Interrupted.into()) } else { Ok(()) }
    });
    ready.pending()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{presentation::Presentation, source::Work, test_fixtures::visual_mods::Machine};
    use std::sync::atomic::AtomicBool;

    #[derive(Default)]
    struct Count(u32);

    #[test]
    fn preparation_survives_the_first_requests_cancellation() {
        let machine = Machine::new("artwork_preparation");
        let (source, content) = machine.open();
        let source = Arc::new(source);
        source.bind(Some((&content.options, content.allow_unverified)), Presentation::default());
        source.content(&content.options).unwrap();
        let load = |cancelled: bool| LoadContext {
            content: source.admit(Work::Content(Arc::new(AtomicBool::new(cancelled)))),
        };
        let prepare = |load: &LoadContext| prepare_artwork_for::<Count>(&source, load, |source, _| {
            source.state::<Count>().lock().unwrap().0 += 1;
        });
        if let Some(ready) = prepare(&load(true)).unwrap() { ready.wait().unwrap(); }
        assert!(prepare(&load(false)).unwrap().is_none());
        assert_eq!(source.state::<Count>().lock().unwrap().0, 1);
        std::fs::write(content.options.user_dir.join("settings.txt"), "language=l_english force_pow2_textures=yes").unwrap();
        if let Some(ready) = prepare(&load(false)).unwrap() { ready.wait().unwrap(); }
        assert_eq!(source.state::<Count>().lock().unwrap().0, 2);
    }
}
