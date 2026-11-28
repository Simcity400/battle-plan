
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::AtomicBool,
    },
};

use content_reader_layers::content::diagnostics::{self, Diagnostics};
use content_reader_layers::{ResolveOptions, content::ResolvedContent};
use replay::campaign::Replay;

use crate::{
    ReplayMetadata,
    content::{ContentKey, content_key, name_localisation},
    metadata::{self, MetadataRefusals, ResidentMetadata, Warnings},
    presentation::{Admitted, Binding, Cancel, Presentation, Presented},
    products::ContentOptions,
};
use content_reader_country_names::country_names::{CountryName, CountryNames};
use content_reader_localisation::localisation::Localisation;
use replay::campaign::SeekResult;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceKey {
    pub source_id: String,
    pub revision: String,
}

type SharedState = Arc<dyn Any + Send + Sync>;

struct StateEntry {
    value: SharedState,
    append: Option<fn(&SharedState) -> SharedState>,
}

#[derive(Default)]
struct RecordedMount(Option<(Weak<ResolvedContent>, Arc<Presented>)>);

#[derive(Default)]
struct ContentCache {
    bound: Option<(ResolveOptions, Arc<ResolvedContent>)>,
    held: Vec<(ResolveOptions, Weak<ResolvedContent>)>,
}

impl ContentCache {
    fn find(&self, options: &ResolveOptions) -> Option<Arc<ResolvedContent>> {
        match &self.bound {
            Some((old, content)) if same_folders(old, options) => Some(Arc::clone(content)),
            _ => self
                .held
                .iter()
                .filter(|(old, _)| same_folders(old, options))
                .find_map(|(_, content)| content.upgrade()),
        }
    }

    fn bind(&mut self, options: &ResolveOptions, content: &Arc<ResolvedContent>) {
        let _ = content.layers.report_uncovered_reads();
        self.held
            .retain(|(old, held)| held.strong_count() > 0 && !same_folders(old, options));
        let previous = self.bound.replace((options.clone(), Arc::clone(content)));
        if let Some((old, previous)) = previous
            && !Arc::ptr_eq(&previous, content)
        {
            self.held.push((old, Arc::downgrade(&previous)));
        }
    }
}

fn refuse_older(error: io::Error) -> io::Error {
    if replay::campaign::is_older_layout(&error) {
        dashboard_core::refused(error.to_string())
    } else {
        error
    }
}

fn block_windows(replay: &Replay) -> HashMap<u64, Vec<crate::span::HourSpan>> {
    let mut windows: HashMap<_, Vec<_>> = HashMap::new();
    for block in &replay.index {
        windows
            .entry(block.branch)
            .or_default()
            .push(crate::span::HourSpan {
                first_hour: block.first_hour,
                last_hour: block.last_hour,
            });
    }
    windows
}

pub struct Source {
    key: SourceKey,
    path: PathBuf,
    replay: Arc<Replay>,
    block_windows: HashMap<u64, Vec<crate::span::HourSpan>>,
    metadata: ResidentMetadata,
    named: Mutex<Option<(NamesKey, ResidentMetadata, Diagnostics)>>,
    content: Mutex<ContentCache>,
    facts: std::sync::OnceLock<Result<Arc<content_reader_layers::content::facts::Pack>, String>>,
    tables: std::sync::OnceLock<Option<engine_executable::executable_tables::Tables>>,
    binding: Mutex<Option<Arc<Binding>>>,
    state: Mutex<HashMap<TypeId, StateEntry>>,
}

pub(crate) type NamesKey = Option<ContentKey>;

fn country_observation(country: &crate::CountryMetadata) -> (&str, &str, &str, i32) {
    (
        &country.id,
        &country.tag,
        &country.observed_branch_id,
        country.observed_hour,
    )
}

type Recorded = (
    Result<Arc<content_reader_layers::content::facts::Pack>, String>,
    Option<engine_executable::executable_tables::Tables>,
);

fn recorded(replay: &Arc<Replay>) -> Recorded {
    let facts = crate::facts::recorded(replay);
    let tables = facts.as_ref().ok().and_then(|pack| pack.tables().ok());
    (facts, tables)
}

