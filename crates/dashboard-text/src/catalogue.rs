use content_reader_politics::faction_templates::FactionTemplateProducts as _;

use crate::scripted_text::EffectDescriberFactory;
use crate::{artwork::png_url, invalid};
use content_reader_defines::defines::Defines;
use dashboard_core::{
    findings::{Finding, Kind},
    presentation::Presentation,
};
use content_reader_layers::content::diagnostics::{self, Diagnostics, Once};
use content_reader_layers::{ResolvedLayers, content::ResolvedContent};
use content_reader_gui::gui::native::{resources, sprites::Plain};
use content_reader_gfx::gfx::SpriteSet;
use content_reader_image::{Rgba8Image, native::Decode};
use content_reader_localisation::{localisation::Argument, localisation::Localisation};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
pub struct ContentError(Arc<io::Error>);
impl From<io::Error> for ContentError {
    fn from(error: io::Error) -> Self {
        Self(Arc::new(error))
    }
}
impl ContentError {
    pub fn error(&self) -> io::Error {
        io::Error::new(self.0.kind(), Arc::clone(&self.0))
    }
}

fn is_kept_error(error: &io::Error) -> bool {
    content_reader_parser::fields::is_content_error(error)
        || dashboard_core::refusal(error).is_some()
}

pub struct CachedResult<T> {
    value: Mutex<Option<Result<Arc<T>, ContentError>>>,
    noted: Mutex<OwnRefusals>,
    #[cfg(any(test, feature = "test-support"))]
    loads: std::sync::atomic::AtomicUsize,
}
impl<T> Default for CachedResult<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            noted: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            loads: Default::default(),
        }
    }
}
impl<T> CachedResult<T> {
    pub fn get(&self) -> Option<io::Result<Arc<T>>> {
        let value = self.value.lock().unwrap_or_else(|error| error.into_inner());
        let result = value.as_ref()?;
        credit(&self.noted.lock().unwrap_or_else(|error| error.into_inner()));
        Some(result.as_ref().map(Arc::clone).map_err(ContentError::error))
    }

    pub fn get_or_init(&self, read: impl FnOnce() -> io::Result<T>) -> io::Result<Arc<T>> {
        self.get_or_init_shared(|| read().map(Arc::new))
    }

    pub fn get_or_init_shared(&self, read: impl FnOnce() -> io::Result<Arc<T>>) -> io::Result<Arc<T>> {
        let mut value = self.value.lock().unwrap_or_else(|error| error.into_inner());
        if value.is_none() {
            #[cfg(any(test, feature = "test-support"))]
            self.loads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (result, noted) = capture(read);
            match result {
                Ok(loaded) => {
                    *self.noted.lock().unwrap_or_else(|error| error.into_inner()) = noted;
                    *value = Some(Ok(loaded));
                }
                Err(error) if is_kept_error(&error) => {
                    *self.noted.lock().unwrap_or_else(|error| error.into_inner()) = noted;
                    *value = Some(Err(ContentError::from(error)));
                }
                Err(error) => return Err(error),
            }
        } else {
            credit(&self.noted.lock().unwrap_or_else(|error| error.into_inner()));
        }
        value
            .as_ref()
            .expect("database result retained")
            .as_ref()
            .map(Arc::clone)
            .map_err(ContentError::error)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub struct KeyedResults<K, T> {
    values: Mutex<BTreeMap<K, Arc<CachedResult<T>>>>,
}
impl<K, T> Default for KeyedResults<K, T> {
    fn default() -> Self {
        Self {
            values: Mutex::new(BTreeMap::new()),
        }
    }
}
impl<K: Ord + Clone, T> KeyedResults<K, T> {
    pub fn get_or_init(
        &self,
        key: &K,
        read: impl FnOnce(&K) -> io::Result<T>,
    ) -> io::Result<Arc<T>> {
        let result = {
            let mut values = self
                .values
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Arc::clone(values.entry(key.clone()).or_default())
        };
        result.get_or_init(|| read(key))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn loads(&self, key: &K) -> usize {
        self.values
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(key)
            .map_or(0, |result| result.loads())
    }
}

#[derive(Clone)]
pub struct Drawn<T> {
    value: T,
    noted: OwnRefusals,
}

impl<T> Drawn<T> {
    pub fn new(render: impl FnOnce() -> T) -> Self {
        let (value, noted) = capture(render);
        Self { value, noted }
    }

    pub fn try_new<E>(render: impl FnOnce() -> Result<T, E>) -> Result<Self, E> {
        let (value, noted) = capture(render);
        match value {
            Ok(value) => Ok(Self { value, noted }),
            Err(error) => {
                credit(&noted);
                Err(error)
            }
        }
    }

    pub fn get(&self) -> &T {
        credit(&self.noted);
        &self.value
    }
}

pub fn created_gui_sprite<'a>(
    sprites: &'a SpriteSet,
    name: &str,
) -> io::Result<&'a content_reader_gfx::gfx::SpriteType> {
    if let Some(sprite) = sprites.find(name).map_err(invalid)? {
        return Ok(sprite);
    }
    let fallback = std::str::from_utf8(content_reader_gui::gui::native::resources::FALLBACK_SPRITE)
        .expect("the executable's fallback sprite literal is ASCII");
    sprites
        .lookup(fallback)
        .map_err(|why| invalid(format!("{name} is not declared, and its fallback: {why}")))
}

pub fn text_icon_sprite<'a>(
    sprites: &'a SpriteSet,
    alias: &str,
) -> io::Result<&'a content_reader_gfx::gfx::SpriteType> {
    let prefixed = format!("GFX_{alias}");
    if let Some(sprite) = sprites.find(&prefixed).map_err(invalid)? {
        return Ok(sprite);
    }
    sprites.lookup(alias).map_err(invalid)
}

pub fn text_icon<'a>(
    sprites: &'a SpriteSet,
    name: &str,
) -> io::Result<(&'a content_reader_gfx::gfx::SpriteType, u32)> {
    let stem = name.split_once('|').map_or(name, |(stem, _)| stem);
    let frame = u32::try_from(content_reader_localisation::localisation::icon_lookup(name).1.unwrap_or(0))
        .map_err(|_| invalid(format!("Text icon {name} names no frame")))?;
    Ok((text_icon_sprite(sprites, stem)?, frame))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Plain,
    Scripted,
}

pub fn localised(localisation: &Localisation, key: &str, view: View) -> Result<String, String> {
    localised_with(localisation, key, &[], view)
}

pub fn localised_bytes(
    localisation: &Localisation,
    key: &[u8],
    view: View,
) -> Result<String, String> {
    let text = localisation.localise(key).map_err(|why| why.to_string())?;
    let (text, _) = scripted(&text, view, Kind::Unsupported)?;
    plain_text(&text)
}

pub fn localised_with(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    view: View,
) -> Result<String, String> {
    localised_with_reason(localisation, key, arguments, view, Kind::Unsupported)
        .0
        .map_err(|finding| finding.message)
}

pub fn localised_details(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    view: View,
    country_kind: Kind,
) -> (Option<String>, Vec<Finding>) {
    let (text, mut findings) =
        localised_with_reason(localisation, key, arguments, view, country_kind);
    let text = text.map_err(|finding| findings.push(*finding)).ok();
    (text, findings)
}

fn localised_with_reason(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    view: View,
    country_kind: Kind,
) -> (Result<String, Box<Finding>>, Vec<Finding>) {
    localised_with_renderer(localisation, key, arguments, |text| {
        scripted(text, view, country_kind).map_err(|reason| {
            let aggregate = if country_kind == Kind::Offline {
                "scripted localisation needs recorded country context; unavailable offline"
            } else {
                "scripted localisation is not evaluated because this product does not pass a country"
            };
            Box::new(Finding::grouped(
                country_kind,
                key,
                aggregate,
                format!("localisation {key} holds {reason}"),
            ))
        })
    })
}

