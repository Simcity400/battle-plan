
pub use dashboard_text::catalogue::*;

use crate::{
    Source, content,
    presentation::{Presentation, Presented},
    products::ContentOptions,
};
use content_reader_layers::content::ResolvedContent;
use content_reader_localisation::localisation;
use dashboard_text::scripted_text::ScriptedContent;
use kept::Keeping;
use std::{io, sync::{Arc, Weak}};

fn load(
    source: &Source,
    options: &ContentOptions,
    settings: &Settings,
    presented: Arc<Presented>,
) -> io::Result<Catalogue> {
    load_with(
        source,
        options,
        settings,
        || Ok(presented),
        Keeping::On,
    )
}

trait Slot: Default + Send + 'static {
    fn slot(&mut self) -> &mut Cached;
}

impl Slot for Cached {
    fn slot(&mut self) -> &mut Cached {
        self
    }
}

#[derive(Default)]
struct Recorded(Cached);

impl Slot for Recorded {
    fn slot(&mut self) -> &mut Cached {
        &mut self.0
    }
}

pub fn reads_recorded(options: &ContentOptions) -> bool {
    options.presentation.is_recorded() || options.presentation.identity().is_empty()
}

fn with_slot<R>(
    source: &Source,
    options: &ContentOptions,
    change: impl FnOnce(&mut Cached) -> R,
) -> R {
    if reads_recorded(options) {
        change(
            source
                .state::<Recorded>()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .slot(),
        )
    } else {
        change(
            &mut source
                .state::<Cached>()
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

fn kept_in<S: Slot>(
    key: Binding,
    source: &Source,
    options: &ContentOptions,
    presented: impl FnOnce() -> io::Result<Arc<Presented>>,
) -> io::Result<Arc<Catalogue>> {
    let settings = key.3.clone();
    let presented = presented()?;
    let mount = Arc::downgrade(&presented.content);
    let stack_key = presented.content.layers.art_stack_key();
    let result = {
        let state = source.state::<S>();
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        let slot = state.slot();
        let matches = slot.as_ref().is_some_and(|(cached, existing, previous)| {
            *cached == key && previous.ptr_eq(&mount)
                && existing
                    .get()
                    .and_then(Result::ok)
                    .is_none_or(|cat| cat.content.layers.art_stack_key() == stack_key)
        });
        if !matches {
            *slot = Some((key, Arc::new(CachedResult::default()), mount));
        }
        Arc::clone(&slot.as_ref().expect("catalogue binding retained").1)
    };
    if let Some(loaded) = result.get() {
        return loaded;
    }
    result.get_or_init(|| load(source, options, &settings, presented))
}

fn recorded_slot(source: &Source, options: &ContentOptions) -> io::Result<Arc<Catalogue>> {
    kept_in::<Recorded>(
        binding_with(options, Presentation::default()),
        source,
        options,
        || source.recorded_presented(&options.options),
    )
}

fn load_with(
    source: &Source,
    options: &ContentOptions,
    settings: &Settings,
    presented: impl FnOnce() -> io::Result<Arc<Presented>>,
    keeping: Keeping,
) -> io::Result<Catalogue> {
    let mut verification = Vec::new();
    let replay = source.replay();
    content::check_replay(&replay, options.allow_unverified, &mut verification)?;
    let recorded_language = content::recorded_language(&replay);
    let presented = presented()?;
    let warnings = Vec::new();
    let layers = &presented.content.layers;
    let cache = &options.options.cache_dir;
    let localisation = match keeping {
        Keeping::On => content::localisation_for(
            source, &options.options, options.allow_unverified, &presented.content,
        ),
        Keeping::Off => localisation::read_cached(layers, &recorded_language, cache).map(Arc::new),
    }?;
    let gui_decode = settings
        .as_ref()
        .map(|&force_pow2_textures| content_reader_image::native::Decode {
            force_pow2_textures,
        })
        .map_err(|why| format!("settings.txt is unavailable: {why}"));
    let read_sprites = || {
        presented
            .content
            .layers
            .artwork("interface sprites", content_reader_gfx::gfx::read)
    };
    let sprites = match keeping {
        Keeping::On if presented.presentation.is_empty() => {
            let stack_key = presented.content.layers.art_stack_key();
            let (sprites, noted) = kept::kept(
                kept::place::<content_reader_gfx::gfx::SpriteSet>(
                    source,
                    &options.options.cache_dir,
                    &stack_key,
                ),
                || content_reader_layers::content::diagnostics::capture(read_sprites),
            );
            noted.credit();
            sprites
        }
        _ => read_sprites(),
    };
    Ok(Catalogue::from_parts(
        Arc::clone(&presented.content),
        presented.presentation.clone(),
        localisation,
        sprites?,
        verification,
        warnings,
        recorded_language,
        gui_decode,
        Some(dashboard_effects::effect_describer),
    ))
}

pub fn read_scripted_content(
    content: Arc<content_reader_layers::content::ResolvedContent>,
    recorded_language: String,
) -> Result<ScriptedContent, String> {
    ScriptedContent::read(content, recorded_language)
        .map(|scripted| scripted.with_effect_describer(dashboard_effects::effect_describer))
}

pub fn built_catalogue(source: &Source, options: &ContentOptions) -> io::Result<Catalogue> {
    load_with(
        source,
        options,
        &settings(options),
        || source.recorded_presented(&options.options),
        Keeping::Off,
    )
}

pub type Binding = (
    content::ContentKey,
    std::path::PathBuf,
    Presentation,
    Settings,
);
type Cached = Option<(Binding, Arc<CachedResult<Catalogue>>, Weak<ResolvedContent>)>;

pub type Settings = Result<bool, String>;

fn settings(options: &ContentOptions) -> Settings {
    content_reader_settings::settings::read_keys(&options.options.user_dir)
        .map(|keys| keys.force_pow2_textures())
        .map_err(|why| why.to_string())
}

pub fn binding(options: &ContentOptions) -> Binding {
    binding_with(options, options.presentation.identity().clone())
}

fn binding_with(options: &ContentOptions, presentation: Presentation) -> Binding {
    (
        content::content_key(Some(options)).expect("content options given"),
        options.options.cache_dir.clone(),
        presentation,
        settings(options),
    )
}

pub type RecordedBinding = (content::ContentKey, std::path::PathBuf);

pub fn recorded_binding(options: &ContentOptions) -> RecordedBinding {
    (
        content::content_key(Some(options)).expect("content options given"),
        options.options.cache_dir.clone(),
    )
}

pub fn binding_matches(key: &Binding, options: &ContentOptions) -> bool {
    *key == binding(options)
}

pub fn catalogue(source: &Source, options: &ContentOptions) -> io::Result<Arc<Catalogue>> {
    if reads_recorded(options) {
        return recorded_slot(source, options);
    }
    let shown = if source.still_admitted(options) {
        kept_in::<Cached>(binding(options), source, options, || {
            source.presented_for(options)
        })?
    } else {
        Arc::new(load(
            source,
            options,
            &settings(options),
            source.presented_for(options)?,
        )?)
    };
    keeping_recorded(source, options, shown)
}

fn keeping_recorded(
    source: &Source,
    options: &ContentOptions,
    shown: Arc<Catalogue>,
) -> io::Result<Arc<Catalogue>> {
    if shown.recorded().is_none() {
        shown.keep_recorded(recorded_slot(source, options)?);
    }
    Ok(shown)
}

pub fn recorded_catalogue(source: &Source, options: &ContentOptions) -> io::Result<Arc<Catalogue>> {
    if options.presentation.is_empty() {
        return catalogue(source, options);
    }
    recorded_slot(source, options)
}

pub fn present<T, E: std::fmt::Display>(
    what: &str,
    admitted: &Catalogue,
    recorded: &Catalogue,
    draw: impl Fn(&Catalogue) -> Result<T, E>,
) -> Drawn<Result<T, E>> {
    #[cfg(any(test, feature = "test-support"))]
    PRESENTED.with(|count| count.set(count.get() + 1));
    Drawn::new(|| {
        if std::ptr::eq(admitted, recorded) || content_reader_layers::content::is_strict() {
            return draw(admitted);
        }
        let (shown, noted) =
            capture_isolated(|| content_reader_layers::content::strict(|| draw(admitted)));
        match shown {
            Ok(value) => {
                credit(&noted);
                Ok(value)
            }
            Err(reason) => {
                let drawn = draw(recorded);
                if drawn.is_ok() {
                    content_reader_layers::content::credit_overlay_refusals(
                        &std::collections::BTreeSet::from([format!("{what} ({reason})")]),
                    );
                }
                drawn
            }
        }
    })
}

pub fn draw<T, E: std::fmt::Display>(
    what: impl FnOnce() -> String,
    shown: &Catalogue,
    operation: impl Fn(&Catalogue) -> Result<T, E>,
) -> Drawn<Result<T, E>> {
    match shown.recorded() {
        Some(recorded) => present(&what(), shown, recorded, operation),
        None => Drawn::new(|| operation(shown)),
    }
}

#[cfg(test)]
mod present_tests {
    use super::*;
    use crate::test_fixtures::visual_mods::{Machine, RED, fallback_line, png, select};

    #[test]
    fn present_is_one_transaction() {
        let machine = Machine::new("present_transaction");
        machine.write("gfx/icon.png", &png(RED));
        machine.write_mod("art", "gfx/icon.png", &png(RED));
        let (source, mut options) = machine.open();
        select(&source, &mut options, &["art"]);
        let admitted = catalogue(&source, &options).unwrap();
        let recorded = recorded_catalogue(&source, &options).unwrap();
        let note = |text: &str| {
            content_reader_layers::content::credit_overlay_refusals(&std::collections::BTreeSet::from([
                text.to_owned(),
            ]))
        };
        let (drawn, line) = fallback_line(|| {
            present("the icon", &admitted, &recorded, |cat| {
                if std::ptr::eq(cat, &raw const *admitted) {
                    assert!(content_reader_layers::content::is_strict());
                    note("discarded selector note");
                    Err("the mod's sprite does not load".to_owned())
                } else {
                    assert!(!content_reader_layers::content::is_strict());
                    Ok::<_, String>(7)
                }
            })
        });
        assert_eq!(*drawn.get(), Ok::<_, String>(7));
        for line in [line, fallback_line(|| drawn.get().clone()).1] {
            let line = line.unwrap();
            assert!(
                line.contains("the icon (the mod's sprite does not load)"),
                "{line}"
            );
            assert!(!line.contains("discarded"), "{line}");
        }
        let (drawn, line) = fallback_line(|| {
            present("the icon", &admitted, &recorded, |_| {
                note("kept selector note");
                Ok::<_, String>(8)
            })
        });
        assert_eq!(*drawn.get(), Ok::<_, String>(8));
        assert!(line.is_some_and(|line| line.contains("kept selector note")));
    }

    #[test]
    fn draw_falls_back_through_the_recorded_catalogue_the_shown_one_keeps() {
        let machine = Machine::new("draw_keeps_recorded");
        machine.write("gfx/icon.png", &png(RED));
        machine.write_mod("art", "gfx/icon.png", &png(RED));
        let (source, mut options) = machine.open();
        select(&source, &mut options, &[]);
        let plain = catalogue(&source, &options).unwrap();
        assert!(plain.recorded().is_none());
        let attempts = std::cell::Cell::new(0);
        let (drawn, line) = fallback_line(|| {
            draw(
                || unreachable!("nothing is named"),
                &plain,
                |cat| {
                    attempts.set(attempts.get() + 1);
                    assert!(!content_reader_layers::content::is_strict());
                    assert!(std::ptr::eq(cat, &raw const *plain));
                    Ok::<_, String>(1)
                },
            )
        });
        assert_eq!(
            (drawn.get().clone(), attempts.get(), line),
            (Ok::<_, String>(1), 1, None)
        );
        select(&source, &mut options, &["art"]);
        let shown = catalogue(&source, &options).unwrap();
        let recorded = shown
            .recorded()
            .expect("the recording's own catalogue kept");
        assert!(Arc::ptr_eq(
            recorded,
            &recorded_catalogue(&source, &options).unwrap()
        ));
        let (drawn, line) = fallback_line(|| {
            draw(
                || "the icon".to_owned(),
                &shown,
                |cat| {
                    if std::ptr::eq(cat, &raw const *shown) {
                        Err("the mod's sprite does not load".to_owned())
                    } else {
                        Ok::<_, String>(2)
                    }
                },
            )
        });
        assert_eq!(*drawn.get(), Ok::<_, String>(2));
        assert!(
            line.is_some_and(|line| line.contains("the icon (the mod's sprite does not load)"))
        );
    }

    #[test]
    fn a_nested_transaction_leaves_falling_back_to_the_outer_one() {
        let machine = Machine::new("nested_present");
        machine.write("gfx/icon.png", &png(RED));
        machine.write_mod("art", "gfx/icon.png", &png(RED));
        let (source, mut options) = machine.open();
        select(&source, &mut options, &["art"]);
        let shown = catalogue(&source, &options).unwrap();
        let inner = |cat: &Catalogue| {
            draw(
                || "the inner icon".to_owned(),
                cat,
                |drawing| {
                    if std::ptr::eq(drawing, &raw const *shown) {
                        assert!(content_reader_layers::content::is_strict());
                        Err("the mod's inner sprite does not load".to_owned())
                    } else {
                        Ok::<_, String>("recorded inner")
                    }
                },
            )
            .get()
            .clone()
        };
        let (drawn, line) = fallback_line(|| {
            draw(
                || "the outer icon".to_owned(),
                &shown,
                |cat| {
                    let inner = inner(cat)?;
                    Ok::<_, String>(if std::ptr::eq(cat, &raw const *shown) {
                        "shown"
                    } else {
                        inner
                    })
                },
            )
        });
        assert_eq!(
            *drawn.get(),
            Ok::<_, String>("recorded inner"),
            "the outer operation's fallback"
        );
        let line = line.unwrap();
        assert!(
            line.contains("the outer icon (the mod's inner sprite does not load)"),
            "{line}"
        );
        assert!(!line.contains("the inner icon"), "{line}");
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PRESENTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn presented() -> usize {
    PRESENTED.with(std::cell::Cell::get)
}

#[derive(Default)]
pub struct ByCatalogue<T> {
    recorded: T,
    shown: T,
}

impl<T> ByCatalogue<T> {
    pub fn of(&mut self, catalogue: &Catalogue) -> &mut T {
        self.of_optional(Some(catalogue))
    }

    pub fn of_optional(&mut self, catalogue: Option<&Catalogue>) -> &mut T {
        if catalogue.is_none_or(|catalogue| catalogue.presentation.is_empty()) {
            &mut self.recorded
        } else {
            &mut self.shown
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn both(&self) -> [&T; 2] {
        [&self.recorded, &self.shown]
    }
}

pub fn bound_catalogue(
    source: &Source,
    options: &ContentOptions,
) -> Option<io::Result<Arc<Catalogue>>> {
    bound_catalogue_with(source, options, || {})
}

pub(crate) fn bound_catalogue_with(
    source: &Source,
    options: &ContentOptions,
    before_load: impl FnOnce(),
) -> Option<io::Result<Arc<Catalogue>>> {
    let presented = source.bound_presented_for(options)?;
    let key = binding(options);
    let settings = key.3.clone();
    let mount = Arc::downgrade(&presented.content);
    let result = with_slot(source, options, |slot| {
        if !source.bound_presented_for(options).is_some_and(|current| mount.ptr_eq(&Arc::downgrade(&current.content))) {
            return None;
        }
        Some(match slot.as_ref() {
            Some((cached, result, previous)) if *cached == key && previous.ptr_eq(&mount) => Arc::clone(result),
            _ => {
                let result = Arc::new(CachedResult::default());
                *slot = Some((key, Arc::clone(&result), mount));
                result
            }
        })
    })?;
    before_load();
    let loaded = result.get_or_init(|| {
        load_with(
            source,
            options,
            &settings,
            || Ok(Arc::clone(&presented)),
            Keeping::On,
        )
    });
    let shown = match loaded { Ok(shown) => shown, Err(error) => return Some(Err(error)) };
    if !reads_recorded(options) && shown.recorded().is_none() {
        let mut recorded = options.clone();
        recorded.presentation = crate::presentation::Admitted::recorded(Arc::clone(options.presentation.binding()));
        match bound_catalogue(source, &recorded)? {
            Ok(recorded) => shown.keep_recorded(recorded),
            Err(error) => return Some(Err(error)),
        }
    }
    Some(Ok(shown))
}

#[cfg(any(test, feature = "test-support"))]
pub fn catalogue_loads(source: &Source, options: &ContentOptions) -> usize {
    with_slot(source, options, |slot| {
        slot.as_ref()
            .filter(|(key, _, _)| binding_matches(key, options))
            .map_or(0, |(_, result, _)| result.loads())
    })
}

pub mod kept;

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod loader_tests {
    use super::*;

    #[test]
    fn text_never_reads_settings_and_unreadable_settings_refuse_only_art() {
        use crate::test_fixtures::{records, source};
        use replay_capture_spool::spool::Record;
        use std::sync::atomic::AtomicBool;
        let (_, folder) = crate::catalogue::tests::folder_catalogue(&[
            ("hoi4.exe", "test"),
            (
                "user/settings.txt",
                "language=l_english force_pow2_textures=no",
            ),
            (
                "user/dlc_load.json",
                r#"{"enabled_mods":[],"disabled_dlcs":[]}"#,
            ),
            (
                "localisation/a_l_english.yml",
                "\u{feff}l_english:\n NAME:0 \"Recorded name\"\n",
            ),
            (
                "localisation/languages.yml",
                "\u{feff}l_english:\n l_english: \"English\"\n",
            ),
        ]);
        let (options, identity) = crate::test_fixtures::content_identity(&folder.0);
        let mut input = records(1, vec![source(1, 100, Some(1))]);
        let Record::SessionStart(session) = &mut input[0] else {
            unreachable!()
        };
        session.identity = identity.clone();
        let replay = Arc::new(crate::test_fixtures::with_facts(&options, &identity, input));
        let settings = options.options.user_dir.join("settings.txt");
        std::fs::remove_file(&settings).unwrap();
        for unreadable in [false, true] {
            if unreadable {
                std::fs::create_dir(&settings).unwrap();
            }
            let source = crate::Source::from_replay(
                crate::SourceKey {
                    source_id: "settings-isolation".into(),
                    revision: unreadable.to_string(),
                },
                "memory.battleplan".into(),
                Arc::clone(&replay),
                &AtomicBool::new(false),
            )
            .unwrap();
            let catalogue = crate::catalogue::catalogue(&source, &options).unwrap();
            assert_eq!(
                catalogue
                    .localised("NAME", crate::catalogue::View::Plain)
                    .unwrap(),
                "Recorded name"
            );
            assert_eq!(catalogue.localisation.language, "english");
            let localisation = crate::content::localisation(&source, &options).unwrap();
            assert_eq!(localisation.get("NAME"), Some("Recorded name"));
            if unreadable {
                let why = catalogue.gui_decode.as_ref().unwrap_err();
                assert!(why.contains("settings.txt"), "{why}");
            } else {
                assert!(
                    catalogue
                        .gui_decode
                        .as_ref()
                        .is_ok_and(|decode| !decode.force_pow2_textures)
                );
            }
            let cached = crate::catalogue::catalogue(&source, &options).unwrap();
            assert!(Arc::ptr_eq(&catalogue, &cached));
            if unreadable {
                std::fs::remove_dir(&settings).unwrap();
            }
            std::fs::write(&settings, "language=l_french force_pow2_textures=yes").unwrap();
            let changed = crate::catalogue::catalogue(&source, &options).unwrap();
            assert!(
                changed
                    .gui_decode
                    .as_ref()
                    .is_ok_and(|decode| decode.force_pow2_textures)
            );
            assert_eq!(changed.localisation.language, "english");
            std::fs::remove_file(&settings).unwrap();
        }
    }
    #[test]
    fn a_read_ahead_never_rebinds_content_a_request_replaced() {
        use crate::test_fixtures::{records, source};
        use replay_capture_spool::spool::Record;
        use std::sync::{atomic::AtomicBool, mpsc};
        let files: &[(&str, &str)] = &[
            ("hoi4.exe", "test"),
            (
                "user/settings.txt",
                "language=l_english force_pow2_textures=no",
            ),
            (
                "user/dlc_load.json",
                r#"{"enabled_mods":[],"disabled_dlcs":[]}"#,
            ),
            (
                "localisation/a_l_english.yml",
                "\u{feff}l_english:\n NAME:0 \"Recorded name\"\n",
            ),
        ];
        let (_, old_folder) = crate::catalogue::tests::folder_catalogue(files);
        let (_, new_folder) = crate::catalogue::tests::folder_catalogue(files);
        let (old, identity) = crate::test_fixtures::content_identity(&old_folder.0);
        let (new, _) = crate::test_fixtures::content_identity(&new_folder.0);
        let mut input = records(1, vec![source(1, 100, Some(1))]);
        let Record::SessionStart(session) = &mut input[0] else {
            unreachable!()
        };
        session.identity = identity.clone();
        let source = crate::Source::from_replay(
            crate::SourceKey {
                source_id: "read-ahead".into(),
                revision: "1".into(),
            },
            "memory.battleplan".into(),
            Arc::new(crate::test_fixtures::with_facts(&old, &identity, input)),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(bound_catalogue(&source, &old).is_none());
        assert!(source.bound_content(&old.options).is_none());
        source.content(&old.options).unwrap();
        let (requested, warnings_before) = std::thread::scope(|scope| {
            let (entered, has_entered) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            let (source, old) = (&source, &old);
            let ahead = job_priority::spawn_in(scope, move || {
                bound_catalogue_with(source, old, || {
                    entered.send(()).unwrap();
                    let _ = released.recv();
                })
            });
            has_entered
                .recv()
                .expect("the read-ahead reached its catalogue load");
            let requested = catalogue(source, &new).unwrap();
            let mut warnings = Vec::new();
            source.append_content_warnings(&mut warnings);
            drop(release);
            ahead.join().unwrap().unwrap().unwrap();
            (requested, warnings)
        });
        assert!(source.bound_content(&new.options).is_some());
        assert!(
            source.bound_content(&old.options).is_none(),
            "the old content is not bound back"
        );
        assert!(
            Arc::ptr_eq(&requested, &catalogue(&source, &new).unwrap()),
            "requests keep the newer catalogue"
        );
        let mut warnings = Vec::new();
        source.append_content_warnings(&mut warnings);
        assert_eq!(
            warnings, warnings_before,
            "replies report the newer content's reads"
        );
        assert!(bound_catalogue(&source, &old).is_none());
    }
}

pub fn equipment_units(catalogue: &Catalogue) -> Result<Arc<content_reader_key_names::equipment::Database>, String> {
    catalogue
        .content
        .layers
        .derived::<CachedResult<content_reader_key_names::equipment::Database>>()
        .get_or_init_shared(|| content_reader_key_names::equipment::Database::shared_complete(&catalogue.content.layers))
        .map_err(|error| error.to_string())
}