impl Source {
    pub fn open(key: SourceKey, path: PathBuf, cancelled: &AtomicBool) -> io::Result<Self> {
        metadata::check_cancel(cancelled)?;
        let replay = Arc::new(Replay::open_timelines(&path).map_err(refuse_older)?);
        metadata::check_cancel(cancelled)?;
        let (facts, tables) = recorded(&replay);
        engine_executable::executable_tables::scope(tables, || {
            Self::read(key, path, replay, (facts, tables), cancelled)
        })
    }

    pub fn from_replay(
        key: SourceKey,
        path: PathBuf,
        replay: Arc<Replay>,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        let recorded = recorded(&replay);
        engine_executable::executable_tables::scope(recorded.1, || {
            Self::read(key, path, replay, recorded, cancelled)
        })
    }

    fn read(
        key: SourceKey,
        path: PathBuf,
        replay: Arc<Replay>,
        (facts, tables): Recorded,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        let metadata = metadata::read_resident(&replay, &path, cancelled)?;
        Ok(Self {
            key,
            path,
            block_windows: block_windows(&replay),
            replay,
            metadata,
            named: Mutex::new(None),
            content: Mutex::default(),
            facts: facts.into(),
            tables: tables.into(),
            binding: Mutex::new(None),
            state: Mutex::new(HashMap::new()),
        })
    }