fn localised_with_renderer(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    render: impl FnOnce(&str) -> Result<(String, Option<&'static str>), Box<Finding>>,
) -> (Result<String, Box<Finding>>, Vec<Finding>) {
    let (text, fallbacks) = localisation.localise_with_fallbacks(key.as_bytes(), arguments);
    let mut findings: Vec<_> = fallbacks
        .into_iter()
        .map(|reason| Finding::new(Kind::Content, key, reason))
        .collect();
    let text = text
        .map_err(|why| {
            Box::new(Finding::grouped(
                Kind::Refused,
                key,
                "native localisation text refused",
                why.to_string(),
            ))
        })
        .and_then(|text| {
            let (text, fallback) = render(&text)?;
            if let Some(reason) = fallback {
                findings.push(Finding::new(
                    Kind::Content,
                    key,
                    format!("localisation {key} holds {reason}"),
                ));
            }
            plain_text(&text).map_err(|why| {
                Box::new(Finding::grouped(
                    Kind::Unsupported,
                    key,
                    &format!("localisation holds {why}"),
                    format!("localisation {key} holds {why}"),
                ))
            })
        });
    (text, findings)
}

fn scripted(
    text: &str,
    view: View,
    country_kind: Kind,
) -> Result<(String, Option<&'static str>), String> {
    let Some(open) = text.find('[') else {
        return Ok((text.to_owned(), None));
    };
    let closed = text[open + 1..].find(']');
    let country_reason = if country_kind == Kind::Offline {
        "recorded country context is unavailable offline"
    } else {
        "this product does not pass that country"
    };
    match (view, closed) {
        (View::Plain, _) => Ok((text.to_owned(), None)),
        (View::Scripted, None) => Ok((
            String::new(),
            Some("an unclosed scripted expression; the game omits the text"),
        )),
        (View::Scripted, Some(length)) => Err(format!(
            "the scripted expression {:?}, which the game evaluates for the country the text is shown for, and {country_reason}",
            &text[open..open + length + 2],
        )),
    }
}

pub fn localised_in(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    scripted: &crate::scripted_text::ScriptedText<'_>,
) -> Result<String, String> {
    localised_in_details(localisation, key, arguments, scripted)
        .0
        .map_err(|finding| finding.message)
}

pub fn localised_in_details(
    localisation: &Localisation,
    key: &str,
    arguments: &[(&str, Argument<'_>)],
    scripted: &crate::scripted_text::ScriptedText<'_>,
) -> (Result<String, Box<Finding>>, Vec<Finding>) {
    localised_with_renderer(localisation, key, arguments, |text| {
        scripted
            .expand(text)
            .map(|text| (text, None))
            .map_err(|why| {
                Box::new(Finding::grouped(
                    Kind::Unsupported,
                    key,
                    "recorded-country scripted text unavailable",
                    format!("localisation {key} holds {why}"),
                ))
            })
    })
}

pub use content_reader_localisation::localisation::{display_text, plain_text};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test-support"), derive(replay_spool::Spooled))]
pub struct Refusals(BTreeMap<String, BTreeSet<String>>);

impl Refusals {
    pub fn merge(&mut self, other: &Self) {
        for (reason, identities) in &other.0 {
            self.0
                .entry(reason.clone())
                .or_default()
                .extend(identities.iter().cloned());
        }
    }

    pub fn note(&mut self, reason: impl Into<String>, identity: impl Into<String>) {
        self.0
            .entry(reason.into())
            .or_default()
            .insert(identity.into());
    }

    pub fn counts(&self, what: &str) -> Vec<String> {
        self.0
            .iter()
            .map(|(reason, identities)| format!("{} {what} not shown: {reason}", identities.len()))
            .collect()
    }

    pub fn lines(&self, what: &str) -> Vec<String> {
        self.diagnostic_lines(&format!("{what} not shown"))
    }

    pub fn diagnostic_lines(&self, what: &str) -> Vec<String> {
        self.counted_diagnostic_lines(what)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    pub fn counted_diagnostic_lines(&self, what: &str) -> Vec<(String, usize)> {
        self.0
            .iter()
            .map(|(reason, identities)| {
                (
                    format!(
                        "{} {what}: {reason}; {}",
                        identities.len(),
                        identities.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                    identities.len(),
                )
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test-support"), derive(replay_spool::Spooled))]
pub enum Reader {
    Scripted,
    States,
    FactionTemplates,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test-support"), derive(replay_spool::Spooled))]
pub struct OwnRefusals {
    texts: Refusals,
    readers: BTreeSet<Reader>,
    #[serde(default)]
    fallbacks: BTreeSet<String>,
    diagnostics: Diagnostics,
}

impl OwnRefusals {
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
            && self.readers.is_empty()
            && self.fallbacks.is_empty()
            && self.diagnostics.is_empty()
    }

    pub fn merge(&mut self, other: &Self) {
        self.texts.merge(&other.texts);
        self.readers.extend(other.readers.iter().copied());
        self.fallbacks.extend(other.fallbacks.iter().cloned());
        self.diagnostics.merge(&other.diagnostics);
    }

    pub fn fallback_line(&self) -> Option<String> {
        content_reader_layers::content::overlay_refusal_line(&self.fallbacks)
    }

    pub fn fallbacks(&self) -> &BTreeSet<String> {
        &self.fallbacks
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub fn readers(&self) -> &BTreeSet<Reader> {
        &self.readers
    }
}

struct Active {
    own: OwnRefusals,
    reports: bool,
    isolated: bool,
}

thread_local! {
    static SCOPES: std::cell::RefCell<Vec<Active>> = const { std::cell::RefCell::new(Vec::new()) };
    static SHARED_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub fn shared_reads() -> u64 {
    SHARED_READS.with(std::cell::Cell::get)
}

fn read_shared() {
    SHARED_READS.with(|reads| reads.set(reads.get() + 1));
}

fn scoped<T>(own: &mut OwnRefusals, reports: bool, run: impl FnOnce() -> T) -> T {
    scope(own, reports, false, run)
}

fn scope<T>(own: &mut OwnRefusals, reports: bool, isolated: bool, run: impl FnOnce() -> T) -> T {
    let mut diagnostics = own.diagnostics.clone();
    diagnostics::scope(&mut diagnostics, isolated, || {
        struct Exit<'a>(&'a mut OwnRefusals);
        impl Drop for Exit<'_> {
            fn drop(&mut self) {
                if let Some(mut active) = SCOPES.with(|scopes| scopes.borrow_mut().pop()) {
                    active.own.diagnostics = diagnostics::current();
                    active
                        .own
                        .fallbacks
                        .extend(active.own.diagnostics.fallbacks().iter().cloned());
                    *self.0 = active.own;
                }
            }
        }
        SCOPES.with(|scopes| {
            scopes.borrow_mut().push(Active {
                own: std::mem::take(own),
                reports,
                isolated,
            });
        });
        let _exit = Exit(own);
        let (value, fallbacks) = if isolated {
            content_reader_layers::content::observe_overlay_refusals_isolated(run)
        } else {
            content_reader_layers::content::observe_overlay_refusals(run)
        };
        SCOPES.with(|scopes| {
            if let Some(active) = scopes.borrow_mut().last_mut() {
                active.own.fallbacks.extend(fallbacks);
            }
        });
        value
    })
}

pub fn own_refusals<T>(own: &mut OwnRefusals, run: impl FnOnce() -> T) -> T {
    scoped(own, true, run)
}

pub fn capture<T>(run: impl FnOnce() -> T) -> (T, OwnRefusals) {
    let mut own = OwnRefusals::default();
    let value = scoped(&mut own, false, run);
    (value, own)
}

pub fn capture_isolated<T>(run: impl FnOnce() -> T) -> (T, OwnRefusals) {
    let mut own = OwnRefusals::default();
    let value = scope(&mut own, false, true, run);
    (value, own)
}

fn isolated() -> bool {
    SCOPES.with(|scopes| scopes.borrow().iter().any(|active| active.isolated))
}

fn reached(scopes: &mut [Active]) -> &mut [Active] {
    let from = scopes
        .iter()
        .rposition(|active| active.isolated)
        .unwrap_or(0);
    &mut scopes[from..]
}

pub fn credit(noted: &OwnRefusals) {
    if noted.is_empty() {
        return;
    }
    noted.diagnostics.credit();
    SCOPES.with(|scopes| {
        for active in reached(&mut scopes.borrow_mut()) {
            active.own.merge(noted);
        }
    });
    content_reader_layers::content::credit_overlay_refusals(&noted.fallbacks);
}

pub fn used(reader: Reader) {
    SCOPES.with(|scopes| {
        for active in reached(&mut scopes.borrow_mut()) {
            active.own.readers.insert(reader);
        }
    });
}

fn note_own(reason: &str, identity: &str) {
    SCOPES.with(|scopes| {
        for active in reached(&mut scopes.borrow_mut()) {
            active.own.texts.note(reason, identity);
        }
    });
}

fn reporting<T>(read: impl FnOnce(&OwnRefusals) -> T) -> Option<T> {
    SCOPES.with(|scopes| {
        scopes
            .borrow()
            .iter()
            .rev()
            .find(|active| active.reports)
            .map(|active| read(&active.own))
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BoundedRefusals {
    refused: Refusals,
    bytes: usize,
    overflow: bool,
    budget: usize,
}

impl BoundedRefusals {
    pub fn new(budget: usize) -> Self {
        Self {
            refused: Refusals::default(),
            bytes: 0,
            overflow: false,
            budget,
        }
    }

    pub fn note(&mut self, reason: impl Into<String>, identity: impl Into<String>) {
        if self.overflow {
            return;
        }
        let reason = reason.into();
        let identity = identity.into();
        if self
            .refused
            .0
            .get(&reason)
            .is_some_and(|identities| identities.contains(&identity))
        {
            return;
        }
        let available = self.budget.saturating_sub(self.bytes + 64);
        let Some(size) = crate::transport::json_len(&(&reason, &identity), available) else {
            self.overflow = true;
            return;
        };
        let Some(bytes) = self.bytes.checked_add(size + 64) else {
            self.overflow = true;
            return;
        };
        if bytes > self.budget {
            self.overflow = true;
            return;
        }
        self.bytes = bytes;
        self.refused.note(reason, identity);
    }

    pub fn check(&self) -> io::Result<usize> {
        if self.overflow {
            Err(io::Error::other(
                "Product refusals exceed the complete-response budget",
            ))
        } else {
            Ok(self.bytes)
        }
    }

    pub fn merge(&mut self, other: &Self) {
        if other.overflow {
            self.overflow = true;
        }
        for (reason, identities) in &other.refused.0 {
            for identity in identities {
                self.note(reason, identity);
            }
        }
    }

    pub fn lines(&self, what: &str) -> Vec<String> {
        self.refused.lines(what)
    }
}


#[cfg(any(test, feature = "test-support"))]
impl replay_spool::Spooled for Catalogue {
    const RESIDENT: bool = true;

    fn spool(&self, _: &mut replay_spool::Writer<'_>) -> std::io::Result<()> {
        Err(replay_spool::invalid("Content is resident, never written"))
    }

    fn unspool(_: &mut replay_spool::Reader<'_>) -> std::io::Result<Self> {
        Err(replay_spool::invalid("Content is resident, never read"))
    }
}

#[derive(Default)]
struct LayerDefines {
    settled: std::sync::OnceLock<(Result<Defines, Arc<io::Error>>, Diagnostics)>,
    reading: Mutex<()>,
}

impl LayerDefines {
    fn entry(
        (result, diagnostics): &(Result<Defines, Arc<io::Error>>, Diagnostics),
    ) -> io::Result<&Defines> {
        diagnostics.credit();
        match result {
            Ok(defines) => Ok(defines),
            Err(error) => Err(io::Error::new(error.kind(), Arc::clone(error))),
        }
    }

    fn read(&self, layers: &ResolvedLayers) -> io::Result<&Defines> {
        if let Some(entry) = self.settled.get() {
            return Self::entry(entry);
        }
        let _guard = self
            .reading
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = self.settled.get() {
            return Self::entry(entry);
        }
        let (result, diagnostics) =
            diagnostics::capture(|| content_reader_defines::defines::read(layers));
        match result {
            Ok(defines) => {
                let entry = self.settled.get_or_init(|| (Ok(defines), diagnostics));
                Self::entry(entry)
            }
            Err(error) if content_reader_defines::defines::is_script_error(&error) => {
                let entry = self
                    .settled
                    .get_or_init(|| (Err(Arc::new(error)), diagnostics));
                Self::entry(entry)
            }
            Err(error) => {
                diagnostics.credit();
                Err(error)
            }
        }
    }
}

pub fn defines_for_layers(layers: &ResolvedLayers) -> io::Result<Defines> {
    layers.derived::<LayerDefines>().read(layers).cloned()
}

pub struct Catalogue {
    pub content: Arc<ResolvedContent>,
    pub presentation: Presentation,
    pub localisation: Arc<Localisation>,
    pub sprites: SpriteSet,
    guis: content_reader_layers::content::ByMode<
        Once<(
            Result<Arc<content_reader_gui::gui::native::Guis>, String>,
            BTreeSet<String>,
        )>,
    >,
    presentation_localisation:
        content_reader_layers::content::ByMode<Once<(Result<Arc<Localisation>, String>, BTreeSet<String>)>>,
    pub gui_decode: Result<content_reader_image::native::Decode, String>,
    pub verification: Vec<String>,
    pub warnings: Vec<String>,
    pub recorded_language: String,
    refused: Mutex<Refusals>,
    findings: Option<Mutex<BTreeSet<Finding>>>,
    effect_describer: Option<EffectDescriberFactory>,
    scripted: Once<Result<crate::scripted_text::ScriptedContent, String>>,
    events: std::sync::OnceLock<content_reader_events::events::LazyEvents>,
    defines: Arc<LayerDefines>,
    war_names: Once<Result<content_reader_wargoals::wargoals::Names, String>>,
    pub state_content: Arc<Once<Result<crate::scripted_text::StateContent, String>>>,
    recorded: Once<Arc<Catalogue>>,
    #[cfg(any(test, feature = "test-support"))]
    content_loads: std::sync::atomic::AtomicUsize,
}

impl Catalogue {
    pub fn recorded(&self) -> Option<&Arc<Catalogue>> {
        self.recorded.get()
    }

    pub fn keep_recorded(&self, recorded: Arc<Catalogue>) {
        let _ = self.recorded.set(recorded);
    }

    pub fn guis(&self) -> io::Result<&content_reader_gui::gui::native::Guis> {
        let (guis, fallbacks) = self.guis.current().get_or_init(|| {
            #[cfg(any(test, feature = "test-support"))]
            self.content_loads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (guis, fallbacks) = content_reader_layers::content::observe_overlay_refusals(|| {
                self.content.layers.artwork(
                    "interface windows",
                    |layers| content_reader_gui::gui::native::read_shared_layers(layers, None),
                )
            });
            (guis.map_err(|error| error.to_string()), fallbacks)
        });
        content_reader_layers::content::credit_overlay_refusals(fallbacks);
        guis.as_deref().map_err(invalid)
    }

    pub fn presentation_localisation(&self) -> io::Result<Arc<Localisation>> {
        let layers = &self.content.layers;
        if !layers.presentation().shows_visual_mods() {
            return Ok(Arc::clone(&self.localisation));
        }
        let (localisation, fallbacks) = self.presentation_localisation.current().get_or_init(|| {
            let (localisation, fallbacks) =
                content_reader_layers::content::observe_overlay_refusals(|| {
                    layers.artwork("localisation", |stack| {
                        use content_reader_localisation::localisation::{read, read_shared};
                        read_shared(stack, &self.localisation.language, read)
                    })
                });
            (
                localisation.map_err(|error| error.to_string()),
                fallbacks,
            )
        });
        content_reader_layers::content::credit_overlay_refusals(fallbacks);
        localisation.clone().map_err(invalid)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn guis_loaded(&self) -> bool {
        self.guis.ordinary().get().is_some()
    }

    pub fn war_names(&self) -> Result<&content_reader_wargoals::wargoals::Names, String> {
        self.war_names
            .get_or_init(|| {
                #[cfg(any(test, feature = "test-support"))]
                self.content_loads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                content_reader_wargoals::wargoals::read_names(&self.content.layers)
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }
    pub fn from_content(
        content: Arc<ResolvedContent>,
        presentation: Presentation,
        localisation: Arc<Localisation>,
        verification: Vec<String>,
        warnings: Vec<String>,
        recorded_language: String,
        gui_decode: Result<content_reader_image::native::Decode, String>,
    ) -> io::Result<Self> {
        let sprites = content
            .layers
            .artwork("interface sprites", content_reader_gfx::gfx::read)?;
        Ok(Self::from_parts(
            content,
            presentation,
            localisation,
            sprites,
            verification,
            warnings,
            recorded_language,
            gui_decode,
            None,
        ))
    }

    pub fn from_parts(
        content: Arc<ResolvedContent>,
        presentation: Presentation,
        localisation: Arc<Localisation>,
        sprites: SpriteSet,
        verification: Vec<String>,
        mut warnings: Vec<String>,
        recorded_language: String,
        gui_decode: Result<content_reader_image::native::Decode, String>,
        effect_describer: Option<EffectDescriberFactory>,
    ) -> Self {
        for (label, count) in [
            ("logged localisation diagnostics", localisation.logged.len()),
            ("refused localisation forms", localisation.refused.len()),
            ("duplicate localisation keys", localisation.duplicates.len()),
        ] {
            if count > 0 {
                warnings.push(format!("Recorded content contains {count} {label}."));
            }
        }
        if !sprites.refused.is_empty() {
            warnings.push(format!(
                "Recorded content contains {} refusals in its interface files: {}.",
                sprites.refused.len(),
                sprites.refused.join("; ")
            ));
        }
        Self {
            defines: content.layers.derived(),
            content,
            presentation,
            localisation,
            sprites,
            guis: Default::default(),
            presentation_localisation: Default::default(),
            gui_decode,
            verification,
            warnings,
            recorded_language,
            refused: Mutex::default(),
            findings: None,
            effect_describer,
            scripted: Once::new(),
            events: Default::default(),
            war_names: Once::new(),
            state_content: Arc::new(Once::new()),
            recorded: Once::new(),
            #[cfg(any(test, feature = "test-support"))]
            content_loads: Default::default(),
        }
    }

    pub fn of_layers(layers: ResolvedLayers, language: &str) -> io::Result<Self> {
        let localisation = Arc::new(content_reader_localisation::localisation::read(
            &layers, language,
        )?);
        let content = Arc::new(ResolvedContent {
            layers,
            stats: Default::default(),
            changed: Vec::new(),
            notes: Vec::new(),
        });
        Self::from_content(
            content,
            Presentation::default(),
            localisation,
            Vec::new(),
            Vec::new(),
            language.to_owned(),
            Ok(Default::default()),
        )
    }

    pub fn localise_in(
        &self,
        key: &str,
        scripted: &crate::scripted_text::ScriptedText<'_>,
    ) -> Option<String> {
        if !self.has_presentation_key(key) {
            return None;
        }
        let (text, findings) = localised_in_details(&self.localisation, key, &[], scripted);
        for finding in findings {
            self.note(finding);
        }
        text.inspect_err(|finding| self.refuse_finding((**finding).clone()))
            .ok()
    }

    pub fn localised_with(
        &self,
        key: &str,
        arguments: &[(&str, Argument<'_>)],
        view: View,
    ) -> Option<String> {
        self.localised_for(key, arguments, view, Kind::Unsupported, None)
    }

    pub fn localised_with_refusals(
        &self,
        key: &str,
        arguments: &[(&str, Argument<'_>)],
        view: View,
        refusals: &Mutex<Refusals>,
    ) -> Option<String> {
        self.localised_for(key, arguments, view, Kind::Unsupported, Some(refusals))
    }

    fn localised_for(
        &self,
        key: &str,
        arguments: &[(&str, Argument<'_>)],
        view: View,
        country_kind: Kind,
        refusals: Option<&Mutex<Refusals>>,
    ) -> Option<String> {
        self.localised_or_why_for(key, arguments, view, country_kind, refusals)
            .ok()
    }

    pub fn localised_or_why(&self, key: &str, view: View) -> Result<String, String> {
        self.localised_or_why_for(key, &[], view, Kind::Unsupported, None)
    }

    fn localised_or_why_for(
        &self,
        key: &str,
        arguments: &[(&str, Argument<'_>)],
        view: View,
        country_kind: Kind,
        refusals: Option<&Mutex<Refusals>>,
    ) -> Result<String, String> {
        if !self.has_presentation_key(key) {
            let key = key.split('\0').next().unwrap_or_default();
            if let Some(refusals) = refusals {
                refusals
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .note(
                        format!("localisation {key} is missing; title is unavailable"),
                        key,
                    );
            }
            return Err(format!("localisation {key} is missing"));
        }
        let (text, findings) =
            localised_with_reason(&self.localisation, key, arguments, view, country_kind);
        for finding in findings {
            self.note(finding);
        }
        text.map_err(|finding| {
            if let Some(refusals) = refusals {
                refusals
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .note(&finding.message, &finding.subject);
            }
            let message = finding.message.clone();
            self.refuse_finding(*finding);
            message
        })
    }

    pub fn has_presentation_key(&self, key: &str) -> bool {
        let key = key.split('\0').next().unwrap_or_default();
        if key.is_empty() || self.localisation.get(key).is_some() {
            return true;
        }
        self.note(Finding::new(
            Kind::Content,
            key,
            format!("localisation {key} is missing; title is unavailable"),
        ));
        false
    }

    pub fn localised(&self, key: &str, view: View) -> Option<String> {
        self.localised_with(key, &[], view)
    }

    pub fn event_index(&self) -> &content_reader_events::events::LazyEvents {
        self.events.get_or_init(|| {
            content_reader_events::events::LazyEvents::from_content(Arc::clone(&self.content))
        })
    }

    pub fn scripted_content(&self) -> Result<&crate::scripted_text::ScriptedContent, String> {
        used(Reader::Scripted);
        self.scripted
            .get_or_init(|| {
                #[cfg(any(test, feature = "test-support"))]
                self.content_loads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                crate::scripted_text::ScriptedContent::read(
                    Arc::clone(&self.content),
                    self.recorded_language.clone(),
                )
                .map(|mut scripted| {
                    scripted.state_content = Arc::clone(&self.state_content);
                    match self.effect_describer {
                        Some(factory) => scripted.with_effect_describer(factory),
                        None => scripted,
                    }
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn defines(&self) -> Result<&Defines, String> {
        self.read_defines().map_err(|why| why.to_string())
    }

    pub fn read_defines(&self) -> io::Result<&Defines> {
        self.defines.read(&self.content.layers)
    }

    pub fn war_participation_factors(
        &self,
    ) -> Result<replay_data::war_participation::Factors, String> {
        let defines = self.scripted_content()?.environment()?.defines();
        let factor = |name| defines.fixed5("NMilitary", name).map_err(|e| e.to_string());
        Ok(replay_data::war_participation::Factors {
            industry_given: factor("WAR_SCORE_LEND_LEASE_GIVEN_IC_FACTOR")?,
            fuel_given: factor("WAR_SCORE_LEND_LEASE_GIVEN_FUEL_FACTOR")?,
            industry_received: factor("WAR_SCORE_LEND_LEASE_RECEIVED_IC_FACTOR")?,
            fuel_received: factor("WAR_SCORE_LEND_LEASE_RECEIVED_FUEL_FACTOR")?,
        })
    }

    pub fn readers_read(&self) -> bool {
        let scripted = match self.scripted.peek() {
            None => false,
            Some(Ok(scripted)) => scripted.environment.peek().is_some(),
            Some(Err(_)) => true,
        };
        scripted
            && self.state_content.peek().is_some()
            && self.content.layers.faction_templates_read()
    }

    pub fn read_readers(&self) {
        drop(self.defines());
        if let Ok(scripted) = self.scripted_content() {
            drop(scripted.environment());
        }
        drop(self.cached_states());
        drop(self.content.layers.faction_templates());
    }

    pub fn cached_states(&self) -> Result<&crate::scripted_text::StateContent, String> {
        used(Reader::States);
        self.state_content
            .get_or_init(|| {
                #[cfg(any(test, feature = "test-support"))]
                self.content_loads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                crate::scripted_text::read_state_content(&self.content.layers)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn content_loads(&self) -> usize {
        self.content_loads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn refuse(&self, reason: &str, identity: &str) {
        self.refuse_finding(Finding::new(Kind::Refused, identity, reason));
    }

    pub fn refuse_as(&self, kind: Kind, reason: &str, identity: &str) {
        self.refuse_finding(Finding::new(kind, identity, reason));
    }

    pub fn refuse_finding(&self, finding: Finding) {
        note_own(&finding.message, &finding.subject);
        if isolated() {
            return;
        }
        self.refused
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .note(&finding.message, &finding.subject);
        self.note(finding);
    }

    pub fn refusal_lines(&self) -> Vec<String> {
        self.refusal_entries()
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    pub fn refusal_entries(&self) -> Vec<(String, Option<usize>)> {
        let texts = |refused: &Refusals| -> Vec<(String, Option<usize>)> {
            refused
                .counted_diagnostic_lines("texts are not shown")
                .into_iter()
                .map(|(line, count)| (line, Some(count)))
                .collect()
        };
        let mut lines = reporting(|own| texts(&own.texts)).unwrap_or_else(|| {
            read_shared();
            texts(
                &self
                    .refused
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            )
        });
        lines.extend(
            self.reader_warning_lines()
                .into_iter()
                .map(|line| (line, None)),
        );
        lines
    }

    pub fn reader_warning_lines(&self) -> Vec<String> {
        if let Some(readers) = reporting(|own| own.readers.clone()) {
            return self.used_reader_warning_lines(&readers);
        }
        read_shared();
        let mut lines = Vec::new();
        let environment_warnings = if let Some(Ok(scripted)) = self.scripted.get() {
            lines.extend(
                scripted
                    .names
                    .warnings
                    .iter()
                    .map(|(_, message)| message.clone()),
            );
            let environment_warnings = scripted
                .environment
                .get()
                .and_then(|environment| environment.as_ref().ok())
                .map_or(&[][..], |environment| environment.warnings());
            lines.extend(environment_warnings.iter().cloned());
            environment_warnings
        } else {
            &[]
        };
        if let Some(Ok(states)) = self.state_content.get() {
            lines.extend(
                crate::scripted_text::live_state_warnings(&states.warnings)
                    .filter(|warning| {
                        !environment_warnings
                            .iter()
                            .any(|line| line.as_str() == *warning)
                    })
                    .map(str::to_owned),
            );
        }
        lines.extend(self.content.layers.faction_template_diagnostics());
        diagnostics::current().append_to(&mut lines);
        lines
    }

    fn used_reader_warning_lines(&self, readers: &BTreeSet<Reader>) -> Vec<String> {
        let mut lines = Vec::new();
        if readers.contains(&Reader::Scripted)
            && let Ok(scripted) = self.scripted_content()
        {
            lines.extend(
                scripted
                    .names
                    .warnings
                    .iter()
                    .map(|(_, message)| message.clone()),
            );
        }
        if readers.contains(&Reader::States)
            && let Ok(states) = self.cached_states()
        {
            lines.extend(
                crate::scripted_text::live_state_warnings(&states.warnings).map(str::to_owned),
            );
        }
        if readers.contains(&Reader::FactionTemplates) {
            let _ = self.content.layers.faction_templates();
            lines.extend(self.content.layers.faction_template_diagnostics());
        }
        lines
    }

    pub fn note(&self, finding: Finding) {
        if let Some(findings) = &self.findings {
            findings
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(finding);
        }
    }

    pub fn take_findings(&self) -> Vec<Finding> {
        self.findings
            .as_ref()
            .map(|findings| {
                std::mem::take(&mut *findings.lock().unwrap_or_else(|e| e.into_inner()))
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn of_layers_for_check(
        layers: ResolvedLayers,
        language: &str,
        effect_describer: EffectDescriberFactory,
    ) -> io::Result<Self> {
        let mut catalogue = Self::of_layers(layers, language)?;
        catalogue.effect_describer = Some(effect_describer);
        catalogue.findings = Some(Mutex::default());
        Ok(catalogue)
    }
}

pub fn read_texture(layers: &ResolvedLayers, path: &str, decode: Decode) -> io::Result<Rgba8Image> {
    gui_texture(layers, path.as_bytes(), decode)
}

pub fn texture_png(
    layers: &ResolvedLayers,
    path: &str,
    decode: Decode,
    frames: u32,
    max_edge: u32,
) -> io::Result<String> {
    texture_frame_png(layers, path, decode, frames, 0, max_edge)
}

pub fn texture_frame_png(
    layers: &ResolvedLayers,
    path: &str,
    decode: Decode,
    frames: u32,
    frame: u32,
    max_edge: u32,
) -> io::Result<String> {
    gui_texture_frame_png(layers, path.as_bytes(), decode, frames, frame, max_edge)
}

pub fn gui_texture(layers: &ResolvedLayers, name: &[u8], decode: Decode) -> io::Result<Rgba8Image> {
    layers.artwork(
        &format!("texture {}", String::from_utf8_lossy(name)),
        |stack| stack_gui_texture(stack, name, decode),
    )
}

fn stack_gui_texture(
    layers: &ResolvedLayers,
    name: &[u8],
    decode: Decode,
) -> io::Result<Rgba8Image> {
    let texture = resources::stack_texture(layers, name, decode)?.map_err(|why| {
        let extra = if why.native_failure() {
            "; the game draws no texture either"
        } else {
            ""
        };
        invalid(format!(
            "GUI texture {} is unavailable: {why}{extra}",
            String::from_utf8_lossy(name)
        ))
    })?;
    content_reader_image::rgba::dds_base(texture.image)
}

pub fn gui_texture_frame_png(
    layers: &ResolvedLayers,
    name: &[u8],
    decode: Decode,
    frames: u32,
    frame: u32,
    max_edge: u32,
) -> io::Result<String> {
    image_frame_png(&gui_texture(layers, name, decode)?, frames, frame, max_edge)
}

pub fn image_frame_png(
    image: &Rgba8Image,
    frames: u32,
    frame: u32,
    max_edge: u32,
) -> io::Result<String> {
    if frames == 0 || frame >= frames {
        return Err(invalid("Invalid sprite frames"));
    }
    let plain = Plain {
        frames: frames
            .try_into()
            .map_err(|_| invalid("Invalid sprite frames"))?,
        ..Plain::default()
    };
    png_url(&native_frame(image, &plain, frame, max_edge)?)
}

pub fn sprite_frame_png(
    layers: &ResolvedLayers,
    sprite: &content_reader_gfx::gfx::SpriteType,
    decode: Decode,
    frame: u32,
    max_edge: u32,
) -> io::Result<String> {
    let render = || {
        sprite_artwork(layers, sprite, |stack, sprite| {
            render_sprite_frame_png(stack, sprite, decode, frame, max_edge)
        })
    };
    if !strip_frame(sprite, frame) {
        return SpriteImages::unkept(render);
    }
    let cache = layers.presentation().derived::<SpriteImages>();
    cache.get(sprite_image_key(sprite, decode, frame, max_edge), render)
}

pub fn prepare_sprite_renderer(catalogue: &Catalogue) {
    let prepare = |catalogue: &Catalogue| {
        for force_pow2_textures in [false, true] {
            let _ = shader_pipeline::artwork::prepare_sprite(
                catalogue.content.layers.presentation(), Plain::default().effect(), Decode { force_pow2_textures },
            );
        }
    };
    prepare(catalogue);
    if let Some(recorded) = catalogue.recorded() {
        prepare(recorded);
    }
}

fn strip_frame(sprite: &content_reader_gfx::gfx::SpriteType, frame: u32) -> bool {
    use content_reader_gui::gui::native::sprites::SpriteKind;
    match &sprite.facts.kind {
        SpriteKind::Plain(plain) | SpriteKind::FrameAnimated { plain, .. } => {
            frame < plain.frames.max(0) as u32
        }
        _ => frame == 0,
    }
}

pub fn sprite_artwork<T>(
    layers: &ResolvedLayers,
    sprite: &content_reader_gfx::gfx::SpriteType,
    mut render: impl FnMut(&ResolvedLayers, &content_reader_gfx::gfx::SpriteType) -> io::Result<T>,
) -> io::Result<T> {
    let shown = layers.presentation().shows_visual_mods();
    layers.artwork(&format!("sprite {}", sprite.name), |stack| {
        if !shown || stack.shows_visual_mods() {
            return render(stack, sprite);
        }
        let recorded = recorded_sprites(stack);
        let own = (*recorded)
            .as_ref()
            .map_err(|why| invalid(why.clone()))?
            .find(&sprite.name)
            .map_err(invalid)?
            .ok_or_else(|| invalid(format!("the recording declares no sprite {}", sprite.name)))?;
        render(stack, own)
    })
}

pub fn recorded_sprites(layers: &ResolvedLayers) -> Arc<Result<SpriteSet, String>> {
    #[derive(Default)]
    struct Recorded(Once<Arc<Result<SpriteSet, String>>>);
    Arc::clone(layers.derived::<Recorded>().0.get_or_init(|| {
        Arc::new(content_reader_gfx::gfx::read(layers).map_err(|error| error.to_string()))
    }))
}

pub fn sprite_image_key(
    sprite: &content_reader_gfx::gfx::SpriteType,
    decode: Decode,
    frame: u32,
    max_edge: u32,
) -> String {
    format!(
        "{:?}/{:?}/{:?}/{frame}/{max_edge}/{:?}",
        sprite.name,
        sprite.facts,
        decode,
        content_reader_layers::content::artwork_mode()
    )
}

pub fn authored_frame(frame: i32) -> u32 {
    u32::try_from(frame).map_or(0, |frame| frame.saturating_sub(1))
}

pub fn sprite_frames_png(
    layers: &ResolvedLayers,
    requests: &[(&content_reader_gfx::gfx::SpriteType, u32)],
    decode: Decode,
    max_edge: u32,
) -> io::Result<()> {
    use content_reader_gui::gui::native::sprites::SpriteKind;
    let layers = layers.presentation();
    let cache = layers.derived::<SpriteImages>();
    let mut keys = BTreeSet::new();
    let requests: Vec<_> = requests
        .iter()
        .filter_map(|&(sprite, frame)| {
            if !strip_frame(sprite, frame) {
                return None;
            }
            let key = sprite_image_key(sprite, decode, frame, max_edge);
            (!cache.contains(&key) && keys.insert(key.clone())).then_some((sprite, frame, key))
        })
        .collect();
    if requests.len() < 2 {
        return Ok(());
    }
    let mut draws = Vec::new();
    let mut reads = Vec::new();
    for &(sprite, frame, _) in &requests {
        let (plain, animated) = match &sprite.facts.kind {
            SpriteKind::Plain(plain) => (plain, None),
            SpriteKind::FrameAnimated { plain, animation } => (plain, Some(animation)),
            _ => {
                return Err(invalid(
                    "sprite batch requires plain or frame-animated draws",
                ));
            }
        };
        let (draw, read) = diagnostics::capture(|| {
            sprite::draw(layers, plain, animated, decode, frame, max_edge)
        });
        draws.push(draw?);
        reads.push(read);
    }
    let images = shader_pipeline::artwork::render_batch(layers, draws, decode)
        .map_err(invalid)?
        .into_iter()
        .map(|(image, rendered)| {
            native_frame(&image, &Plain::default(), 0, max_edge)
                .and_then(|image| png_url(&image))
                .map(|image| (image, rendered))
        })
        .collect::<io::Result<Vec<_>>>()?;
    for (((_, _, key), (image, rendered)), read) in requests.into_iter().zip(images).zip(reads) {
        cache.get(key, || {
            read.credit();
            rendered.credit();
            Ok(image)
        })?;
    }
    Ok(())
}

type ImageEntry = Once<(Result<String, String>, BTreeSet<String>)>;

#[derive(Default)]
pub struct SpriteImages {
    entries: Mutex<BTreeMap<String, Arc<ImageEntry>>>,
}

impl SpriteImages {
    pub fn contains(&self, key: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(key)
    }

    fn get(&self, key: String, render: impl FnOnce() -> io::Result<String>) -> io::Result<String> {
        let slot = Arc::clone(
            self.entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(key)
                .or_default(),
        );
        let (result, fallbacks) = slot.get_or_init(|| {
            let (result, fallbacks) = content_reader_layers::content::observe_overlay_refusals(render);
            (result.map_err(|error| error.to_string()), fallbacks)
        });
        content_reader_layers::content::credit_overlay_refusals(fallbacks);
        result.clone().map_err(invalid)
    }

    fn unkept(render: impl FnOnce() -> io::Result<String>) -> io::Result<String> {
        let (result, fallbacks) = content_reader_layers::content::observe_overlay_refusals(render);
        content_reader_layers::content::credit_overlay_refusals(&fallbacks);
        result.map_err(|error| invalid(error.to_string()))
    }
}

fn render_sprite_frame_png(
    layers: &ResolvedLayers,
    sprite: &content_reader_gfx::gfx::SpriteType,
    decode: Decode,
    frame: u32,
    max_edge: u32,
) -> io::Result<String> {
    use content_reader_gui::gui::native::sprites::SpriteKind;
    let (plain, animated) = match &sprite.facts.kind {
        SpriteKind::Plain(plain) => (plain, None),
        SpriteKind::FrameAnimated { plain, animation } => (plain, Some(animation)),
        _ => {
            return Err(invalid(format!(
                "sprite {}: {} needs native sprite rendering",
                sprite.name,
                sprite.facts.kind.keyword()
            )));
        }
    };
    sprite::frame(layers, plain, animated, decode, frame, max_edge)
        .and_then(|image| png_url(&image))
        .map_err(|error| invalid(format!("sprite {}: {error}", sprite.name)))
}

mod shield;
mod sprite;
pub use shield::MaskedShield;

fn native_frame(
    image: &Rgba8Image,
    plain: &Plain,
    frame: u32,
    max_edge: u32,
) -> io::Result<Rgba8Image> {
    debug_assert!(frame < plain.frames.max(0) as u32);
    let texture = [image.width, image.height];
    let size = resources::frame_extent(plain, texture);
    let pitch = resources::frame_geometry(plain, texture);
    if max_edge == 0 || size.iter().any(|edge| *edge <= 0) {
        return Err(invalid(
            "Invalid sprite frames: the queried frame has no pixels",
        ));
    }
    let offset = if plain.frame_direction_y() {
        [0.0, frame as f32 * pitch[1]]
    } else {
        [frame as f32 * pitch[0], 0.0]
    };
    let size = size.map(|edge| edge as u32);
    let scale = (f64::from(size[0].max(size[1])) / f64::from(max_edge)).max(1.0);
    let width = (f64::from(size[0]) / scale).round().max(1.0) as u32;
    let height = (f64::from(size[1]) / scale).round().max(1.0) as u32;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let px = (offset[0] + (x as f32 + 0.5) * pitch[0] / width as f32) as u32;
            let py = (offset[1] + (y as f32 + 0.5) * pitch[1] / height as f32) as u32;
            let at =
                ((py.min(image.height - 1) * image.width + px.min(image.width - 1)) * 4) as usize;
            pixels.extend_from_slice(&image.pixels[at..at + 4]);
        }
    }
    Ok(Rgba8Image {
        width,
        height,
        pixels,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub mod texture_tests;

#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    use super::*;
    pub use dashboard_core::test_fixtures::game;
    pub mod script_game;

    #[test]
    fn keyed_results_resolve_new_keys_once_and_share_success() {
        let cache = KeyedResults::default();
        for key in ["authored", "later"] {
            let key = key.to_owned();
            assert_eq!(cache.loads(&key), 0);
            let first = cache
                .get_or_init(&key, |spelling| Ok(format!("class/{spelling}")))
                .unwrap();
            for _ in 0..10 {
                let held = cache
                    .get_or_init(&key, |_| panic!("classified the same operand twice"))
                    .unwrap();
                assert!(Arc::ptr_eq(&first, &held));
                assert_eq!(*held, format!("class/{key}"));
            }
            assert_eq!(cache.loads(&key), 1);
        }
    }

    #[test]
    fn keyed_results_keep_content_errors_and_retry_read_errors() {
        let cache = KeyedResults::<String, String>::default();
        let key = "mod/refused".to_owned();
        let error = cache
            .get_or_init(&key, |spelling| {
                Err(content_reader_parser::fields::invalid(format!(
                    "{spelling}: unsupported authored projection"
                )))
            })
            .unwrap_err();
        for _ in 0..10 {
            let held = cache
                .get_or_init(&key, |_| panic!("retried a fixed content error"))
                .unwrap_err();
            assert_eq!(held.kind(), error.kind());
            assert_eq!(held.to_string(), error.to_string());
            assert!(content_reader_parser::fields::is_content_error(&held));
        }
        assert_eq!(cache.loads(&key), 1);

        let key = "unreadable".to_owned();
        for transient in [
            io::Error::from_raw_os_error(2),
            io::Error::from(io::ErrorKind::PermissionDenied),
            io::Error::from(io::ErrorKind::NotFound),
            io::Error::new(
                io::ErrorKind::InvalidData,
                "content file changed since verification: common/probe.txt",
            ),
        ] {
            let expected_kind = transient.kind();
            let got = cache.get_or_init(&key, |_| Err(transient)).unwrap_err();
            assert_eq!(got.kind(), expected_kind);
        }
        assert_eq!(*cache.get_or_init(&key, |_| Ok("recovered".to_owned())).unwrap(), "recovered");
        assert_eq!(cache.loads(&key), 5);
    }

    #[test]
    fn keyed_results_retry_interrupted_reads_without_evicting_other_keys() {
        let cache = KeyedResults::default();
        let held_key = "held".to_owned();
        let held = cache.get_or_init(&held_key, |_| Ok(7)).unwrap();
        let key = "cancelled".to_owned();
        for _ in 0..2 {
            assert_eq!(
                cache
                    .get_or_init(&key, |_| Err(io::Error::from(io::ErrorKind::Interrupted)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Interrupted
            );
        }
        assert_eq!(*cache.get_or_init(&key, |_| Ok(11)).unwrap(), 11);
        assert_eq!(
            *cache.get_or_init(&key, |_| panic!("reloaded")).unwrap(),
            11
        );
        assert_eq!(cache.loads(&key), 3);
        assert!(Arc::ptr_eq(
            &held,
            &cache
                .get_or_init(&held_key, |_| panic!("evicted another key"))
                .unwrap()
        ));
        assert_eq!(cache.loads(&held_key), 1);
    }

    #[test]
    fn keyed_results_serialise_concurrent_first_access_per_key() {
        let cache = KeyedResults::default();
        let barrier = std::sync::Barrier::new(16);
        let rows = std::thread::scope(|threads| {
            let pending: Vec<_> = (0..16)
                .map(|index| {
                    let cache = &cache;
                    let barrier = &barrier;
                    job_priority::spawn_in(threads, move || {
                        let key = if index % 2 == 0 { "known" } else { "refused" }.to_owned();
                        barrier.wait();
                        let result = cache.get_or_init(&key, |spelling| {
                            if spelling == "known" {
                                Ok("authored class".to_owned())
                            } else {
                                Err(content_reader_parser::fields::invalid("refused by name"))
                            }
                        });
                        (key, result)
                    })
                })
                .collect();
            pending
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        let first = Arc::clone(
            rows.iter()
                .find_map(|(_, result)| result.as_ref().ok())
                .unwrap(),
        );
        for (key, result) in rows {
            match result {
                Ok(row) => assert!(Arc::ptr_eq(&first, &row)),
                Err(error) => {
                    assert_eq!(key, "refused");
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    assert_eq!(error.to_string(), "refused by name");
                }
            }
        }
        assert_eq!(cache.loads(&"known".to_owned()), 1);
        assert_eq!(cache.loads(&"refused".to_owned()), 1);
    }

    #[test]
    fn keyed_results_share_a_recorded_stack_and_reset_on_a_new_binding() {
        let (first, _first_folder) = folder_catalogue(&[]);
        let (second, _second_folder) = folder_catalogue(&[]);
        let cache = first
            .content
            .layers
            .derived::<KeyedResults<String, String>>();
        let cloned = first
            .content
            .layers
            .clone()
            .derived::<KeyedResults<String, String>>();
        let other = second
            .content
            .layers
            .derived::<KeyedResults<String, String>>();
        let key = "same recorded spelling".to_owned();
        let original = cache.get_or_init(&key, |_| Ok("first".into())).unwrap();
        assert!(Arc::ptr_eq(
            &original,
            &cloned
                .get_or_init(&key, |_| panic!("reclassified within the same stack"))
                .unwrap()
        ));
        assert_eq!(
            *other.get_or_init(&key, |_| Ok("second".into())).unwrap(),
            "second"
        );
        assert_eq!(
            (cache.loads(&key), cloned.loads(&key), other.loads(&key)),
            (1, 1, 1)
        );
    }

    #[test]
    fn content_results_share_success_and_failure_but_retry_cancellation() {
        let cache = Arc::new(CachedResult::default());
        let rows = std::thread::scope(|threads| {
            let pending: Vec<_> = (0..16)
                .map(|_| {
                    let cache = Arc::clone(&cache);
                    job_priority::spawn_in(threads, move || {
                        cache
                            .get_or_init(|| Ok(vec!["mod/token".to_owned()]))
                            .unwrap()
                    })
                })
                .collect();
            pending
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(cache.loads(), 1);
        assert!(rows.iter().all(|row| Arc::ptr_eq(row, &rows[0])));

        let refused = CachedResult::<Vec<String>>::default();
        for _ in 0..20 {
            let error = refused
                .get_or_init(|| {
                    Err(content_reader_parser::fields::invalid(
                        "mod definition refused by name",
                    ))
                })
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "mod definition refused by name");
            assert!(content_reader_parser::fields::is_content_error(&error));
        }
        assert_eq!(refused.loads(), 1);
        let visual = CachedResult::<Vec<String>>::default();
        for _ in 0..2 {
            let error = visual
                .get_or_init(|| Err(dashboard_core::refused("Synthetic visual mods are refused")))
                .unwrap_err();
            assert_eq!(dashboard_core::refusal(&error), Some("Synthetic visual mods are refused"));
        }
        let error = visual.get().unwrap().unwrap_err();
        assert_eq!(dashboard_core::refusal(&error), Some("Synthetic visual mods are refused"));
        assert_eq!(visual.loads(), 1);
        let transient = CachedResult::default();
        for error in [
            io::Error::from(io::ErrorKind::Interrupted),
            io::Error::from(io::ErrorKind::PermissionDenied),
            io::Error::from(io::ErrorKind::NotFound),
            io::Error::new(
                io::ErrorKind::InvalidData,
                "content file changed since verification: common/probe.txt",
            ),
        ] {
            let expected = error.kind();
            assert_eq!(
                transient
                    .get_or_init(|| Err::<Vec<String>, _>(error))
                    .unwrap_err()
                    .kind(),
                expected
            );
            assert!(transient.get().is_none());
        }
        assert_eq!(
            *transient
                .get_or_init(|| Ok(vec!["after transient read errors".to_owned()]))
                .unwrap(),
            ["after transient read errors"]
        );
        assert_eq!(transient.loads(), 5);
    }

    #[test]
    fn cached_results_recover_from_read_errors_keep_mod_parse_errors_and_retry_absence() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = std::env::temp_dir().join(format!(
            "cached-result-layers-{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("base");
        let mod_dir = root.join("mod");
        std::fs::create_dir_all(base.join("map/strategicregions")).unwrap();
        std::fs::create_dir_all(mod_dir.join("map/strategicregions")).unwrap();
        let mod_region = mod_dir.join("map/strategicregions/mod_region.txt");
        let valid_source = "strategic_region = { id = 7 name = \"MOD_REGION\" provinces = { 1 } }";
        std::fs::write(&mod_region, valid_source).unwrap();

        let reads = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&reads);
        let layers = ResolvedLayers::from_folders(&[(&base, &[][..]), (&mod_dir, &[][..])])
            .unwrap()
            .with_read_trace(Arc::new(move |read| {
                if read.path == b"map/strategicregions/mod_region.txt" {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        let cache = layers.derived::<CachedResult<content_reader_regions::regions::StrategicRegions>>();

        let io_error = {
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                let _locked = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&mod_region)
                    .unwrap();
                cache
                    .get_or_init(|| content_reader_regions::regions::read(&layers))
                    .unwrap_err()
            }
            #[cfg(unix)]
            {
                let parent = mod_dir.join("map/strategicregions");
                let backup = mod_dir.join("map/strategicregions_bak");
                std::fs::rename(&parent, &backup).unwrap();
                std::fs::write(&parent, b"").unwrap();
                let error = cache
                    .get_or_init(|| content_reader_regions::regions::read(&layers))
                    .unwrap_err();
                std::fs::remove_file(&parent).unwrap();
                std::fs::rename(&backup, &parent).unwrap();
                error
            }
        };
        assert_ne!(io_error.kind(), io::ErrorKind::InvalidData, "{io_error}");
        assert!(cache.get().is_none());

        std::fs::write(
            &mod_region,
            "strategic_region = { id = 7 name = \"CHANGED\" provinces = { 1 } }",
        )
        .unwrap();
        let changed = cache
            .get_or_init(|| content_reader_regions::regions::read(&layers))
            .unwrap_err();
        assert_eq!(changed.kind(), io::ErrorKind::InvalidData);
        assert!(changed.to_string().contains("changed since verification"), "{changed}");
        assert!(cache.get().is_none());

        std::fs::write(&mod_region, valid_source).unwrap();
        let loaded = cache
            .get_or_init(|| content_reader_regions::regions::read(&layers))
            .unwrap();
        assert_eq!(loaded.regions[&7].name_key, "MOD_REGION");
        let reads_after_recovery = reads.load(Ordering::Relaxed);
        let again = cache
            .get_or_init(|| panic!("successful read was not kept"))
            .unwrap();
        assert!(Arc::ptr_eq(&loaded, &again));
        assert_eq!(cache.loads(), 3);
        assert_eq!(reads.load(Ordering::Relaxed), reads_after_recovery);

        let broken_dir = root.join("broken_mod");
        std::fs::create_dir_all(broken_dir.join("map/strategicregions")).unwrap();
        let broken_file = broken_dir.join("map/strategicregions/mod_broken.txt");
        std::fs::write(
            &broken_file,
            "strategic_region = { id = 9 name = \"BROKEN\" provinces = { 0 } }",
        )
        .unwrap();
        let broken_reads = Arc::new(AtomicUsize::new(0));
        let broken_count = Arc::clone(&broken_reads);
        let broken_layers =
            ResolvedLayers::from_folders(&[(&base, &[][..]), (&broken_dir, &[][..])])
                .unwrap()
                .with_read_trace(Arc::new(move |read| {
                    if read.path == b"map/strategicregions/mod_broken.txt" {
                        broken_count.fetch_add(1, Ordering::Relaxed);
                    }
                }));
        let broken_cache =
            broken_layers.derived::<CachedResult<content_reader_regions::regions::StrategicRegions>>();
        let first_broken = broken_cache
            .get_or_init(|| content_reader_regions::regions::read(&broken_layers))
            .unwrap_err();
        assert_eq!(first_broken.kind(), io::ErrorKind::InvalidData);
        assert!(content_reader_parser::fields::is_content_error(&first_broken));
        assert!(first_broken.to_string().contains("mod_broken.txt"), "{first_broken}");
        assert_eq!(broken_reads.load(Ordering::Relaxed), 1);
        for _ in 0..4 {
            let repeated = broken_cache
                .get_or_init(|| panic!("parse error was read again"))
                .unwrap_err();
            assert_eq!(repeated.to_string(), first_broken.to_string());
            assert!(content_reader_parser::fields::is_content_error(&repeated));
        }
        assert_eq!(broken_cache.loads(), 1);
        assert_eq!(broken_reads.load(Ordering::Relaxed), 1);

        let absent_cache =
            layers.derived::<CachedResult<content_reader_key_names::terrain::DefaultMap>>();
        for attempt in 1..=2 {
            let missing = absent_cache
                .get_or_init(|| content_reader_key_names::terrain::read_default_map(&layers))
                .unwrap_err();
            assert_eq!(missing.kind(), io::ErrorKind::NotFound, "{missing}");
            assert!(!content_reader_parser::fields::is_content_error(&missing));
            assert!(absent_cache.get().is_none());
            assert_eq!(absent_cache.loads(), attempt);
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn content_result_binding_shares_clones_and_resets_for_a_new_stack() {
        let (first, _first_folder) = folder_catalogue(&[]);
        let (second, _second_folder) = folder_catalogue(&[]);
        let first_cache = first.content.layers.derived::<CachedResult<Vec<String>>>();
        let clone_cache = first
            .content
            .layers
            .clone()
            .derived::<CachedResult<Vec<String>>>();
        let second_cache = second.content.layers.derived::<CachedResult<Vec<String>>>();
        let original = first_cache
            .get_or_init(|| Ok(vec!["first".into()]))
            .unwrap();
        assert!(Arc::ptr_eq(
            &original,
            &clone_cache
                .get_or_init(|| panic!("shared stack reparsed"))
                .unwrap()
        ));
        assert_eq!(
            *second_cache
                .get_or_init(|| Ok(vec!["second".into()]))
                .unwrap(),
            ["second"]
        );
        assert_eq!(
            (
                first_cache.loads(),
                clone_cache.loads(),
                second_cache.loads()
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn presentation_titles_distinguish_missing_empty_and_authored_key_text() {
        let (catalogue, _folder) = folder_catalogue(&[(
            "localisation/titles_l_english.yml",
            "\u{feff}l_english:\n NAMED:0 \"Authored title\"\n EMPTY:0 \"\"\n SAME:0 \"SAME\"\n",
        )]);
        assert_eq!(
            catalogue.localised("NAMED", View::Plain).as_deref(),
            Some("Authored title")
        );
        assert_eq!(
            catalogue.localised("EMPTY", View::Plain).as_deref(),
            Some("")
        );
        assert_eq!(
            catalogue.localised("SAME", View::Plain).as_deref(),
            Some("SAME")
        );
        assert_eq!(catalogue.localised("MISSING", View::Plain), None);
        assert_eq!(catalogue.localised("", View::Plain).as_deref(), Some(""));
        assert_eq!(
            catalogue.localised("NAMED\0suffix", View::Plain).as_deref(),
            Some("Authored title")
        );
        assert_eq!(
            localised(&catalogue.localisation, "MISSING", View::Plain).unwrap(),
            "MISSING"
        );
    }

    #[test]
    fn missing_titles_name_their_refusal_in_the_callers_reply() {
        let (catalogue, _folder) = folder_catalogue(&[]);
        let refusals = Mutex::new(Refusals::default());
        let before = catalogue.refusal_lines();
        for _ in 0..2 {
            assert_eq!(
                catalogue.localised_with_refusals(
                    "mod_ideology\0suffix",
                    &[],
                    View::Plain,
                    &refusals,
                ),
                None
            );
        }
        let expected = "localisation mod_ideology is missing; title is unavailable";
        let caller = refusals.lock().unwrap().counted_diagnostic_lines("titles");
        assert_eq!(caller.len(), 1);
        assert!(caller[0].0.contains(expected));
        assert_eq!(caller[0].1, 1);
        assert_eq!(catalogue.refusal_lines(), before);
    }

    #[test]
    fn saved_name_bytes_follow_native_nul_and_preserve_empty_authored_text() {
        let localisation = loaded("l_english:\n KEY:0 \"Native name\"\n EMPTY:0 \"\"\n");
        assert_eq!(
            localised_bytes(&localisation, b"KEY\0\xff", View::Plain).unwrap(),
            "Native name"
        );
        assert_eq!(
            localised_bytes(&localisation, b"EMPTY", View::Plain).unwrap(),
            ""
        );
        assert_eq!(
            localised_bytes(&localisation, b"\0\xff", View::Plain).unwrap(),
            ""
        );
        assert!(localised_bytes(&localisation, b"\xff", View::Plain).is_err());
        assert_eq!(
            localised_bytes(&localisation, b"[ROOT.GetName]", View::Plain).unwrap(),
            "[ROOT.GetName]"
        );
        assert!(localised_bytes(&localisation, b"[ROOT.GetName]", View::Scripted).is_err());
    }

    #[test]
    fn gui_sprite_binding_uses_native_inheritance_and_kind_local_names() {
        game::enter();
        use content_reader_gui::gui::native;
        let gui = native::read(
            br#"guiTypes = { containerWindowType = {
            name = topbar
            buttonType = { name = base spriteType = "GFX_inherited" }
            containerWindowType = { name = nested
                buttonType = { name = player_flag spriteType = "GFX_wrong_window" }
            }
            buttonType = { name = player_flag parent = base }
        } }"#,
        );
        let guis = native::Guis::new(vec![("interface/topbar.gui".into(), gui)]);
        let mut warnings = Vec::new();
        assert_eq!(
            content_reader_gui::gui::native::window_button_sprite(
                &guis,
                "TOPBAR",
                "PLAYER_FLAG",
                &mut warnings
            )
            .unwrap(),
            "GFX_inherited"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_missing_gui_sprite_uses_the_native_fallback_but_a_refused_one_does_not() {
        game::enter();
        let facts = content_reader_gui::gui::native::sprites::read_sprites(
            br#"spriteTypes = {
            spriteType = { name = GFX_default_fallback_texture texturefile = fallback.dds }
            spriteType = { name = refused noOfFrames = -1 }
        }"#,
        );
        let sprites = SpriteSet::new(&[("interface/test.gfx".to_owned(), facts)]);
        assert_eq!(
            created_gui_sprite(&sprites, "absent").unwrap().name,
            "GFX_default_fallback_texture"
        );
        assert!(
            created_gui_sprite(&sprites, "refused")
                .unwrap_err()
                .to_string()
                .contains("noOfFrames")
        );
        let why = created_gui_sprite(&SpriteSet::default(), "missing")
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("missing") && why.contains("GFX_default_fallback_texture"),
            "{why}"
        );
    }

    #[test]
    fn text_icon_sprite_selects_native_prefix_then_bare_and_keeps_named_refusal() {
        game::enter();
        let facts = content_reader_gui::gui::native::sprites::read_sprites(
            br#"spriteTypes = {
                spriteType = { name = GFX_mod_alias texturefile = prefixed.dds }
                spriteType = { name = mod_alias texturefile = bare.dds }
                spriteType = { name = bare_only texturefile = bare_only.dds }
                spriteType = { name = GFX_refused noOfFrames = -1 }
                spriteType = { name = refused texturefile = must_not_stand_in.dds }
            }"#,
        );
        let sprites = SpriteSet::new(&[("interface/synthetic.gfx".to_owned(), facts)]);
        assert_eq!(
            text_icon_sprite(&sprites, "mod_alias").unwrap().name,
            "GFX_mod_alias"
        );
        assert_eq!(
            text_icon_sprite(&sprites, "bare_only").unwrap().name,
            "bare_only"
        );
        assert!(
            text_icon_sprite(&sprites, "refused")
                .unwrap_err()
                .to_string()
                .contains("noOfFrames")
        );
        assert!(text_icon_sprite(&sprites, "missing").is_err());
    }

    #[test]
    fn a_text_icon_routes_to_its_stems_sprite_and_the_frame_its_suffix_names() {
        use content_reader_gui::gui::native::sprites::{Plain, SpriteFacts, SpriteKind, Sprites};
        let sprite = |name: &str| SpriteFacts {
            name: name.as_bytes().to_vec(),
            load_type: 0,
            kind: SpriteKind::Plain(Plain { texture: b"strip.dds".to_vec(), frames: 4, ..Plain::default() }),
            refusals: Vec::new(),
        };
        let facts = Sprites { sprites: vec![sprite("GFX_mod_alias"), sprite("mod_alias"), sprite("bare_only")], ..Sprites::default() };
        let sprites = SpriteSet::new(&[("interface/synthetic.gfx".to_owned(), facts)]);
        let routed = |name: &str| text_icon(&sprites, name).map(|(sprite, frame)| (sprite.name.clone(), frame));
        assert_eq!(routed("mod_alias|3").unwrap(), ("GFX_mod_alias".to_owned(), 2), "a frame suffix draws that frame of the sprite its stem names");
        assert_eq!(routed("mod_alias").unwrap(), ("GFX_mod_alias".to_owned(), 0), "the native prefix is chosen before the bare name");
        assert_eq!(routed("bare_only|1").unwrap(), ("bare_only".to_owned(), 0));
        assert!(routed("mod_alias|0").unwrap_err().to_string().contains("names no frame"));
        assert!(routed("missing|2").is_err(), "an undeclared stem is refused by name");
        let colours = [[200, 20, 20, 255], [20, 200, 20, 255], [20, 20, 200, 255], [200, 200, 20, 255]];
        let strip = Rgba8Image { width: 4, height: 1, pixels: colours.concat() };
        let drawn = |name: &str| {
            let (sprite, frame) = text_icon(&sprites, name).unwrap();
            let SpriteKind::Plain(plain) = &sprite.facts.kind else { unreachable!("a plain sprite") };
            native_frame(&strip, plain, frame, 96).unwrap().pixels
        };
        assert_eq!(drawn("mod_alias"), colours[0], "no suffix draws the first frame's colour");
        assert_eq!(drawn("mod_alias|3"), colours[2], "the suffix draws its own frame's colour, never the first");
    }

    #[test]
    fn gui_artwork_uses_the_texture_resolver_and_client_power_of_two_setting() {
        game::enter();
        let (_, folder) = folder_catalogue(&[("gfx/interface/present.txt", "")]);
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12] = 3;
        tga[14] = 1;
        tga[16] = 32;
        tga[17] = 0x28;
        tga.extend_from_slice(&[0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]);
        tga.extend_from_slice(&[0; 8]);
        tga.extend_from_slice(b"TRUEVISION-XFILE.\0");
        std::fs::write(folder.0.join("gfx/interface/icon.tga"), tga).unwrap();
        let layers = ResolvedLayers::from_folders(&[(&folder.0, &[][..])]).unwrap();
        let image = gui_texture(
            &layers,
            b"gfx/interface/icon.png",
            content_reader_image::native::Decode {
                force_pow2_textures: true,
            },
        )
        .unwrap();
        assert_eq!((image.width, image.height), (4, 1));
        assert_eq!(&image.pixels[12..16], &[0, 0, 0, 255]);
    }

    #[cfg(test)]
    fn fixture() -> Localisation {
        loaded(
            &(String::from(concat!(
                "l_english:\n",
                " NAME:0 \"§YReal name§!\\nSecond line\"\n",
                " NESTED:0 \"Leader of $NAME$\"\n",
                " SCOPED:0 \"[ROOT.GetName]\"\n",
                " BROKEN:0 \"a$MISSING$b\"\n",
                " ICON:0 \"£pol_power 100§§ @GER£x\"\n",
                " GAP:0 \"a¤b\"\n",
                " BRACKET:0 \"A]B\"\n",
                " OPEN:0 \"A[B\"\n",
                " PRICE:0 \"Price $$5\"\n",
                " LONG:0 \"",
            )) + &"é".repeat(0xC00)
                + "x\"\n"),
        )
    }

    pub fn loaded(yml: &str) -> Localisation {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hoi4_dashboard_localisation_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _folder = Folder(root.clone());
        let file = root.join("localisation/test_l_english.yml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("\u{feff}{yml}")).unwrap();
        let layers = ResolvedLayers::from_folders(&[(&root, &[][..])]).unwrap();
        content_reader_localisation::localisation::read(&layers, "english").unwrap()
    }

    pub fn loaded_templates(text: &str) -> Arc<content_reader_politics::faction_templates::Catalog> {
        let (catalogue, _folder) =
            folder_catalogue(&[("common/factions/templates/test.txt", text)]);
        catalogue.content.layers.faction_templates().unwrap()
    }

    pub struct Folder(pub std::path::PathBuf);

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_folder(files: &[(&str, &str)]) -> (ResolvedLayers, Folder) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hoi4_dashboard_catalogue_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let folder = Folder(root.clone());
        std::fs::create_dir_all(&root).unwrap();
        if files.iter().any(|(path, _)| path.ends_with(".gfx")) {
            super::texture_tests::write_sprite_program(&root);
        }
        for (path, text) in files {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
        }
        let layers = ResolvedLayers::from_folders(&[(&root, &[][..])]).unwrap();
        (layers, folder)
    }

    #[test]
    fn the_catalogues_defines_do_not_wait_on_scripted_localisation_reading() {
        let (catalogue, _folder) = folder_catalogue(&[("common/defines/00_defines.lua", "NDefines = { NProduction = { MAX_CIV_FACTORIES_PER_LINE = 37 } }")]);
        let refused = catalogue.scripted_content().err().expect("scripted localisation needs a define these defines lack");
        assert!(refused.contains("MAX_SCRIPTED_LOC_RECURSION"), "{refused}");
        let defines = catalogue.defines().expect("the defines are their own read");
        assert_eq!(defines.number("NProduction", "MAX_CIV_FACTORIES_PER_LINE").unwrap(), 37.0);
    }

    #[test]
    fn shared_defines_read_ahead_and_layer_callers_parse_once_per_stack() {
        let stack = |value| {
            let script = format!("NDefines={{Probe={{Value={value}}}}}");
            let (layers, folder) = write_folder(&[("common/defines/probe.lua", &script)]);
            (layers_catalogue(layers), folder)
        };
        let (first, _first_folder) = stack(37);
        let (second, _second_folder) = stack(83);
        for (catalogue, expected) in [(&first, 37), (&second, 83)] {
            let layers = &catalogue.content.layers;
            let _generation = layers.enter_generation();
            let parses = || layers.product_counts().iter()
                .filter(|product| product.parser == "defines Lua 5.1")
                .map(|product| product.builds)
                .sum::<u64>();
            assert_eq!(parses(), 0);
            catalogue.read_readers();
            assert_eq!(parses(), 1);
            assert!(catalogue.defines.settled.get().is_some());
            assert_eq!(defines_for_layers(layers).unwrap().int32("Probe", "Value").unwrap(), expected);
            assert_eq!(catalogue.defines().unwrap().int32("Probe", "Value").unwrap(), expected);
            assert_eq!(parses(), 1);
        }
    }

    #[test]
    fn shared_defines_keep_a_layers_first_script_error_for_the_catalogue() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        for scoped in [false, true] {
            let reads = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&reads);
            let (layers, _folder) = write_folder(&[("common/defines/refused.lua", "error('refused probe')")]);
            let layers = layers.with_read_trace(Arc::new(move |read| {
                if read.path == b"common/defines/refused.lua" {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
            let _generation = scoped.then(|| layers.enter_generation());
            let first = defines_for_layers(&layers).unwrap_err();
            assert_eq!(first.kind(), io::ErrorKind::InvalidData);
            assert!(content_reader_defines::defines::is_script_error(&first));
            let reason = first.to_string();
            assert!(reason.contains("common/defines/refused.lua") && reason.contains("refused probe"), "{reason}");
            assert_eq!(reads.load(Ordering::Relaxed), 1);
            let catalogue = layers_catalogue(layers.clone());
            let again = catalogue.read_defines().unwrap_err();
            assert_eq!(again.kind(), first.kind());
            assert!(content_reader_defines::defines::is_script_error(&again));
            assert_eq!(catalogue.defines().unwrap_err(), reason);
            assert_eq!(defines_for_layers(&layers).unwrap_err().to_string(), reason);
            assert_eq!(reads.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn shared_defines_retry_after_io_and_verification_failures_on_both_paths() {
        let source = "NDefines={Probe={Value=42}}";
        let (layers, folder) = write_folder(&[("common/defines/probe.lua", source)]);
        let catalogue = layers_catalogue(layers.clone());
        let file = folder.0.join("common/defines/probe.lua");

        let (io_err, cat_err) = {
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                let _locked = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&file)
                    .unwrap();
                (defines_for_layers(&layers).unwrap_err(), catalogue.read_defines().unwrap_err())
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
                let first = defines_for_layers(&layers).unwrap_err();
                let second = catalogue.read_defines().unwrap_err();
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
                (first, second)
            }
        };
        assert_ne!(io_err.kind(), io::ErrorKind::InvalidData, "{io_err}");
        assert!(!content_reader_defines::defines::is_script_error(&io_err), "{io_err}");
        assert_ne!(cat_err.kind(), io::ErrorKind::InvalidData, "{cat_err}");

        std::fs::write(&file, "NDefines={Probe={Value=99}}").unwrap();
        let changed = catalogue.read_defines().unwrap_err();
        assert_eq!(changed.kind(), io::ErrorKind::InvalidData);
        assert!(changed.to_string().contains("changed since verification"), "{changed}");
        assert!(!content_reader_defines::defines::is_script_error(&changed));

        std::fs::write(&file, source).unwrap();
        assert_eq!(defines_for_layers(&layers).unwrap().int32("Probe", "Value").unwrap(), 42);
        assert_eq!(catalogue.defines().unwrap().int32("Probe", "Value").unwrap(), 42);
    }

    pub fn folder_catalogue(files: &[(&str, &str)]) -> (Catalogue, Folder) {
        let (layers, folder) = write_folder(files);
        (layers_catalogue(layers), folder)
    }

    pub fn localisation_catalogue(files: &[(&str, &str)]) -> (Catalogue, Folder) {
        let (layers, folder) = write_folder(files);
        (layers_catalogue_without_tables(layers), folder)
    }

    pub fn with_effect_describer(
        mut catalogue: Catalogue,
        factory: EffectDescriberFactory,
    ) -> Catalogue {
        assert!(catalogue.scripted.peek().is_none());
        catalogue.effect_describer = Some(factory);
        catalogue
    }

    pub fn layers_catalogue(layers: ResolvedLayers) -> Catalogue {
        if engine_executable::executable_tables::current().is_none() {
            game::enter();
        }
        layers_catalogue_without_tables(layers)
    }

    fn layers_catalogue_without_tables(layers: ResolvedLayers) -> Catalogue {
        let localisation = content_reader_localisation::localisation::read(&layers, "english").unwrap();
        let sprites = content_reader_gfx::gfx::read(&layers).unwrap();
        Catalogue {
            defines: layers.derived(),
            content: Arc::new(ResolvedContent {
                layers,
                stats: Default::default(),
                changed: Vec::new(),
                notes: Vec::new(),
            }),
            presentation: Presentation::default(),
            localisation: Arc::new(localisation),
            sprites,
            guis: Default::default(),
            presentation_localisation: Default::default(),
            gui_decode: Ok(Default::default()),
            verification: Vec::new(),
            warnings: Vec::new(),
            recorded_language: "english".into(),
            refused: Mutex::default(),
            findings: None,
            effect_describer: None,
            scripted: Once::new(),
            events: Default::default(),
            war_names: Once::new(),
            state_content: Arc::new(Once::new()),
            recorded: Once::new(),
            content_loads: Default::default(),
        }
    }

    #[test]
    fn participation_factors_follow_the_evaluated_military_define_stack() {
        game::enter();
        let (_, base) = folder_catalogue(&[(
            "common/defines/00_base.lua",
            "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NMilitary={
                WAR_SCORE_LEND_LEASE_GIVEN_IC_FACTOR=1.25,
                WAR_SCORE_LEND_LEASE_GIVEN_FUEL_FACTOR=0.5,
                WAR_SCORE_LEND_LEASE_RECEIVED_IC_FACTOR=2.5,
                WAR_SCORE_LEND_LEASE_RECEIVED_FUEL_FACTOR=0.25}}",
        )]);
        let (_, overlay) = folder_catalogue(&[(
            "common/defines/99_override.lua",
            "NDefines.NMilitary.WAR_SCORE_LEND_LEASE_GIVEN_IC_FACTOR =
                NDefines.NMilitary.WAR_SCORE_LEND_LEASE_GIVEN_IC_FACTOR * 2
             NDefines.NMilitary.WAR_SCORE_LEND_LEASE_RECEIVED_FUEL_FACTOR = -0.125",
        )]);
        let layers =
            ResolvedLayers::from_folders(&[(&base.0, &[][..]), (&overlay.0, &[][..])]).unwrap();
        let catalogue = layers_catalogue(layers);
        assert_eq!(
            catalogue.war_participation_factors().unwrap(),
            replay_data::war_participation::Factors {
                industry_given: 250_000,
                fuel_given: 50_000,
                industry_received: 250_000,
                fuel_received: -12_500,
            }
        );
    }

    #[test]
    fn faction_templates_share_one_cache_and_report_each_warning_once() {
        game::enter();
        let (catalogue, _folder) = folder_catalogue(&[
            (
                "common/factions/templates/test.txt",
                "bad={name=token:missing_name visible=yes}",
            ),
            (
                "common/defines/test.lua",
                "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}",
            ),
        ]);
        assert!(catalogue.refusal_lines().is_empty());
        let templates = catalogue.content.layers.faction_templates().unwrap();
        let scripted = catalogue.scripted_content().unwrap();
        let mut frame = crate::test_fixtures::source(1, 100, Some(1));
        *frame.tables[replay_data::rows::TableId::Factions.index()] =
            replay_frame::frame::TableView::Complete(replay_data::rows::TableRows::Factions(vec![
                replay_data::rows::FactionRow {
                    id: 5,
                    template_token: Some(17),
                    ..Default::default()
                },
            ]));
        *frame.tables[replay_data::rows::TableId::LexicalTokens.index()] =
            replay_frame::frame::TableView::Complete(replay_data::rows::TableRows::LexicalTokens(
                vec![replay_data::rows::LexicalTokenRow {
                    token: 17,
                    literal: b"bad".to_vec(),
                }],
            ));
        let at =
            crate::scripted_text::ScriptedText::new(&frame, &catalogue.localisation, scripted, 0);
        assert!(
            at.faction_name_by_id(5)
                .unwrap_err()
                .contains("token missing_name")
        );
        let shared = scripted.layers.layers.faction_templates().unwrap();
        assert!(Arc::ptr_eq(&templates, &shared));
        assert!(!templates.warnings.is_empty());
        let lines = catalogue.refusal_lines();
        for warning in &templates.warnings {
            assert_eq!(lines.iter().filter(|line| *line == warning).count(), 1);
        }
    }

    #[test]
    fn plain_text_reads_the_stored_controls() {
        let loc = fixture();
        assert_eq!(localised(&loc, "ICON", View::Plain), Ok("100§ ".into()));
        assert_eq!(
            localised(&loc, "GAP", View::Plain),
            Err(
                "localisation GAP holds the control byte 0x14 (an authored `¤`), which plain text cannot show".into()
            )
        );
        assert_eq!(localised(&loc, "GAP", View::Plain).ok(), None);
    }

    #[test]
    fn localised_text_removes_colour_and_expands_references() {
        let loc = fixture();
        assert_eq!(
            localised(&loc, "NAME", View::Plain),
            Ok("Real name\nSecond line".into())
        );
        assert_eq!(
            localised(&loc, "NESTED", View::Plain),
            Ok("Leader of Real name\nSecond line".into())
        );
    }

    #[test]
    fn text_follows_the_games_localiser() {
        let loc = fixture();
        assert_eq!(localised(&loc, "PRICE", View::Plain), Ok("Price $5".into()));
        assert_eq!(localised(&loc, "BROKEN", View::Plain), Ok("ab".into()));
        assert_eq!(localised(&loc, "BRACKET", View::Plain), Ok("A]B".into()));
        assert_eq!(localised(&loc, "LONG", View::Plain), Ok("é".repeat(0xC00)));
        assert_eq!(
            localised(&loc, "Not a key", View::Plain).as_deref(),
            Ok("Not a key")
        );
        assert_eq!(
            localised(&loc, "ABSENT", View::Plain).as_deref(),
            Ok("ABSENT")
        );
    }

    #[test]
    fn brackets_follow_the_view() {
        let loc = fixture();
        assert_eq!(
            localised(&loc, "SCOPED", View::Plain),
            Ok("[ROOT.GetName]".into())
        );
        assert_eq!(localised(&loc, "OPEN", View::Plain), Ok("A[B".into()));
        assert_eq!(
            localised(&loc, "SCOPED", View::Scripted),
            Err(
                "localisation SCOPED holds the scripted expression \"[ROOT.GetName]\", which the game evaluates for the country the text is shown for, and this product does not pass that country".into()
            )
        );
        assert_eq!(localised(&loc, "OPEN", View::Scripted), Ok(String::new()));
        assert_eq!(localised(&loc, "BRACKET", View::Scripted), Ok("A]B".into()));
    }

    #[test]
    fn plain_and_recorded_text_share_recovery_and_refusal_kinds() {
        game::enter();
        let (catalogue, _folder) = folder_catalogue(&[
            (
                "localisation/mod_l_english.yml",
                concat!(
                    "\u{feff}l_english:\n",
                    " BROKEN:0 \"a$MISSING$b\"\n",
                    " GAP:0 \"a¤b\"\n",
                    " CUT:0 \"$VALUE$\"\n",
                    " SCOPED:0 \"§Y[ROOT.GetTag]§!\"\n",
                ),
            ),
            (
                "common/defines/mod.lua",
                "NDefines = { NGame = { MAX_SCRIPTED_LOC_RECURSION = 23 } }",
            ),
        ]);
        let frame = crate::scope::tests::positive_frame();
        let scripted = crate::scripted_text::ScriptedText::new(
            &frame,
            &catalogue.localisation,
            catalogue.scripted_content().unwrap(),
            1,
        );
        let cut = format!("a{}", "é".repeat(0xC00));
        let arguments = [("VALUE", Argument::Text(&cut))];
        for (key, refusal) in [
            ("ABSENT", None),
            ("BROKEN", None),
            ("GAP", Some(Kind::Unsupported)),
            ("CUT", Some(Kind::Refused)),
        ] {
            let plain = localised_with_reason(
                &catalogue.localisation,
                key,
                &arguments,
                View::Plain,
                Kind::Unsupported,
            );
            let recorded =
                localised_in_details(&catalogue.localisation, key, &arguments, &scripted);
            assert_eq!(plain, recorded, "{key}");
            assert_eq!(plain.0.as_ref().err().map(|f| f.kind), refusal, "{key}");
            assert!(plain.1.iter().all(|f| f.kind == Kind::Content), "{key}");
            if matches!(key, "ABSENT" | "BROKEN") {
                assert!(!plain.1.is_empty(), "{key}");
            }
        }
        assert_eq!(
            localised_in_details(&catalogue.localisation, "SCOPED", &[], &scripted),
            (Ok("GER".into()), Vec::new())
        );
        for kind in [Kind::Unsupported, Kind::Offline] {
            let (text, findings) =
                localised_details(&catalogue.localisation, "SCOPED", &[], View::Scripted, kind);
            assert_eq!(text, None);
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, kind);
            assert_eq!(findings[0].subject, "SCOPED");
        }
        assert_eq!(catalogue.localised("GAP", View::Plain), None);
        let warnings = catalogue.refusal_lines();
        assert_eq!(warnings.len(), 1);
        assert_eq!(catalogue.localise_in("GAP", &scripted), None);
        assert_eq!(catalogue.refusal_lines(), warnings);
        assert!(catalogue.take_findings().is_empty());
    }

    #[test]
    fn a_refusal_is_counted_once_per_identity() {
        let mut refusals = Refusals::default();
        refusals.note("why", "B");
        refusals.note("why", "A");
        refusals.note("why", "A");
        assert_eq!(refusals.counts("names are"), ["2 names are not shown: why"]);
        assert_eq!(
            refusals.lines("names are"),
            ["2 names are not shown: why; A, B"]
        );
        assert_eq!(
            refusals.counted_diagnostic_lines("names are not shown"),
            [("2 names are not shown: why; A, B".into(), 2)]
        );
        assert_eq!(
            refusals.diagnostic_lines("retained values affected"),
            ["2 retained values affected: why; A, B"]
        );
        assert_eq!(
            refusals.counted_diagnostic_lines("retained values affected"),
            [("2 retained values affected: why; A, B".into(), 2)]
        );
        refusals.note("another reason", "C");
        assert_eq!(
            refusals
                .counted_diagnostic_lines("names are not shown")
                .iter()
                .map(|(_, count)| count)
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn projected_refusal_sets_merge_without_duplicate_identities() {
        let mut first = Refusals::default();
        first.note("missing operand", "A");
        let mut next = Refusals::default();
        next.note("missing operand", "A");
        next.note("missing operand", "B");
        next.note("refused content", "C");
        let next: Refusals = serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
        first.merge(&next);
        first.merge(&next);
        assert_eq!(
            first.lines("Project texts"),
            [
                "2 Project texts not shown: missing operand; A, B",
                "1 Project texts not shown: refused content; C",
            ]
        );
    }

    #[test]
    fn bounded_refusals_deduplicate_and_bound_escaped_warning_output() {
        let mut refused = BoundedRefusals::new(1024);
        refused.note("Unsupported \"mod\" form", "operative \"one\"\nfield");
        let first = refused.check().unwrap();
        refused.note("Unsupported \"mod\" form", "operative \"one\"\nfield");
        assert_eq!(refused.check().unwrap(), first);
        refused.note("Unsupported \"mod\" form", "operative two field");
        let lines = refused.lines("operative fields");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("2 operative fields not shown:"));
        assert!(lines[0].contains("operative two field"));
        assert!(crate::transport::json_len(&lines, refused.check().unwrap()).is_some());
    }

    #[test]
    fn bounded_refusals_overflow_refuses_and_stops_storage_growth() {
        let mut refused = BoundedRefusals::new(1024);
        refused.note("Known refusal", "operative one field");
        let before = refused.bytes;
        let names = refused.refused.0.clone();
        refused.bytes = refused.budget - 1;
        refused.note("A new refusal", "operative two field");
        assert!(refused.check().is_err());
        assert_eq!(refused.refused.0, names);
        assert_eq!(refused.bytes, refused.budget - 1);
        struct NoConversion;
        impl From<NoConversion> for String {
            fn from(_: NoConversion) -> Self {
                panic!("overflowed refusal must not allocate another note")
            }
        }
        refused.note(NoConversion, NoConversion);
        assert!(refused.check().is_err());
        assert!(before > 0);
    }

    #[test]
    fn catalogue_refusal_entries_keep_shared_read_warnings_uncounted() {
        game::enter();
        let (catalogue, _folder) =
            folder_catalogue(&[("outside_policy/synthetic.txt", "independent fixture")]);
        let reads = catalogue.content.layers.report_uncovered_reads();
        let ((entries, lines, warnings), _) = content_reader_layers::content::diagnostics::capture(|| {
            catalogue
                .content
                .layers
                .read("outside_policy/synthetic.txt")
                .unwrap();
            catalogue.refuse("synthetic reason", "synthetic name");
            (
                catalogue.refusal_entries(),
                catalogue.refusal_lines(),
                catalogue.reader_warning_lines(),
            )
        });
        assert_eq!(
            entries,
            [
                (
                    "1 texts are not shown: synthetic reason; synthetic name".into(),
                    Some(1)
                ),
                (reads.warning().unwrap(), None),
            ]
        );
        assert_eq!(
            lines,
            entries
                .into_iter()
                .map(|(line, _)| line)
                .collect::<Vec<_>>()
        );
        assert_eq!(warnings, [reads.warning().unwrap()]);
    }

    #[test]
    fn own_refusal_scopes_report_only_their_own_refusals_and_survive_saving() {
        let (catalogue, _folder) = folder_catalogue(&[(
            "localisation/mod_l_english.yml",
            "\u{feff}l_english:\n GAP:0 \"a¤b\"\n OTHER:0 \"c¤d\"\n",
        )]);
        assert_eq!(catalogue.localised("OTHER", View::Plain), None);
        let mut own = OwnRefusals::default();
        let lines = own_refusals(&mut own, || {
            assert_eq!(catalogue.localised("GAP", View::Plain), None);
            catalogue.refusal_lines()
        });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("1 texts are not shown: ") && lines[0].ends_with("; GAP"),
            "{lines:?}"
        );
        catalogue.refuse("later reason", "LATER");
        let shared = catalogue.refusal_lines();
        assert_eq!(shared.len(), 3, "{shared:?}");
        let saved: OwnRefusals =
            serde_json::from_slice(&serde_json::to_vec(&own).unwrap()).unwrap();
        for mut restored in [own, saved] {
            assert_eq!(
                own_refusals(&mut restored, || catalogue.refusal_lines()),
                lines
            );
        }
        assert_eq!(catalogue.refusal_lines(), shared);
    }

    #[test]
    fn shared_reads_count_only_reads_outside_every_reporting_scope() {
        let (catalogue, _folder) = folder_catalogue(&[(
            "localisation/mod_l_english.yml",
            "\u{feff}l_english:\n GAP:0 \"a¤b\"\n",
        )]);
        let before = shared_reads();
        own_refusals(&mut OwnRefusals::default(), || {
            catalogue.localised("GAP", View::Plain);
            catalogue.refusal_lines();
            catalogue.reader_warning_lines();
        });
        assert_eq!(shared_reads(), before);
        capture(|| catalogue.refusal_entries());
        assert!(
            shared_reads() > before,
            "a capture reports the shared tally"
        );
        let after = shared_reads();
        catalogue.reader_warning_lines();
        assert!(shared_reads() > after);
    }

    #[test]
    fn cached_results_credit_their_read_refusals_to_every_consumer() {
        let (catalogue, _folder) = folder_catalogue(&[(
            "localisation/mod_l_english.yml",
            "\u{feff}l_english:\n GAP:0 \"a¤b\"\n",
        )]);
        let cache = KeyedResults::<String, Option<String>>::default();
        let key = "GAP".to_owned();
        let read = |consumer: &mut OwnRefusals| {
            own_refusals(consumer, || {
                cache
                    .get_or_init(&key, |key| Ok(catalogue.localised(key, View::Plain)))
                    .unwrap();
                catalogue.refusal_lines()
            })
        };
        let (mut first, mut second) = (OwnRefusals::default(), OwnRefusals::default());
        let lines = read(&mut first);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(read(&mut second), lines);
        assert_eq!(
            cache.loads(&key),
            1,
            "the second consumer used the cached result"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn memos_credit_their_render_refusals_to_every_consumer() {
        let (catalogue, _folder) = folder_catalogue(&[(
            "localisation/mod_l_english.yml",
            "\u{feff}l_english:\n GAP:0 \"a¤b\"\n",
        )]);
        let memo: std::sync::Mutex<Option<Drawn<Option<String>>>> = std::sync::Mutex::default();
        let mut renders = 0;
        let mut read = |consumer: &mut OwnRefusals| {
            own_refusals(consumer, || {
                let mut memo = memo.lock().unwrap();
                let drawn = memo.get_or_insert_with(|| {
                    renders += 1;
                    Drawn::new(|| catalogue.localised("GAP", View::Plain))
                });
                assert_eq!(*drawn.get(), None);
                catalogue.refusal_lines()
            })
        };
        let (mut first, mut second) = (OwnRefusals::default(), OwnRefusals::default());
        let lines = read(&mut first);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(read(&mut second), lines, "the second consumer used the memo");
        assert_eq!(renders, 1);
        assert_eq!(first, second);
    }

    #[test]
    fn own_scopes_report_the_warnings_of_the_readers_they_used() {
        game::enter();
        let files = [(
            "common/factions/templates/test.txt",
            "bad={name=token:missing_name visible=yes}",
        )];
        let (catalogue, _folder) = folder_catalogue(&files);
        let templates = catalogue.content.layers.faction_templates().unwrap();
        assert!(!templates.warnings.is_empty());
        assert_eq!(catalogue.reader_warning_lines(), templates.warnings);
        let mut unused = OwnRefusals::default();
        assert!(own_refusals(&mut unused, || catalogue.refusal_lines()).is_empty());
        let mut user = OwnRefusals::default();
        let lines = own_refusals(&mut user, || {
            used(Reader::FactionTemplates);
            catalogue.refusal_lines()
        });
        assert_eq!(lines, templates.warnings);
        let (fresh, _fresh_folder) = folder_catalogue(&files);
        let mut restored: OwnRefusals =
            serde_json::from_slice(&serde_json::to_vec(&user).unwrap()).unwrap();
        assert_eq!(
            own_refusals(&mut restored, || fresh.reader_warning_lines()),
            templates.warnings
        );
    }

    #[test]
    fn an_unwinding_scope_keeps_its_refusals_and_leaves_no_scope_behind() {
        let (catalogue, _folder) = folder_catalogue(&[]);
        let mut own = OwnRefusals::default();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            own_refusals::<()>(&mut own, || {
                catalogue.refuse("before the panic", "UNWOUND");
                panic!("synthetic observer panic");
            })
        }));
        assert!(unwound.is_err());
        assert!(SCOPES.with(|scopes| scopes.borrow().is_empty()));
        let mut later = OwnRefusals::default();
        assert!(own_refusals(&mut later, || catalogue.refusal_lines()).is_empty());
        assert_eq!(
            own_refusals(&mut own, || catalogue.refusal_lines()),
            ["1 texts are not shown: before the panic; UNWOUND"]
        );
    }

    #[test]
    fn own_scopes_leave_out_the_stacks_live_artwork_and_uncovered_read_diagnostics() {
        game::enter();
        let (catalogue, _folder) =
            folder_catalogue(&[("outside_policy/synthetic.txt", "another consumer's file")]);
        let reads = catalogue.content.layers.report_uncovered_reads();
        let mut own = OwnRefusals::default();
        let before = own_refusals(&mut own, || {
            catalogue.refuse("own reason", "OWN");
            catalogue.refusal_lines()
        });
        catalogue
            .content
            .layers
            .note_shader_warning("synthetic artwork-effect note".into());
        catalogue
            .content
            .layers
            .read("outside_policy/synthetic.txt")
            .unwrap();
        let after = own_refusals(&mut own, || catalogue.refusal_lines());
        assert_eq!(after, before);
        assert_eq!(after, ["1 texts are not shown: own reason; OWN"]);
        let shared = catalogue.refusal_lines();
        assert!(
            !shared.contains(&"synthetic artwork-effect note".to_owned()),
            "{shared:?}"
        );
        assert!(!shared.contains(&reads.warning().unwrap()), "{shared:?}");
    }

    #[test]
    fn native_frames_share_horizontal_vertical_fractional_and_downsampled_offsets() {
        let image = Rgba8Image {
            width: 4,
            height: 4,
            pixels: (0u8..16).flat_map(|value| [value, 0, 0, 255]).collect(),
        };
        let values = |sample: Rgba8Image| {
            sample
                .pixels
                .chunks_exact(4)
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>()
        };
        let strip = |frames, frame_direction| Plain {
            frames,
            frame_direction,
            ..Plain::default()
        };
        let sample = |plain: Plain, frame, max_edge| native_frame(&image, &plain, frame, max_edge);
        assert_eq!(
            values(sample(strip(2, 0), 1, 4).unwrap()),
            [2, 3, 6, 7, 10, 11, 14, 15]
        );
        assert_eq!(
            values(sample(strip(2, 1), 1, 4).unwrap()),
            [8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert_eq!(values(sample(strip(1, 0), 0, 2).unwrap()), [5, 7, 13, 15]);
        assert_eq!(values(sample(strip(3, 0), 1, 4).unwrap()), [2, 6, 10, 14]);
        assert!(sample(strip(5, 0), 0, 4).is_err());
        assert!(sample(strip(1, 0), 0, 0).is_err());
    }
}