    pub fn key(&self) -> &SourceKey {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn campaign_key(&self) -> &str {
        &self.metadata.campaign_key
    }

    pub fn replay(&self) -> Arc<Replay> {
        Arc::clone(&self.replay)
    }

    pub fn replay_ref(&self) -> &Replay {
        &self.replay
    }

    pub fn facts(&self) -> Result<Arc<content_reader_layers::content::facts::Pack>, String> {
        self.facts
            .get_or_init(|| crate::facts::recorded(&self.replay))
            .clone()
    }

    pub fn tables(&self) -> Option<engine_executable::executable_tables::Tables> {
        *self
            .tables
            .get_or_init(|| self.facts().ok().and_then(|pack| pack.tables().ok()))
    }

    pub fn in_scope<T>(&self, work: impl FnOnce() -> T) -> T {
        engine_executable::executable_tables::scope(self.tables(), work)
    }

    fn play(&self, options: &ResolveOptions) -> io::Result<ResolvedContent> {
        content_reader_layers::playback(&self.replay.identity, self.facts(), options)
    }
    pub fn block_windows(&self, branch: u64) -> &[crate::span::HourSpan] {
        self.block_windows.get(&branch).map_or(&[], Vec::as_slice)
    }

    pub fn metadata(&self) -> Result<ReplayMetadata, metadata::MetadataError> {
        let named = self.named.lock().unwrap_or_else(|error| error.into_inner());
        let mut metadata = match named.as_ref() {
            Some((_, metadata, diagnostics)) => {
                diagnostics.credit();
                let mut metadata = metadata.wire()?;
                diagnostics.append_to(&mut metadata.warnings);
                metadata
            }
            None => self.metadata.wire()?,
        };
        drop(named);
        self.append_content_warnings(&mut metadata.warnings);
        metadata.build_warnings = metadata::build_warnings(&self.replay);
        Ok(metadata)
    }

    pub fn append_content_warnings(&self, warnings: &mut Vec<String>) {
        diagnostics::current().append_to(warnings);
        if let Some((_, content)) = &self
            .content
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bound
        {
            content.append_changed_to(warnings);
        }
    }

    fn own_content(&self, options: &ResolveOptions) -> io::Result<Arc<ResolvedContent>> {
        let cache = self
            .content
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(content) = cache.find(options) {
            return Ok(content);
        }
        drop(cache);
        Ok(Arc::new(self.play(options)?))
    }

    pub fn changed_content(&self, content: &ContentOptions) -> Option<Arc<ResolvedContent>> {
        self.own_content(&content.options).ok()
    }

    pub fn presentation_refusals(&self, content: &ContentOptions) -> Option<Arc<Presented>> {
        let shown = content.presentation.content_work() && content.presentation.shows_mods();
        shown.then(|| self.presented_for(content).ok()).flatten()
    }

    pub fn name_countries(&self, content: Option<&ContentOptions>) -> io::Result<()> {
        let key = content_key(content);
        let mut named = self.named.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((cached, _, diagnostics)) = named.as_ref()
            && *cached == key
        {
            diagnostics.credit();
            return Ok(());
        }
        let (metadata, diagnostics) =
            diagnostics::capture(|| self.named_metadata_with(name_localisation(self, content), content, None));
        diagnostics.credit();
        *named = Some((key, metadata?, diagnostics));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn named_metadata(
        &self,
        localisation: Result<Arc<Localisation>, String>,
    ) -> io::Result<ResidentMetadata> {
        self.named_metadata_with(localisation, None, None)
    }

    #[cfg(test)]
    fn named_metadata_from(
        &self,
        localisation: Result<Arc<Localisation>, String>,
        previous: Option<&ResidentMetadata>,
    ) -> io::Result<ResidentMetadata> {
        self.named_metadata_with(localisation, None, previous)
    }

    fn named_metadata_with(
        &self,
        localisation: Result<Arc<Localisation>, String>,
        content: Option<&ContentOptions>,
        previous: Option<&ResidentMetadata>,
    ) -> io::Result<ResidentMetadata> {
        self.try_named_metadata_from(localisation, content, previous)
            .map_err(|error| io::Error::other(format!("Country metadata naming failed: {error}")))
    }

    fn try_named_metadata_from(
        &self,
        localisation: Result<Arc<Localisation>, String>,
        content: Option<&ContentOptions>,
        previous: Option<&ResidentMetadata>,
    ) -> io::Result<ResidentMetadata> {
        let mut metadata = self.metadata.copy_presentation()?;
        let saved_countries: HashMap<_, _> = previous
            .into_iter()
            .flat_map(|previous| &previous.countries)
            .map(|country| (country_observation(country), country))
            .collect();
        let reuse_warnings = localisation.is_err();
        let reason = match localisation {
            Err(reason) => {
                for country in &mut metadata.countries {
                    if let Some(saved) = saved_countries.get(&country_observation(country)) {
                        country.name.clone_from(&saved.name);
                    }
                }
                reason
            }
            Ok(localisation) => {
                let mut unnamed = 0;
                let mut frames = std::collections::BTreeMap::new();
                let mut scoped: Option<Option<crate::products::scoped_text::ScopedText<'static>>> = None;
                for country in &mut metadata.countries {
                    if let Some(saved) = saved_countries.get(&country_observation(country)) {
                        country.name.clone_from(&saved.name);
                        continue;
                    }
                    let at = (country.observed_branch_id.clone(), country.observed_hour);
                    let frame = match frames.entry(at) {
                        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            let frame = if let Ok(branch) = country.observed_branch_id.parse() {
                                let tables =
                                    Some(&content_reader_country_names::country_names::TABLES[..]);
                                match self.replay.seek_selected(
                                    branch,
                                    country.observed_hour,
                                    tables,
                                ) {
                                    Ok(SeekResult::Frame(frame)) => {
                                        Some(CountryNames::read(&frame, &localisation))
                                    }
                                    Err(error)
                                        if engine_allocation::is_allocation_error(&error) =>
                                    {
                                        return Err(error);
                                    }
                                    _ => None,
                                }
                            } else {
                                None
                            };
                            entry.insert(frame)
                        }
                    };
                    let name = match frame.as_ref().zip(country.id.parse::<u32>().ok()) {
                        Some((names, slot)) => match names.resolve_name(slot) {
                            Ok(CountryName::Plain(name)) => Some(name),
                            Ok(CountryName::Scripted { key, text }) => {
                                if let Ok(branch) = country.observed_branch_id.parse() {
                                    let evaluator = match &scoped {
                                        Some(evaluator) => evaluator.as_ref(),
                                        None => {
                                            let loaded = match content {
                                                Some(opts) => match crate::catalogue::recorded_catalogue(self, opts) {
                                                    Ok(cat) => Some(crate::products::scoped_text::ScopedText::owned(
                                                        Arc::clone(&self.replay),
                                                        Ok(cat),
                                                    )),
                                                    Err(error) if engine_allocation::is_allocation_error(&error) => {
                                                        return Err(error);
                                                    }
                                                    Err(_) => None,
                                                },
                                                None => None,
                                            };
                                            scoped.insert(loaded).as_ref()
                                        }
                                    };
                                    match evaluator {
                                        Some(evaluator) => evaluator
                                            .expand_raw_at_branch(
                                                branch,
                                                country.observed_hour,
                                                crate::products::scoped_text::Scope::Country(slot),
                                                &key,
                                                &text,
                                            )?
                                            .ok(),
                                        None => None,
                                    }
                                } else {
                                    None
                                }
                            }
                            Err(_) => None,
                        },
                        None => None,
                    };
                    match name {
                        Some(name) => country.name = metadata::short_text(&name, 512),
                        None => unnamed += 1,
                    }
                }
                if unnamed > 0 {
                    replay_data::codec::try_format(format_args!(
                        "{unnamed} country names could not be resolved from the recorded content and show their tag."
                    ))?
                } else {
                    String::new()
                }
            }
        };
        let mut warnings = Warnings::new()?;
        warnings.import(&self.metadata.warnings, &self.metadata.refusals.warnings)?;
        if !reason.is_empty() {
            warnings.warn(&reason)?;
        }
        if reuse_warnings && let Some(previous) = previous {
            warnings.import(&previous.warnings, &previous.refusals.warnings)?;
        }
        let mut reasons = Warnings::new()?;
        if let Some(damaged) = &self.metadata.damaged {
            reasons.import(&damaged.reasons, &self.metadata.refusals.reasons)?;
        }
        let (values, warning_refusals) = warnings.finish();
        metadata.warnings = values;
        let (values, reason_refusals) = reasons.finish();
        if let Some(damaged) = &mut metadata.damaged {
            damaged.reasons = values;
        }
        Ok(ResidentMetadata {
            value: metadata,
            refusals: MetadataRefusals {
                warnings: warning_refusals,
                reasons: reason_refusals,
            },
        })
    }

    pub fn content(&self, options: &ResolveOptions) -> io::Result<Arc<ResolvedContent>> {
        loop {
            let (content, rebind) = {
                let mut cache = self.content.lock().map_err(|error| io::Error::other(error.to_string()))?;
                match cache.find(options) {
                    Some(content) => {
                        let rebind = !cache.bound.as_ref().is_some_and(|(_, bound)| Arc::ptr_eq(bound, &content));
                        (content, rebind)
                    }
                    None => {
                        let content = {
                            let _memory = engine_allocation::labels::enter(
                                engine_allocation::labels::Owner::RUN,
                                engine_allocation::labels::Kind::Content,
                            );
                            Arc::new(self.play(options)?)
                        };
                        cache.bind(options, &content);
                        (content, false)
                    }
                }
            };
            if content.layers.admit_generation().is_err() {
                return self.refresh_content(options, &content);
            }
            if !rebind { return Ok(content); }
            let mut cache = self.content.lock().map_err(|error| io::Error::other(error.to_string()))?;
            if !cache.find(options).is_some_and(|current| Arc::ptr_eq(&current, &content)) {
                continue;
            }
            if !cache.bound.as_ref().is_some_and(|(_, bound)| Arc::ptr_eq(bound, &content)) {
                cache.bind(options, &content);
            }
            return Ok(content);
        }
    }

    pub fn refresh_content(
        &self,
        options: &ResolveOptions,
        expected: &Arc<ResolvedContent>,
    ) -> io::Result<Arc<ResolvedContent>> {
        let mut expected = Arc::clone(expected);
        loop {
            let mut cache = self.content.lock().map_err(|error| io::Error::other(error.to_string()))?;
            if let Some(current) = cache.find(options)
                && !Arc::ptr_eq(&current, &expected)
            {
                drop(cache);
                if current.layers.admit_generation().is_ok() { return self.content(options); }
                expected = current;
                continue;
            }
            let _generation = content_reader_layers::content::Generation::enter();
            let current = Arc::new(self.play(options)?);
            cache.bind(options, &current);
            drop(cache);
            current.layers.admit_generation()?;
            return Ok(current);
        }
    }

    pub fn bound_content(&self, options: &ResolveOptions) -> Option<Arc<ResolvedContent>> {
        self.content
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bound
            .as_ref()
            .filter(|(old, _)| same_folders(old, options))
            .map(|(_, content)| Arc::clone(content))
    }

    pub fn binding(&self) -> Option<Arc<Binding>> {
        self.binding
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn presentation(&self) -> Presentation {
        self.binding()
            .map(|binding| binding.presentation.clone())
            .unwrap_or_default()
    }

    pub fn bind(
        &self,
        options: Option<(&ResolveOptions, bool)>,
        presentation: Presentation,
    ) -> bool {
        let mut binding = self
            .binding
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some((options, allow_unverified)) = options else {
            let retired = binding.take();
            retired.iter().for_each(|retired| retired.retire());
            return retired.is_some();
        };
        if binding
            .as_ref()
            .is_some_and(|current| current.binds(options, allow_unverified, &presentation))
        {
            return false;
        }
        let replaced = binding.replace(Arc::new(Binding::new(
            options.clone(),
            allow_unverified,
            presentation,
        )));
        replaced.iter().for_each(|replaced| replaced.retire());
        true
    }

    pub fn admit(&self, work: Work) -> Option<ContentOptions> {
        let binding = self.binding()?;
        let options = binding.options.clone();
        let allow_unverified = binding.allow_unverified;
        let presentation = match work {
            Work::Content(cancel) => Admitted::admitted(binding, true, cancel),
            Work::Ahead => {
                let cancel = Arc::clone(&binding) as Arc<dyn Cancel>;
                Admitted::admitted(binding, false, cancel)
            }
            Work::Recorded => Admitted::recorded(binding),
        };
        Some(ContentOptions {
            options,
            allow_unverified,
            presentation,
        })
    }

    pub fn still_admitted(&self, content: &ContentOptions) -> bool {
        match self.binding() {
            Some(current) => Arc::ptr_eq(&current, content.presentation.binding()),
            None => content.presentation.identity().is_empty(),
        }
    }

    pub fn admit_resolved(&self, work: Work) -> Option<ContentOptions> {
        let mut content = self.admit(work)?;
        self.resolve(&mut content);
        Some(content)
    }

    pub fn resolve(&self, content: &mut ContentOptions) {
        self.resolve_with(content, |options| self.content(options));
    }

    fn resolve_with(&self, content: &mut ContentOptions, recorded: impl FnOnce(&ResolveOptions) -> io::Result<Arc<ResolvedContent>>) {
        if !content.presentation.shows_mods() {
            return;
        }
        let admitted = &content.presentation;
        let mount = admitted
            .binding()
            .mount
            .get_or_mount(admitted.cancel(), || {
                let recorded = recorded(&content.options)?;
                Presented::mount(
                    &recorded,
                    &content.options,
                    &self.replay,
                    admitted.identity(),
                    admitted.cancel(),
                )
                .map(Arc::new)
            });
        content.presentation.set_mount(&mount);
    }

    pub fn presented_for(&self, content: &ContentOptions) -> io::Result<Arc<Presented>> {
        if !content.presentation.shows_mods() {
            return self.recorded_presented(&content.options);
        }
        content.presentation.mount().unwrap_or_else(|| {
            debug_assert!(
                false,
                "work showing visual mods reads art its entry did not resolve"
            );
            Err(io::Error::other(
                "The selected visual mods were not prepared for this work",
            ))
        })
    }

    pub fn resolve_bound(&self, content: &mut ContentOptions) {
        if let Some(recorded) = self.bound_content(&content.options) && self.still_admitted(content) {
            self.resolve_with(content, |_| Ok(recorded));
        }
    }

    pub fn bound_presented_for(&self, content: &ContentOptions) -> Option<Arc<Presented>> {
        let recorded = self.bound_content(&content.options)?;
        if !self.still_admitted(content) {
            return None;
        }
        if content.presentation.shows_mods() {
            return content.presentation.mount()?.ok();
        }
        self.recorded_presented_with(&content.options, recorded).ok()
    }

    pub fn recorded_presented(&self, options: &ResolveOptions) -> io::Result<Arc<Presented>> {
        self.recorded_presented_with(options, self.content(options)?)
    }

    fn recorded_presented_with(&self, options: &ResolveOptions, recorded: Arc<ResolvedContent>) -> io::Result<Arc<Presented>> {
        let state = self.state::<RecordedMount>();
        let mut kept = state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((content, presented)) = &kept.0
            && std::ptr::eq(content.as_ptr(), Arc::as_ptr(&recorded))
        {
            return Ok(Arc::clone(presented));
        }
        let presented = Arc::new(Presented::mount(
            &recorded,
            options,
            &self.replay,
            &Presentation::default(),
            &std::sync::atomic::AtomicBool::new(false),
        )?);
        kept.0 = Some((Arc::downgrade(&recorded), Arc::clone(&presented)));
        Ok(presented)
    }

    pub fn state<T: Default + Send + 'static>(&self) -> Arc<Mutex<T>> {
        let mut states = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = states
            .entry(TypeId::of::<T>())
            .or_insert_with(|| StateEntry {
                value: Arc::new(Mutex::new(T::default())),
                append: None,
            });
        Arc::clone(&entry.value)
            .downcast::<Mutex<T>>()
            .expect("state is keyed by its own type")
    }

    pub fn append_state<T: Default + Clone + Send + 'static>(&self) -> Arc<Mutex<T>> {
        fn fork<T: Clone + Send + 'static>(value: &SharedState) -> SharedState {
            let value = value.downcast_ref::<Mutex<T>>().expect("state type");
            Arc::new(Mutex::new(
                value
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone(),
            ))
        }
        let value = self.state::<T>();
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(&TypeId::of::<T>())
            .expect("state created")
            .append = Some(fork::<T>);
        value
    }

    pub fn inherit_append_states(&self, previous: &Self) {
        let opted: Vec<_> = previous
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .append
                    .map(|fork| (*id, Arc::clone(&entry.value), fork))
            })
            .collect();
        if opted.is_empty() || !self.is_append_of(previous) {
            return;
        }
        for (id, value, fork) in opted {
            let value = fork(&value);
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    id,
                    StateEntry {
                        value,
                        append: Some(fork),
                    },
                );
        }
    }

    pub fn is_append_of(&self, previous: &Self) -> bool {
        let old = previous.replay_ref();
        let new = self.replay_ref();
        if old.campaign_id != new.campaign_id
            || old.identity != new.identity
            || old.format != new.format
            || old.index.len() > new.index.len()
        {
            return false;
        }
        let held: std::collections::HashSet<&replay::campaign::BlockIndex> =
            new.index.iter().collect();
        if old.index.iter().any(|block| !held.contains(block)) {
            return false;
        }
        if old
            .sessions
            .iter()
            .any(|session| !new.sessions.contains(session))
        {
            return false;
        }
        if old
            .branches
            .iter()
            .any(|branch| !new.branches.contains(branch))
            || old.gaps.iter().any(|gap| !new.gaps.contains(gap))
        {
            return false;
        }
        !new.gaps
            .iter()
            .filter(|gap| !old.gaps.contains(gap))
            .any(|gap| {
                old.index
                    .iter()
                    .any(|block| block.branch == gap.branch && gap.first_hour <= block.last_hour)
            })
    }
}

pub enum Work {
    Content(Arc<dyn Cancel>),
    Ahead,
    Recorded,
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(binding) = self
            .binding
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            binding.retire();
        }
    }
}

fn same_folders(old: &ResolveOptions, options: &ResolveOptions) -> bool {
    old.game_dir == options.game_dir
        && old.user_dir == options.user_dir
        && old.cache_dir == options.cache_dir
}

#[cfg(test)]
mod tests;
