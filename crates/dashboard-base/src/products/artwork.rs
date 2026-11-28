use content_reader_key_names::key_name_products::KeyNamesExt as _;

pub mod resolver;
pub mod sprites;
mod prepare;

pub use prepare::{prepare_artwork, prepare_artwork_for};
pub use resolver::{
    ArtFile, ArtFrame, ArtOrigin, ArtProvenance, ArtRenderVariant, ArtResolution, ArtScopeKey,
    ArtTarget, CURRENT_INSTALL_ART_LABEL, CURRENT_INSTALL_ART_NOTICE, FlagSetKey, Searched, art_file, art_scope_key, relayed_searches,
    resolve, resolve_equipment_icon,
    resolve_flag, resolve_flag_pixels, resolve_portrait_entry, resolve_sprite,
    resolve_sprite_or_texture, resolve_sprite_type, resolve_sprite_type_on_layers,
    resolve_text_icon, resolve_texture, resolve_with_sprites, sprite_definition,
};

use super::ContentOptions;
use crate::{
    Artwork, FocusDatabase, Source,
    catalogue::{self, CachedResult, Catalogue},
    invalid,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const UNKNOWN_FOCUS_ICON: &str = content_reader_focus::focus::UNKNOWN_ICON;

pub const ICON_EDGE: u32 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Focus(FocusDatabase),
    Technology,
}

impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Self::Focus(FocusDatabase::National) => "focus",
            Self::Focus(FocusDatabase::Continuous) => "continuous focus",
            Self::Technology => "technology",
        }
    }
}

fn focus_database(database: FocusDatabase) -> content_reader_focus::focus::Database {
    match database {
        FocusDatabase::National => content_reader_focus::focus::Database::National,
        FocusDatabase::Continuous => content_reader_focus::focus::Database::Continuous,
    }
}

fn undeclared_icon(sprite: &str) -> String {
    format!("sprite {sprite} is undeclared or has no texture; a placeholder is shown")
}

fn declared_icon_png(catalogue: &Catalogue, sprite: &str) -> io::Result<Option<String>> {
    let Some(sprite) = catalogue.sprites.find(sprite).map_err(invalid)? else {
        return Ok(None);
    };
    if sprite.texture.is_none() {
        return Ok(None);
    }
    resolve_sprite_type(catalogue, sprite, 0, ICON_EDGE).into_io_result()
}

struct TechnologyName {
    database_index: Option<u32>,
    presentation: Result<content_reader_military::technology::PresentationName, String>,
    folders: Vec<String>,
    children: std::sync::OnceLock<catalogue::Drawn<Result<Vec<String>, String>>>,
}

#[derive(Default)]
struct TechnologyNames {
    names: BTreeMap<String, Vec<TechnologyName>>,
    unread: BTreeMap<String, String>,
    stopped: Option<String>,
    unplaced: Option<String>,
}

impl TechnologyNames {
    fn read(
        definitions: Vec<content_reader_military::technology::Template>,
        unread: Vec<content_reader_military::technology::UnreadFile>,
    ) -> Self {
        let mut out = Self::default();
        let mut unknown_place = None;
        for definition in definitions {
            if definition.database_index.is_none() && unknown_place.is_none() {
                unknown_place = Some(format!(
                    "technology {} and every later definition have no known place in load order",
                    definition.token
                ));
            }
            out.names
                .entry(definition.token.to_ascii_lowercase())
                .or_default()
                .push(TechnologyName {
                    database_index: definition.database_index,
                    presentation: definition.presentation,
                    folders: definition.folders,
                    children: std::sync::OnceLock::new(),
                });
        }
        for file in unread {
            out.unplaced.get_or_insert_with(|| {
                format!(
                    "{} is not read whole, so no later template has a known place",
                    file.why
                )
            });
            match file.tokens {
                Some(tokens) => {
                    for token in tokens {
                        out.unread.entry(token).or_insert_with(|| file.why.clone());
                    }
                }
                None => {
                    out.stopped.get_or_insert(file.why);
                }
            }
        }
        out.unplaced = out.unplaced.or(unknown_place);
        out
    }

    fn get(&self, token: &str) -> Option<&Vec<TechnologyName>> {
        self.names.get(&token.to_ascii_lowercase())
    }

    fn unread(&self, token: &str) -> Option<&str> {
        self.unread
            .get(&token.to_ascii_lowercase())
            .or(self.stopped.as_ref())
            .map(String::as_str)
    }

    fn may_define(&self, token: &str) -> bool {
        self.get(token).is_some() || self.unread(token).is_some()
    }
}

#[cfg(test)]
fn technology_name(
    presentation: &content_reader_military::technology::PresentationName,
    world: Result<&crate::triggers::World<'_>, String>,
    database: Option<&Arc<content_reader_key_names::equipment::Database>>,
) -> Result<String, String> {
    let mut children = technology_children(presentation, world, database)?;
    Ok(if children.len() == 1 {
        children.remove(0)
    } else {
        presentation.template.clone()
    })
}

fn technology_children(
    presentation: &content_reader_military::technology::PresentationName,
    world: Result<&crate::triggers::World<'_>, String>,
    database: Option<&Arc<content_reader_key_names::equipment::Database>>,
) -> Result<Vec<String>, String> {
    presentation.children(
        |body| {
            world
                .as_ref()
                .map_err(Clone::clone)?
                .evaluate(body, crate::triggers::Scopes::empty())
                .decided()
        },
        |child| {
            let database =
                database.ok_or_else(|| "native equipment database is unavailable".to_owned())?;
            let enabled = world
                .as_ref()
                .map(|world| {
                    engine_dlc::dlc_feature_state::NAME_MASKS
                        .iter()
                        .any(|(name, mask)| mask & (1 << 7) != 0 && world.has_dlc(name))
                })
                .map_err(Clone::clone);
            database
                .technology_child(child, enabled)?
                .map(|equipment| {
                    equipment
                        .name_str()
                        .map(str::to_owned)
                        .map_err(|e| e.to_string())
                })
                .transpose()
        },
    )
}

#[derive(Default)]
struct PreparedContent {
    national: CachedResult<Preparation>,
    continuous: CachedResult<Preparation>,
    technology: CachedResult<Preparation>,
}

struct Preparation {
    definitions: Arc<content_reader_focus::focus::Focuses>,
    point_days: Option<f64>,
    technology_names: Arc<TechnologyNames>,
    inventory: Vec<String>,
    preparation_warnings: Vec<content_reader_parser::Diagnostic>,
    warnings: Vec<String>,
    folder_definitions: Arc<Vec<content_reader_military::technology::FolderDefinition>>,
    folder_warnings: Vec<String>,
}

impl Preparation {
    fn read(layers: &content_reader_layers::ResolvedLayers, kind: Kind) -> io::Result<Self> {
        let mut warnings = Vec::new();
        let mut folder_definitions = Vec::new();
        let mut folder_warnings = Vec::new();
        let (definitions, point_days, technology_names, inventory, preparation_warnings) =
            match kind {
                Kind::Focus(database) => {
                    let (definitions, definition_warnings) = content_reader_focus::focus::read_database(layers, focus_database(database))?;
                    warnings.extend(
                        definition_warnings
                            .iter()
                            .map(|(_, message)| message.clone()),
                    );
                    (
                        definitions,
                        content_reader_focus::focus::point_days(layers)?,
                        TechnologyNames::default(),
                        Vec::new(),
                        definition_warnings,
                    )
                }
                Kind::Technology => {
                    let read = content_reader_military::technology::Database::read(layers)?;
                    match content_reader_military::technology::read_folders(layers) {
                        Ok(folders) => {
                            folder_definitions = folders.folders;
                            folder_warnings
                                .extend(folders.warnings.into_iter().map(|(_, message)| message));
                        }
                        Err(error) => {
                            folder_warnings.push(format!("Research folders unavailable: {error}"));
                        }
                    }
                    warnings.extend(
                        read.equipment_warnings
                            .into_iter()
                            .map(|(_, message)| message),
                    );
                    if let Some(database) = layers.key_name_products()?.equipment_database() {
                        warnings.extend(
                            database
                                .misreads
                                .iter()
                                .map(|note| format!("Equipment database: {note}")),
                        );
                    }
                    (
                        content_reader_focus::focus::Focuses::default(),
                        None,
                        TechnologyNames::read(read.definitions, read.unread),
                        read.tokens,
                        read.inventory_warnings,
                    )
                }
            };
        Ok(Self {
            folder_definitions: Arc::new(folder_definitions),
            folder_warnings,
            definitions: Arc::new(definitions),
            point_days,
            technology_names: Arc::new(technology_names),
            inventory,
            preparation_warnings,
            warnings,
        })
    }
}

type IconKey = (bool, content_reader_layers::content::ArtworkMode, String);

pub struct Context {
    diagnostics: catalogue::OwnRefusals,
    kind: Kind,
    technology_names: Arc<TechnologyNames>,
    catalogue: Arc<Catalogue>,
    artwork_cache: BTreeMap<String, catalogue::Drawn<(Artwork, Vec<String>)>>,
    icon_cache: std::sync::Mutex<BTreeMap<IconKey, catalogue::Drawn<Result<Option<String>, String>>>>,
    definitions: Arc<content_reader_focus::focus::Focuses>,
    focus_text_needs_frame: OnceLock<bool>,
    point_days: Option<f64>,
    warnings: Vec<String>,
    inventory: Vec<String>,
    preparation_warnings: Vec<content_reader_parser::Diagnostic>,
    folder_definitions: Arc<Vec<content_reader_military::technology::FolderDefinition>>,
    folder_warnings: Vec<String>,
    folder_availability: OnceLock<catalogue::Drawn<Vec<Option<bool>>>>,
}

pub fn ensure<'a>(
    slot: &'a mut Option<Context>,
    source: &Source,
    content: &ContentOptions,
    kind: Kind,
) -> io::Result<&'a mut Context> {
    ensure_in(slot, catalogue::catalogue(source, content)?, kind)
}

pub fn shared(
    slot: &mut Option<Arc<Context>>,
    source: &Source,
    content: &ContentOptions,
    kind: Kind,
) -> io::Result<Arc<Context>> {
    let catalogue = catalogue::catalogue(source, content)?;
    if !slot
        .as_ref()
        .is_some_and(|cached| cached.kind == kind && Arc::ptr_eq(&cached.catalogue, &catalogue))
    {
        *slot = Some(Arc::new(Context::load(catalogue, kind)?));
    }
    let context = Arc::clone(slot.as_ref().expect("artwork context loaded"));
    catalogue::credit(&context.diagnostics);
    Ok(context)
}

pub fn ensure_in(
    slot: &mut Option<Context>,
    catalogue: Arc<Catalogue>,
    kind: Kind,
) -> io::Result<&mut Context> {
    if !slot
        .as_ref()
        .is_some_and(|cached| cached.kind == kind && Arc::ptr_eq(&cached.catalogue, &catalogue))
    {
        *slot = Some(Context::load(catalogue, kind)?);
    }
    let context = slot.as_mut().expect("artwork context loaded");
    catalogue::credit(&context.diagnostics);
    Ok(context)
}

fn focus_slot(frame: replay_frame::frame::HourFrame<'_>, tag: &str) -> Result<Option<u32>, String> {
    use replay_data::rows::{CountryLinkRow, CountryRow, column, mask, tables};
    const SLOT: u64 = mask::<CountryRow>(&["canonical_slot"]);
    const TAG: usize = column::<CountryRow>("tag");
    const LINK: u64 = mask::<CountryLinkRow>(&["canonical_slot"]);
    let countries = frame
        .complete::<tables::Countries>(SLOT)
        .map_err(|error| error.to_string())?;
    let links = frame
        .complete::<tables::CountryLinks>(LINK)
        .map_err(|error| error.to_string())?;
    let mut named = false;
    for pair in countries.with_text(|row| &row.tag, TAG) {
        let (country, text) = pair.map_err(|error| error.to_string())?;
        if text != tag.as_bytes() {
            continue;
        }
        named = true;
        if let Some(slot) = crate::metadata::link_slot_in(&links, country.id)
            && slot == country.canonical_slot
        {
            return Ok(Some(slot));
        }
    }
    if named {
        Err("focus country identity is unresolved".into())
    } else {
        Ok(None)
    }
}

pub type ResolvedArt = (Artwork, Option<String>);

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(any(test, feature = "test-support"), derive(replay_spool::Spooled))]
pub struct ChosenArt {
    pub art: Artwork,
    pub icon: Result<String, String>,
    pub text: Vec<String>,
}

pub fn refusal(icon: Option<String>, text: &[String]) -> Option<String> {
    let mut refused = icon;
    for reason in text {
        match &mut refused {
            Some(previous) => {
                if !previous.contains(reason.as_str()) {
                    previous.push_str("; ");
                    previous.push_str(reason);
                }
            }
            None => refused = Some(reason.clone()),
        }
    }
    refused
}

struct FocusItem {
    art: Artwork,
    icon: Option<Result<String, String>>,
    text: Vec<String>,
}

impl FocusItem {
    fn resolved(self) -> ResolvedArt {
        let Self { mut art, icon, text } = self;
        let refused = match icon {
            Some(Ok(icon)) => {
                art.icon = Some(icon);
                None
            }
            Some(Err(reason)) => Some(reason),
            None => None,
        };
        (art, refusal(refused, &text))
    }

    fn chosen(self) -> ChosenArt {
        ChosenArt {
            art: self.art,
            icon: self.icon.expect("an event's focus art asks for its icon"),
            text: self.text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FocusProjection {
    Full,
    Text,
    Titles,
    Display,
    Icons,
}

pub struct FocusFrame<'a> {
    pub frame: replay_frame::frame::HourFrame<'a>,
    catalogue: &'a Catalogue,
    text: OnceLock<(
        Result<crate::scripted_text::ScriptedText<'a>, String>,
        catalogue::OwnRefusals,
    )>,
    rereads:
        std::cell::RefCell<std::collections::HashMap<crate::triggers::inputs::Key, Option<u64>>>,
    #[cfg(any(test, feature = "test-support"))]
    pub text_builds: std::cell::Cell<usize>,
}

impl<'a> FocusFrame<'a> {
    pub fn new(
        frame: impl Into<replay_frame::frame::HourFrame<'a>>,
        catalogue: &'a Catalogue,
    ) -> Self {
        Self {
            frame: frame.into(),
            catalogue,
            text: OnceLock::new(),
            rereads: Default::default(),
            #[cfg(any(test, feature = "test-support"))]
            text_builds: std::cell::Cell::new(0),
        }
    }


    pub fn reread(&self, key: &crate::triggers::inputs::Key) -> Option<u64> {
        if let Some(read) = self.rereads.borrow().get(key) {
            return *read;
        }
        let read = self.read_now(key);
        self.rereads.borrow_mut().insert(key.clone(), read);
        read
    }

    fn read_now(&self, key: &crate::triggers::inputs::Key) -> Option<u64> {
        match key {
            crate::triggers::inputs::Key::FocusTag(tag) => Some(crate::triggers::inputs::digest(
                &focus_slot(self.frame, tag),
            )),
            _ => self.text().ok()?.reread(key),
        }
    }

    fn text(&self) -> Result<&crate::scripted_text::ScriptedText<'a>, String> {
        let shown = self.catalogue;
        let frame = self.frame;
        let mut built = false;
        let (text, noted) = self.text.get_or_init(|| {
            built = true;
            #[cfg(any(test, feature = "test-support"))]
            self.text_builds.set(self.text_builds.get() + 1);
            catalogue::capture(|| {
                shown.scripted_content().map(|content| {
                    crate::triggers::inputs::unrecorded(|| {
                        crate::scripted_text::ScriptedText::new_in(
                            frame,
                            &shown.localisation,
                            content,
                            0,
                        )
                    })
                })
            })
        });
        if !built {
            catalogue::credit(noted);
        }
        text.as_ref().map_err(Clone::clone)
    }
}

impl FocusProjection {
    fn icons(self) -> bool {
        matches!(self, Self::Full | Self::Display | Self::Icons)
    }

    fn descriptions(self) -> bool {
        matches!(self, Self::Full | Self::Text)
    }
}

fn focus_icons_need_frame(definition: &content_reader_focus::focus::FocusDefinition) -> bool {
    definition
        .icons
        .cases
        .iter()
        .chain(definition.alternate_icons.cases.iter())
        .any(|case| {
            case.trigger
                .as_ref()
                .is_some_and(|trigger| !trigger.is_empty())
        })
}

fn focus_needs_frame(
    catalogue: &Catalogue,
    definition: &content_reader_focus::focus::FocusDefinition,
    include_descriptions: bool,
) -> bool {
    focus_icons_need_frame(definition)
        || [&definition.localisation, &definition.description]
            .into_iter()
            .take(if include_descriptions { 2 } else { 1 })
            .any(|key| {
                catalogue
                    .localisation
                    .localise(key.as_bytes())
                    .is_ok_and(|text| text.contains('['))
            })
}

impl Context {
    pub fn load(catalogue: Arc<Catalogue>, kind: Kind) -> io::Result<Self> {
        let (loaded, diagnostics) = catalogue::capture(|| Self::load_inner(catalogue, kind));
        let mut loaded = loaded?;
        loaded.diagnostics = diagnostics;
        Ok(loaded)
    }

    fn load_inner(catalogue: Arc<Catalogue>, kind: Kind) -> io::Result<Self> {
        let mut warnings = vec![CURRENT_INSTALL_ART_NOTICE.to_owned()];
        warnings.extend(catalogue.verification.iter().cloned());
        let layers = &catalogue.content.layers;
        let prepared = layers.derived::<PreparedContent>();
        let slot = match kind {
            Kind::Focus(FocusDatabase::National) => &prepared.national,
            Kind::Focus(FocusDatabase::Continuous) => &prepared.continuous,
            Kind::Technology => &prepared.technology,
        };
        let prepared = slot
            .get_or_init(|| Preparation::read(layers, kind))
            .map_err(|error| {
                if error.kind() == io::ErrorKind::Interrupted {
                    error
                } else {
                    invalid(error.to_string())
                }
            })?;
        let definitions = Arc::clone(&prepared.definitions);
        let technology_names = Arc::clone(&prepared.technology_names);
        let point_days = prepared.point_days;
        let inventory = match kind {
            Kind::Focus(database) => definitions
                .ids_in(focus_database(database))
                .map(str::to_owned)
                .collect(),
            Kind::Technology => prepared.inventory.clone(),
        };
        let preparation_warnings = prepared.preparation_warnings.clone();
        let folder_definitions = Arc::clone(&prepared.folder_definitions);
        let folder_warnings = prepared.folder_warnings.clone();
        warnings.extend(prepared.warnings.iter().cloned());
        warnings.extend(catalogue.warnings.iter().cloned());
        Ok(Self {
            diagnostics: Default::default(),
            kind,
            technology_names,
            catalogue,
            artwork_cache: BTreeMap::new(),
            icon_cache: Default::default(),
            definitions,
            focus_text_needs_frame: OnceLock::new(),
            point_days,
            warnings,
            inventory,
            preparation_warnings,
            folder_definitions,
            folder_warnings,
            folder_availability: OnceLock::new(),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn content_loads(catalogue: &Catalogue) -> usize {
        let prepared = catalogue.content.layers.derived::<PreparedContent>();
        prepared.national.loads() + prepared.continuous.loads() + prepared.technology.loads() + catalogue.content_loads()
    }

    pub fn research_folders(
        &self,
        tokens: &BTreeMap<&str, Option<u32>>,
        definitions: &BTreeSet<(&str, u32)>,
    ) -> (
        Vec<crate::ResearchFolder>,
        Vec<crate::ResearchTechnologyFolder>,
        Vec<String>,
    ) {
        let availability = self
            .folder_availability
            .get_or_init(|| {
                catalogue::Drawn::new(|| {
                    let world = self
                        .catalogue
                        .scripted_content()
                        .and_then(|content| content.unrecorded_trigger_world());
                    self.folder_definitions
                        .iter()
                        .map(|folder| match &folder.available {
                            None => Some(true),
                            Some(trigger) => world.as_ref().ok().and_then(|world| {
                                world
                                    .evaluate(trigger, crate::triggers::Scopes::empty())
                                    .decided()
                                    .ok()
                            }),
                        })
                        .collect()
                })
            })
            .get();
        let available: Vec<(&str, Option<bool>)> = self
            .folder_definitions
            .iter()
            .zip(availability)
            .map(|(folder, available)| (folder.key.as_str(), *available))
            .collect();
        let uses: Vec<(&str, Option<u32>, Option<u32>)> = tokens
            .iter()
            .map(|(&token, &at)| (token, None, at))
            .chain(
                definitions
                    .iter()
                    .map(|&(token, index)| (token, Some(index), Some(index))),
            )
            .collect();
        let mut placed = BTreeMap::new();
        let mut unplaced = Vec::new();
        for &(token, shown, at) in &uses {
            match self.technology(token, at) {
                Ok(technology) => {
                    placed.insert((token.to_owned(), shown), technology.folders.clone());
                }
                Err(_) if !self.technology_names.may_define(token) => {}
                Err(reason) => unplaced.push(reason),
            }
        }
        let (shown, notes) = shown_folders(
            &available,
            &placed,
            uses.iter()
                .map(|&(token, shown, _)| (token, shown))
                .filter(|&(token, shown)| {
                    placed.contains_key(&(token.to_owned(), shown))
                        || !self.technology_names.may_define(token)
                }),
        );
        let folders = self
            .folder_definitions
            .iter()
            .map(|folder| crate::ResearchFolder {
                key: folder.key.clone(),
                name: self.localise(&folder.key),
            })
            .collect();
        let mut warnings = self.folder_warnings.clone();
        warnings.extend(notes);
        warnings.extend(
            unplaced
                .into_iter()
                .map(|reason| format!("{reason}, so its research folder is unknown.")),
        );
        (folders, shown, warnings)
    }

    pub fn localise(&self, key: &str) -> Option<String> {
        if !self.catalogue.has_presentation_key(key) {
            return None;
        }
        if matches!(self.kind, Kind::Technology) {
            return self.catalogue.localised(key, self.view());
        }
        let (text, findings) = catalogue::localised_details(
            &self.catalogue.localisation,
            key,
            &[],
            self.view(),
            dashboard_core::findings::Kind::Offline,
        );
        for finding in findings {
            self.catalogue.note(finding);
        }
        text
    }

    fn static_focus_text(&self, key: &str) -> Option<String> {
        if !self.catalogue.has_presentation_key(key) {
            return None;
        }
        let raw = self.catalogue.localisation.localise(key.as_bytes()).ok()?;
        if raw.contains('[') {
            return None;
        }
        catalogue::plain_text(&raw).ok()
    }

    fn view(&self) -> catalogue::View {
        match self.kind {
            Kind::Focus(_) => catalogue::View::Scripted,
            Kind::Technology => catalogue::View::Plain,
        }
    }

    fn focus(
        &self,
        token: &str,
    ) -> Result<Option<&content_reader_focus::focus::FocusDefinition>, &content_reader_parser::coverage::Gap>
    {
        match self.kind {
            Kind::Focus(database) => self.definitions.get_in(focus_database(database), token),
            Kind::Technology => Ok(None),
        }
    }

    pub fn refusal_lines(&self) -> Vec<String> {
        self.catalogue.refusal_lines()
    }

    pub fn standing_warnings(&self) -> Vec<String> {
        let mut warnings = self.warnings.clone();
        warnings.sort();
        warnings.dedup();
        warnings
    }

    fn description_key(&self, token: &str, tag: Option<&str>) -> Result<String, String> {
        self.description_key_at(token, tag, None)
    }

    fn description_key_at(
        &self,
        token: &str,
        tag: Option<&str>,
        at: Option<u32>,
    ) -> Result<String, String> {
        if matches!(self.kind, Kind::Focus(_)) {
            return Ok(self
                .focus(token)
                .map_err(|gap| format!("focus {token}: {gap}"))?
                .map(|d| d.description.clone())
                .unwrap_or_else(|| format!("{token}_desc")));
        }
        let technology = self.technology(token, at)?;
        let children = self.technology_children_of(technology)?;
        let presentation = technology
            .presentation
            .as_ref()
            .map_err(Clone::clone)?;
        let key_for = |root: &str| {
            let key = format!("{root}_desc");
            let prefixed = tag.map(|tag| format!("{tag}_{key}"));
            prefixed
                .filter(|key| self.catalogue.localisation.get(key).is_some())
                .unwrap_or(key)
        };
        if !children.is_empty() {
            let registry = self
                .catalogue
                .content
                .layers
                .key_name_products()
                .map_err(|error| error.to_string())?;
            let database = registry
                .equipment_database()
                .ok_or_else(|| "native equipment database is unavailable".to_owned())?;
            let mut ordinary = 0;
            for name in children {
                let child = database
                    .named(name)
                    .ok_or_else(|| format!("native admitted equipment {name} is unavailable"))?;
                ordinary += usize::from(!child.duplicate);
            }
            if ordinary == 1 {
                let first = database
                    .named(&children[0])
                    .expect("admitted child resolved above");
                let key = key_for(first.name_str().map_err(|error| error.to_string())?);
                if self.catalogue.localisation.get(&key).is_some() {
                    return Ok(key);
                }
                let archetype = first.archetype.and_then(|index| database.equipment.get(index))
                    .ok_or_else(|| format!("technology {token}: first equipment description archetype is unavailable"))?;
                return Ok(key_for(
                    archetype.name_str().map_err(|error| error.to_string())?,
                ));
            }
        }
        Ok(key_for(&presentation.template))
    }

    fn description(&self, token: &str) -> Option<String> {
        self.description_at(token, None)
    }

    fn description_at(&self, token: &str, at: Option<u32>) -> Option<String> {
        self.localise(&self.description_key_at(token, None, at).ok()?)
    }

    pub fn description_is_defined(&self, token: &str) -> bool {
        self.description_key(token, None)
            .is_ok_and(|key| self.catalogue.localisation.get(&key).is_some())
    }

    fn base_duration_days(&self, token: &str) -> Option<f64> {
        let days = self.focus(token).ok()??.cost? * self.point_days?;
        days.is_finite().then_some(days)
    }

    fn technology(&self, token: &str, at: Option<u32>) -> Result<&TechnologyName, String> {
        let names = &self.technology_names;
        let definitions = names.get(token).map_or(&[][..], Vec::as_slice);
        if let Some(at) = at {
            if let Some(definition) = definitions
                .iter()
                .find(|definition| definition.database_index == Some(at))
            {
                return Ok(definition);
            }
            return Err(match &names.unplaced {
                Some(why) => format!(
                    "technology {token}: recorded template {at} is no definition of known place, and {why}, so which definition it is is unknown"
                ),
                None if definitions.is_empty() => {
                    format!("technology {token}: definition unavailable in recorded content")
                }
                None => format!(
                    "technology {token}: recorded template {at} is none of its {} definitions",
                    definitions.len()
                ),
            });
        }
        if let Some(why) = names.unread(token) {
            return Err(format!(
                "technology {token} may be defined in a file not read whole ({why}), and which definition it is was not recorded"
            ));
        }
        let Some(first) = definitions.first() else {
            return Err(format!(
                "technology {token}: definition unavailable in recorded content"
            ));
        };
        if definitions[1..].iter().all(|other| {
            other.presentation == first.presentation && other.folders == first.folders
        }) {
            return Ok(first);
        }
        Err(format!(
            "technology {token} is defined {} times with different names or folders, and which definition it is was not recorded",
            definitions.len()
        ))
    }

    fn technology_children_of<'a>(
        &'a self,
        presentation: &'a TechnologyName,
    ) -> Result<&'a [String], String> {
        presentation
            .children
            .get_or_init(|| {
                catalogue::Drawn::new(|| {
                    let presentation =
                        presentation.presentation.as_ref().map_err(Clone::clone)?;
                    let registry = self
                        .catalogue
                        .content
                        .layers
                        .key_name_products()
                        .map_err(|error| error.to_string())?;
                    let world = self
                        .catalogue
                        .scripted_content()
                        .and_then(|content| content.unrecorded_trigger_world());
                    technology_children(
                        presentation,
                        world.as_ref().map_err(Clone::clone),
                        registry.equipment_database(),
                    )
                })
            })
            .get()
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }

    #[cfg(test)]
    fn technology_root(&self, token: &str) -> Result<String, String> {
        self.technology_root_at(token, None)
    }

    fn technology_root_at(&self, token: &str, at: Option<u32>) -> Result<String, String> {
        let technology = self.technology(token, at)?;
        let children = self.technology_children_of(technology)?;
        Ok(if children.len() == 1 {
            children[0].clone()
        } else {
            technology
                .presentation
                .as_ref()
                .map_err(Clone::clone)?
                .template
                .clone()
        })
    }

    fn name(&self, token: &str) -> Option<String> {
        self.name_at(token, None)
    }

    fn name_at(&self, token: &str, at: Option<u32>) -> Option<String> {
        if self.kind == Kind::Technology {
            let key = self.technology_root_at(token, at).ok()?;
            return self.localise(&key);
        }
        let definition = self.focus(token).ok()?;
        let key = definition.map(|d| d.localisation.as_str()).unwrap_or(token);
        self.localise(key)
    }

    #[expect(clippy::result_large_err, reason = "the error is the dashboard's shared finding, which the caller records as it is")]
    fn selected_icon(
        &self,
        token: &str,
    ) -> Result<Option<String>, dashboard_core::findings::Finding> {
        use dashboard_core::findings::{Finding, Kind as FindingKind};
        match self.kind {
            Kind::Technology => Ok(Some(format!("GFX_{token}_medium"))),
            Kind::Focus(_) => match self.focus(token) {
                Err(gap) => Err(Finding::grouped(
                    FindingKind::Refused,
                    token,
                    "focus definition is unknown",
                    gap.to_string(),
                )),
                Ok(Some(definition)) => definition
                    .icon(None, |trigger| {
                        if trigger.is_empty() {
                            Ok(true)
                        } else {
                            Err("focus icon requires recorded country/hour trigger operands".into())
                        }
                    })
                    .map(|name| Some(name.unwrap_or(UNKNOWN_FOCUS_ICON).to_owned()))
                    .map_err(|reason| {
                        Finding::grouped(
                            FindingKind::Offline,
                            token,
                            "focus icon requires recorded context",
                            reason,
                        )
                    }),
                Ok(None) => Ok(None),
            },
        }
    }

    fn declared_icon(&self, token: &str) -> Option<&content_reader_gfx::gfx::SpriteType> {
        let name = self.selected_icon(token).ok()??;
        self.catalogue
            .sprites
            .find(&name)
            .ok()
            .flatten()
            .filter(|sprite| sprite.texture.is_some())
    }

    fn artwork(
        &self,
        token: &str,
    ) -> io::Result<(Artwork, Vec<dashboard_core::findings::Finding>)> {
        use dashboard_core::findings::{Finding, Kind as FindingKind};
        let name = self.name(token);
        let placeholder = |message: String| {
            Finding::grouped(
                FindingKind::Fallback,
                token,
                "icon is unavailable; a placeholder is shown",
                message,
            )
        };
        let mut findings = Vec::new();
        let mut drawn_sprite = None;
        let icon = match self.selected_icon(token) {
            Err(finding) => {
                findings.push(finding);
                None
            }
            Ok(None) => {
                findings.push(placeholder(
                    "focus definition is absent from the recorded content; a placeholder is shown"
                        .into(),
                ));
                None
            }
            Ok(Some(sprite)) => {
                let undeclared = undeclared_icon(&sprite);
                let drawn = catalogue::draw(
                    || format!("{} {token} icon", self.kind.noun()),
                    &self.catalogue,
                    |catalogue| {
                        declared_icon_png(catalogue, &sprite)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| undeclared.clone())
                    },
                );
                match drawn.get() {
                    Ok(icon) => {
                        drawn_sprite = Some(sprite);
                        Some(icon.clone())
                    }
                    Err(why) if *why == undeclared => {
                        findings.push(placeholder(undeclared));
                        None
                    }
                    Err(why) => return Err(invalid(why.clone())),
                }
            }
        };
        Ok((
            Artwork {
                token: token.into(),
                name,
                description: self.description(token),
                base_duration_days: self.base_duration_days(token),
                icon,
                sprite: drawn_sprite,
            },
            findings,
        ))
    }

    pub fn tokens(&self) -> (&[String], &[content_reader_parser::Diagnostic]) {
        (&self.inventory, &self.preparation_warnings)
    }

    pub fn prepare(&self, token: &str) -> (Artwork, Vec<dashboard_core::findings::Finding>) {
        let (art, mut findings) = self.prepare_icon(token);
        findings.extend(self.text_findings(token, None));
        (art, findings)
    }

    fn prepare_icon(&self, token: &str) -> (Artwork, Vec<dashboard_core::findings::Finding>) {
        use dashboard_core::findings::{Finding, Kind};
        let mut findings = Vec::new();
        let art = match self.artwork(token) {
            Ok((art, problems)) => {
                findings.extend(problems);
                art
            }
            Err(error) => {
                findings.push(Finding::grouped(
                    Kind::Refused,
                    token,
                    "icon is unavailable; a placeholder is shown",
                    format!("icon unavailable: {error}"),
                ));
                Artwork {
                    token: token.into(),
                    name: self.name(token),
                    description: self.description(token),
                    base_duration_days: self.base_duration_days(token),
                    icon: None,
                    sprite: None,
                }
            }
        };
        (art, findings)
    }

    fn text_findings(&self, token: &str, at: Option<u32>) -> Vec<dashboard_core::findings::Finding> {
        use dashboard_core::findings::{Finding, Kind};
        let mut findings = Vec::new();
        if let Some(Err(reason)) = self
            .technology_names
            .get(token)
            .is_some()
            .then(|| self.technology(token, at).ok())
            .flatten()
            .and_then(|name| name.children.get().map(catalogue::Drawn::get))
        {
            findings.push(Finding::grouped(
                Kind::Refused,
                token,
                "technology name is unavailable",
                reason.clone(),
            ));
        }
        if let Err(gap) = self.focus(token) {
            findings.push(Finding::grouped(
                Kind::Refused,
                token,
                "focus name is unavailable",
                gap.to_string(),
            ));
        }
        if let Err(reason) = self.description_key_at(token, None, at) {
            findings.push(Finding::grouped(
                Kind::Refused,
                token,
                "description is unavailable",
                reason,
            ));
        }
        findings
    }

    fn recorded_text(
        &self,
        token: &str,
        recorded: &[Option<u32>],
    ) -> (Option<String>, Option<String>, Vec<dashboard_core::findings::Finding>) {
        match recorded {
            [] => (
                self.name(token),
                self.description(token),
                self.text_findings(token, None),
            ),
            [at] => (
                self.name_at(token, *at),
                self.description_at(token, *at),
                self.text_findings(token, *at),
            ),
            several => (
                self.name(token),
                self.description(token),
                several
                    .iter()
                    .flat_map(|at| self.text_findings(token, *at))
                    .collect(),
            ),
        }
    }

    pub fn requested_focus_needs_frame(&self, tokens: &[String]) -> bool {
        tokens
            .iter()
            .filter_map(|token| self.focus(token).ok().flatten())
            .any(|definition| focus_needs_frame(&self.catalogue, definition, true))
    }

    pub fn focus_country_icon_override(&self, token: &str, tag: &str) -> bool {
        self.focus(token).ok().flatten().is_some_and(|definition| {
            definition.button_sprite_overrides(token, Some(tag)).first().is_some_and(|country| {
                self.drawing_catalogues()
                    .any(|catalogue| !matches!(catalogue.sprites.find(country), Ok(None)))
            })
        })
    }

    fn drawing_catalogues(&self) -> impl Iterator<Item = &Catalogue> {
        std::iter::once(&*self.catalogue).chain(self.catalogue.recorded().map(|recorded| &**recorded))
    }

    pub fn requested_focus_display_needs_frame(&self, tokens: &[String]) -> bool {
        tokens
            .iter()
            .filter_map(|token| self.focus(token).ok().flatten())
            .any(|definition| focus_needs_frame(&self.catalogue, definition, false))
    }

    pub fn requested_focus_icons_need_frame(&self, tokens: &[String]) -> bool {
        tokens
            .iter()
            .filter_map(|token| self.focus(token).ok().flatten())
            .any(focus_icons_need_frame)
    }

    pub fn requested_focus_title_needs_frame(&self, tokens: &[String]) -> bool {
        tokens
            .iter()
            .filter_map(|token| self.focus(token).ok().flatten())
            .any(|definition| {
                self.catalogue
                    .localisation
                    .localise(definition.localisation.as_bytes())
                    .is_ok_and(|text| text.contains('['))
            })
    }

    pub fn requested_focus_text_needs_frame(&self, tokens: &[String]) -> bool {
        tokens
            .iter()
            .filter_map(|token| self.focus(token).ok().flatten())
            .any(|definition| {
                [&definition.localisation, &definition.description]
                    .into_iter()
                    .any(|key| {
                        self.catalogue
                            .localisation
                            .localise(key.as_bytes())
                            .is_ok_and(|text| text.contains('['))
                    })
            })
    }

    pub fn requested_tree_focus_needs_frame(
        &self,
        token: &str,
        tag: &str,
        projection: FocusProjection,
    ) -> bool {
        let tokens = [token.to_owned()];
        let icons = projection.icons()
            && self.focus(token).ok().flatten().is_some_and(|definition| {
                let overrides = definition.button_sprite_overrides(token, Some(tag));
                focus_icons_need_frame(definition)
                    && self.drawing_catalogues().any(|catalogue| {
                        !overrides
                            .iter()
                            .any(|name| !matches!(catalogue.sprites.find(name), Ok(None)))
                    })
            });
        icons
            || match projection {
                FocusProjection::Icons => false,
                FocusProjection::Full | FocusProjection::Text => {
                    self.requested_focus_text_needs_frame(&tokens)
                }
                FocusProjection::Display | FocusProjection::Titles => {
                    self.requested_focus_title_needs_frame(&tokens)
                }
            }
    }

    pub fn focus_text_needs_frame(&self) -> bool {
        *self.focus_text_needs_frame.get_or_init(|| {
            let Kind::Focus(database) = self.kind else {
                return false;
            };
            self.definitions
                .known_in(focus_database(database))
                .any(|definition| focus_needs_frame(&self.catalogue, definition, true))
        })
    }

    pub fn focuses_at(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
    ) -> Vec<ResolvedArt> {
        self.focuses(requests, frame, false, FocusProjection::Full, |items| {
            items.collect()
        })
    }

    pub fn event_focus_choices_at<'f, T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: replay_frame::frame::HourFrame<'f>,
        collect: impl FnOnce(&mut dyn Iterator<Item = ChosenArt>) -> T,
    ) -> T {
        let catalogue = Arc::clone(&self.catalogue);
        let inputs = FocusFrame::new(frame, &catalogue);
        self.focus_items(requests, &inputs, false, FocusProjection::Full, false, |items| {
            let mut chosen = items.map(FocusItem::chosen);
            collect(&mut chosen)
        })
    }

    pub fn event_focus_icons(
        &self,
        chosen: &[(&str, &Result<String, String>)],
    ) -> Vec<Result<String, String>> {
        if chosen.len() <= 8 {
            self.prefetch_requested(chosen.iter().map(|&(token, _)| (token, None)));
        }
        chosen
            .iter()
            .map(|&(token, choice)| -> Result<String, String> {
                let name = choice.as_ref().map_err(Clone::clone)?;
                self.draw_focus_icon(token, |_| Ok(name.clone()))
            })
            .collect()
    }

    fn draw_focus_icon(
        &self,
        token: &str,
        choose: impl Fn(&Catalogue) -> Result<String, String>,
    ) -> Result<String, String> {
        catalogue::draw(
            || format!("focus {token} icon"),
            &self.catalogue,
            |drawing| {
                let name = choose(drawing)?;
                match self.cached_icon(drawing, &name) {
                    Ok(Some(icon)) => Ok(icon),
                    Ok(None) => Err(format!(
                        "focus {token}: selected sprite {name} is undeclared or has no texture"
                    )),
                    Err(reason) => Err(format!("focus {token}: {reason}")),
                }
            },
        )
        .get()
        .clone()
    }

    pub fn focus_tree_at<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focuses(requests, frame, true, FocusProjection::Full, collect)
    }

    pub fn focus_text_at<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focuses(requests, frame, true, FocusProjection::Text, collect)
    }

    pub fn focus_titles_at<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focuses(requests, frame, true, FocusProjection::Titles, collect)
    }

    pub fn focus_display_at<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focuses(requests, frame, true, FocusProjection::Display, collect)
    }

    pub fn focus_icons_at<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focuses(requests, frame, true, FocusProjection::Icons, collect)
    }

    fn focuses<T>(
        &mut self,
        requests: &[(&str, &str)],
        frame: &replay_frame::frame::DecodedFrame,
        tree: bool,
        projection: FocusProjection,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        let catalogue = Arc::clone(&self.catalogue);
        let inputs = FocusFrame::new(frame, &catalogue);
        self.focuses_in(requests, &inputs, tree, projection, collect)
    }

    pub fn focuses_in<T>(
        &mut self,
        requests: &[(&str, &str)],
        inputs: &FocusFrame<'_>,
        tree: bool,
        projection: FocusProjection,
        collect: impl FnOnce(&mut dyn Iterator<Item = ResolvedArt>) -> T,
    ) -> T {
        self.focus_items(requests, inputs, tree, projection, true, |items| {
            let mut resolved = items.map(FocusItem::resolved);
            collect(&mut resolved)
        })
    }

    fn prefetch_requested<'r>(&self, requests: impl Iterator<Item = (&'r str, Option<&'r str>)>) {
        let sprites = requests
            .filter_map(|(token, tree)| {
                let definition = self.focus(token).ok()??;
                let selected = if let Some(tag) = tree {
                    let mut selected = None;
                    for name in definition.button_sprite_overrides(token, Some(tag)) {
                        match self.catalogue.sprites.find(&name) {
                            Ok(Some(_)) => {
                                selected = Some(name);
                                break;
                            }
                            Ok(None) => {}
                            Err(_) => return None,
                        }
                    }
                    selected
                } else {
                    None
                };
                let selected = selected.or_else(|| {
                    definition
                        .icon(None, |trigger| {
                            if trigger.is_empty() {
                                Ok(true)
                            } else {
                                Err("recorded context required".into())
                            }
                        })
                        .ok()
                        .map(|name| name.unwrap_or(UNKNOWN_FOCUS_ICON).to_owned())
                })?;
                self.catalogue
                    .sprites
                    .find(&selected)
                    .ok()
                    .flatten()
                    .filter(|sprite| sprite.texture.is_some())
            })
            .collect();
        self.prefetch_icons(sprites);
    }

    fn focus_items<T>(
        &mut self,
        requests: &[(&str, &str)],
        inputs: &FocusFrame<'_>,
        tree: bool,
        projection: FocusProjection,
        draw: bool,
        collect: impl FnOnce(&mut dyn Iterator<Item = FocusItem>) -> T,
    ) -> T {
        let frame = inputs.frame;
        if draw && projection.icons() && requests.len() <= 8 {
            self.prefetch_requested(
                requests
                    .iter()
                    .map(|&(token, tag)| (token, tree.then_some(tag))),
            );
        }
        let catalogue = Arc::clone(&self.catalogue);
        let mut country_scopes = BTreeMap::new();
        let mut items = requests.iter().map(|&(token, tag)| {
            let country = country_scopes
                .entry(tag)
                .or_insert_with(|| {
                    crate::triggers::inputs::query(
                        || crate::triggers::inputs::Key::FocusTag(tag.to_owned()),
                        || focus_slot(frame, tag),
                    )
                })
                .clone();
            let context = || inputs.text();
            let icon = projection.icons().then(|| match self.focus(token) {
                Err(gap) => Err(format!("focus {token}: {gap}")),
                Ok(None) => Err(format!("focus {token}: definition absent from recorded content")),
                Ok(Some(definition)) => {
                    let overrides = if tree {
                        definition.button_sprite_overrides(token, Some(tag))
                    } else {
                        Vec::new()
                    };
                    let evaluated = std::cell::OnceCell::new();
                    let definition_icon = || {
                        evaluated
                            .get_or_init(|| {
                                definition
                                    .icon(None, |trigger| {
                                        if trigger.is_empty() {
                                            return Ok(true);
                                        }
                                        let slot = country.clone()?.ok_or_else(|| {
                                            format!("focus {token}: country scope was not recorded")
                                        })?;
                                        let graph = std::rc::Rc::new(std::cell::RefCell::new(
                                            crate::scripted_text::EventScopes::new(vec![
                                                crate::scripted_text::EventScope::of(
                                                    crate::scripted_text::EventObject::Country(slot),
                                                ),
                                            ]),
                                        ));
                                        let evaluator = crate::triggers::Evaluator::new(
                                            context()?.trigger_world()?,
                                            None,
                                        )
                                        .with_event_scopes(graph);
                                        evaluator
                                            .evaluate(
                                                trigger,
                                                evaluator
                                                    .event_scopes(0)
                                                    .map_err(|why| why.to_string())?,
                                            )
                                            .decided()
                                    })
                                    .map(|name| name.unwrap_or(UNKNOWN_FOCUS_ICON).to_owned())
                            })
                            .clone()
                    };
                    if !draw {
                        return definition_icon();
                    }
                    self.draw_focus_icon(token, |drawing| {
                        for name in &overrides {
                            if drawing.sprites.find(name)?.is_some() {
                                return Ok(name.clone());
                            }
                        }
                        definition_icon()
                    })
                }
            });
            let mut text = Vec::new();
            let mut art = Artwork {
                token: token.into(),
                name: None,
                description: None,
                base_duration_days: self.base_duration_days(token),
                icon: None,
                sprite: None,
            };
            if matches!(projection, FocusProjection::Icons) {
                return FocusItem { art, icon, text };
            }
            let localise = |key: &str| -> Result<String, String> {
                if !catalogue.has_presentation_key(key) {
                    return Err(format!("localisation key {key} is absent from the recorded language"));
                }
                let raw = catalogue
                    .localisation
                    .localise(key.as_bytes())
                    .map_err(|error| error.to_string())?;
                if !raw.contains('[') {
                    return catalogue::plain_text(&raw);
                }
                let slot = country.clone()?.ok_or_else(|| {
                    format!("focus {token}: Countries has no row for the requested country")
                })?;
                let context = context()?;
                let mut scope = crate::scripted_text::EventScope::of(
                    crate::scripted_text::EventObject::Country(slot),
                );
                let seed = crate::triggers::inputs::unrecorded(|| crate::scripted_text::clock_seed_in(frame));
                scope.clock_seed = Some(seed);
                let scopes = match seed {
                    Some(seed) => {
                        let hash = token.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
                            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
                        });
                        let seed = seed.wrapping_add(hash);
                        scope.random_seed = crate::scripted_text::random::scope_seed(seed);
                        scope.random_counter = crate::scripted_text::random::scope_counter(seed);
                        crate::scripted_text::EventScopes::new(vec![scope])
                    }
                    None => crate::scripted_text::EventScopes::new(vec![scope]),
                };
                let expanded = if seed.is_some() {
                    context.expand_in(&raw, &scopes)
                } else {
                    context.expand_in_without_random_seed(
                        &raw,
                        &scopes,
                        "focus random_list: Clock seed is not recorded",
                    )
                }?;
                catalogue::plain_text(&expanded)
            };
            let definition = self.focus(token);
            if let Ok(Some(definition)) = definition {
                let description = projection.descriptions()
                    .then_some((&definition.description, &mut art.description));
                for (key, target) in std::iter::once((&definition.localisation, &mut art.name))
                    .chain(description)
                {
                    match localise(key) {
                        Ok(value) => *target = Some(value),
                        Err(reason) => {
                            *target = None;
                            text.push(format!("focus localisation {key}: {reason}"));
                        }
                    }
                }
            } else if !matches!(icon, Some(Err(_))) {
                text.push(match definition {
                    Err(gap) => format!("focus {token}: {gap}"),
                    _ => format!("focus {token}: definition absent from recorded content"),
                });
            }
            FocusItem { art, icon, text }
        });
        collect(&mut items)
    }

    fn prefetch_icons(&self, sprites: Vec<&content_reader_gfx::gfx::SpriteType>) {
        let Ok(decode) = self.catalogue.gui_decode.as_ref() else {
            return;
        };
        let layers = &self.catalogue.content.layers;
        let sprites: Vec<_> = sprites.into_iter().map(|sprite| (sprite, 0)).collect();
        let batch = || catalogue::sprite_frames_png(layers, &sprites, *decode, ICON_EDGE);
        let _ = if self.catalogue.recorded().is_some() {
            content_reader_layers::content::strict(batch)
        } else {
            batch()
        };
    }

    fn cached_icon(&self, drawing: &Catalogue, name: &str) -> io::Result<Option<String>> {
        let key = (
            !std::ptr::eq(drawing, &raw const *self.catalogue),
            content_reader_layers::content::artwork_mode(),
            name.to_owned(),
        );
        let held = |cache: &BTreeMap<IconKey, catalogue::Drawn<Result<Option<String>, String>>>| {
            cache.get(&key).map(|result| result.get().clone())
        };
        if let Some(result) = held(&self.icon_cache.lock().unwrap_or_else(|e| e.into_inner())) {
            return result.map_err(invalid);
        }
        let drawn = catalogue::Drawn::new(|| {
            declared_icon_png(drawing, name).map_err(|error| error.to_string())
        });
        let mut cache = self.icon_cache.lock().unwrap_or_else(|e| e.into_inner());
        let result = cache.entry(key).or_insert(drawn).get().clone();
        result.map_err(invalid)
    }

    pub fn focus_metadata(
        &self,
        tokens: BTreeSet<&str>,
        cancelled: &AtomicBool,
    ) -> io::Result<(Vec<Artwork>, Vec<String>)> {
        let mut items = Vec::with_capacity(tokens.len());
        for token in tokens {
            crate::metadata::check_cancel(cancelled)?;
            items.push(Artwork {
                token: token.into(),
                name: None,
                description: None,
                base_duration_days: self.base_duration_days(token),
                icon: None,
                sprite: self.selected_icon(token).ok().flatten(),
            });
        }
        Ok((items, self.standing_warnings()))
    }

    pub fn text_catalogue(
        &self,
        tokens: BTreeSet<&str>,
        cancelled: &AtomicBool,
    ) -> io::Result<(Vec<Artwork>, Vec<String>)> {
        self.recorded_text_catalogue(tokens.into_iter().map(|token| (token, None)).collect(), cancelled)
    }

    pub fn definition_text(
        &self,
        token: &str,
        database_index: u32,
    ) -> (Option<String>, Option<String>) {
        (
            self.name_at(token, Some(database_index)),
            self.description_at(token, Some(database_index)),
        )
    }

    pub fn recorded_text_catalogue(
        &self,
        tokens: BTreeMap<&str, Option<u32>>,
        cancelled: &AtomicBool,
    ) -> io::Result<(Vec<Artwork>, Vec<String>)> {
        let mut items = Vec::with_capacity(tokens.len());
        for (token, at) in tokens {
            crate::metadata::check_cancel(cancelled)?;
            let (name, description) = if matches!(self.kind, Kind::Focus(_)) {
                match self.focus(token) {
                    Err(_) => (None, None),
                    Ok(definition) => {
                        let name_key =
                            definition.map_or(token, |definition| &definition.localisation);
                        let description_key = self.description_key(token, None).ok();
                        (
                            self.static_focus_text(name_key),
                            description_key
                                .as_deref()
                                .and_then(|key| self.static_focus_text(key)),
                        )
                    }
                }
            } else {
                (self.name_at(token, at), self.description_at(token, at))
            };
            items.push(Artwork {
                token: token.into(),
                name,
                description,
                base_duration_days: self.base_duration_days(token),
                icon: None,
                sprite: None,
            });
        }
        Ok((items, self.standing_warnings()))
    }

    pub fn catalogue(
        &mut self,
        tokens: BTreeSet<&str>,
        cancelled: &AtomicBool,
    ) -> io::Result<(Vec<Artwork>, Vec<String>)> {
        self.recorded_catalogue(
            tokens.into_iter().map(|token| (token, &[][..])).collect(),
            cancelled,
        )
    }

    pub fn recorded_catalogue(
        &mut self,
        tokens: BTreeMap<&str, &[Option<u32>]>,
        cancelled: &AtomicBool,
    ) -> io::Result<(Vec<Artwork>, Vec<String>)> {
        if tokens.len() <= 8 {
            let sprites = tokens
                .keys()
                .filter(|token| !self.artwork_cache.contains_key(**token))
                .filter_map(|token| self.declared_icon(token))
                .collect();
            self.prefetch_icons(sprites);
        }
        let mut warnings = self.warnings.clone();
        let mut artwork = Vec::new();
        for (token, recorded) in tokens {
            if cancelled.load(Ordering::Acquire) {
                return Err(invalid("Widget request cancelled"));
            }
            if !self.artwork_cache.contains_key(token) {
                let drawn = catalogue::Drawn::new(|| {
                    let (art, findings) = self.prepare_icon(token);
                    let messages: Vec<String> = findings
                        .into_iter()
                        .map(|finding| format!("{token}: {}", finding.message))
                        .collect();
                    (art, messages)
                });
                self.artwork_cache.insert(token.into(), drawn);
            }
            let (art, messages) = self.artwork_cache[token].get();
            let (name, description, findings) = self.recorded_text(token, recorded);
            let art = Artwork {
                name,
                description,
                ..art.clone()
            };
            warnings.extend(messages.iter().cloned());
            warnings.extend(
                findings
                    .into_iter()
                    .map(|finding| format!("{token}: {}", finding.message)),
            );
            artwork.push(art);
        }
        Ok((artwork, warnings))
    }
}

pub fn nuke_icon(catalogue: &Catalogue) -> io::Result<Option<String>> {
    catalogue::draw(
        || "nuclear stockpile icon".to_owned(),
        catalogue,
        |catalogue| draw_nuke_icon(catalogue).map_err(dashboard_core::QueryError::from),
    )
    .get()
    .clone()
    .map(Some)
    .map_err(io::Error::from)
}

fn draw_nuke_icon(catalogue: &Catalogue) -> io::Result<String> {
    use content_reader_gui::gui::native::{Fields, Kind};
    let (_, root) = content_reader_gui::gui::native::kept_container_window(
        catalogue.guis()?,
        "topbar",
        &mut Vec::new(),
    )?;
    let container = root
        .find_recursive(Kind::Container, "nukes")
        .ok_or_else(|| dashboard_core::refused("nukes artwork: topbar/nukes container unavailable"))?;
    if !root.refusals().is_empty() {
        return Err(dashboard_core::refused(format!(
            "nukes artwork: topbar has {} unsupported GUI fields: {:?}",
            root.refusals().len(),
            root.refusals()
        )));
    }
    let mut stack = vec![container];
    let mut icons = Vec::new();
    while let Some(element) = stack.pop() {
        if element.hide {
            return Err(dashboard_core::refused(format!(
                "nukes artwork: {} is hidden; its client visibility state is not recorded",
                element.path
            )));
        }
        if !element.refusals().is_empty() {
            return Err(dashboard_core::refused(format!(
                "nukes artwork: {} has {} unsupported GUI fields: {:?}",
                element.path,
                element.refusals().len(),
                element.refusals()
            )));
        }
        if matches!(element.fields, Fields::Icon(_)) {
            icons.push(element);
        }
        stack.extend(element.nested());
    }
    if icons.len() != 1 {
        return Err(dashboard_core::refused(format!(
            "nukes artwork: topbar/nukes has {} icons; their semantic role is not recorded: {}",
            icons.len(),
            icons
                .iter()
                .map(|icon| icon.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let Fields::Icon(icon) = &icons[0].fields else {
        unreachable!()
    };
    let name = std::str::from_utf8(&icon.sprite).map_err(|error| invalid(error.to_string()))?;
    let sprite = catalogue::created_gui_sprite(&catalogue.sprites, name)?;
    let frame = u32::try_from(
        icon.frame
            .checked_sub(1)
            .ok_or_else(|| invalid("nukes artwork: frame overflow"))?,
    )
    .map_err(|_| {
        dashboard_core::refused(format!(
            "nukes artwork: {} has unsupported frame {}",
            icons[0].path, icon.frame
        ))
    })?;
    resolve_sprite_type(catalogue, sprite, frame, ICON_EDGE)
        .into_io_result()?
        .ok_or_else(|| invalid("nukes artwork unavailable"))
}

pub fn window_icon_url(
    source: &Source,
    content: Option<&dashboard_core::config::ContentOptions>,
    window: &str,
    icon: &str,
    edge: u32,
) -> Result<String, String> {
    let Some(content) = content else {
        return Err("the recorded content stack is not configured".to_owned());
    };
    let catalogue = catalogue::catalogue(source, content).map_err(|error| error.to_string())?;
    window_icon(&catalogue, window, icon, edge)
}

fn window_icon(catalogue: &Catalogue, window: &str, icon: &str, edge: u32) -> Result<String, String> {
    let guis = catalogue.guis().map_err(|error| error.to_string())?;
    let name = content_reader_gui::gui::native::window_icon_sprite(
        guis,
        window,
        icon,
        &mut Vec::new(),
    )
    .map_err(|error| error.to_string())?;
    let declared = content_reader_gui::gui::native::kept_container_window(guis, window, &mut Vec::new())
        .ok()
        .and_then(|(_, element)| element.find_recursive(content_reader_gui::gui::native::Kind::Icon, icon))
        .and_then(|element| element.icon())
        .map_or(1, |icon| icon.frame);
    let frame = declared
        .checked_sub(1)
        .and_then(|frame| u32::try_from(frame).ok())
        .ok_or_else(|| format!("{window}.{icon}: unsupported frame {declared}"))?;
    let sprite =
        catalogue::created_gui_sprite(&catalogue.sprites, &name).map_err(|error| error.to_string())?;
    resolve_sprite_type(catalogue, sprite, frame, edge)
        .into_result()?
        .ok_or_else(|| format!("{window}.{icon}: icon artwork unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::texture_tests::{from_url, png_bytes, texture_catalogue};
    use content_reader_image::{Rgba8Image, native::Decode};

    #[test]
    fn focus_text_keeps_authored_titles_without_reaching_conditional_icons() {
        use replay_data::{rows::TableId, rows::TableRows};
        use replay_frame::{frame::DecodedFrame, frame::TableView};
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/national_focus/mod.txt",
                    "focus_tree={focus={id=mod_focus text=mod_title icon={GFX_unavailable={has_country_flag=mod_flag}}} focus={id=mod_empty text=empty_title} focus={id=mod_static text=empty_title icon=GFX_mod_static}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n mod_title:0 \"Authored title\"\n mod_title_desc:0 \"Authored description\"\n empty_title:0 \"\"\n empty_title_desc:0 \"\"\n",
                ),
            ],
            &[],
        );
        let catalogue = Arc::new(catalogue);
        let national = Kind::Focus(FocusDatabase::National);
        let mut text = Context::load(Arc::clone(&catalogue), national).unwrap();
        let images = Context::load(catalogue, national).unwrap();
        assert!(Arc::ptr_eq(&text.definitions, &images.definitions));
        assert!(!text.requested_focus_text_needs_frame(&["mod_focus".into()]));
        assert!(text.requested_focus_needs_frame(&["mod_focus".into()]));
        let frame = DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|index| {
                TableView::Complete(TableRows::empty(TableId::ALL[index])).into()
            }),
            metrics: Default::default(),
        };
        let resolved: Vec<_> = text.focus_text_at(
            &[("mod_focus", "synthetic"), ("mod_empty", "synthetic")],
            &frame,
            |items| items.collect(),
        );
        assert_eq!(resolved[0].0.name.as_deref(), Some("Authored title"));
        assert_eq!(
            resolved[0].0.description.as_deref(),
            Some("Authored description")
        );
        assert_eq!(resolved[1].0.name.as_deref(), Some(""));
        assert!(
            resolved
                .iter()
                .all(|(art, reason)| art.icon.is_none() && reason.is_none())
        );
        assert!(text.icon_cache.lock().unwrap().is_empty());
        let (static_text, _) = text
            .text_catalogue(
                BTreeSet::from(["mod_focus", "mod_empty"]),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            static_text
                .iter()
                .find(|item| item.token == "mod_focus")
                .unwrap()
                .name
                .as_deref(),
            Some("Authored title")
        );
        assert_eq!(
            static_text
                .iter()
                .find(|item| item.token == "mod_empty")
                .unwrap()
                .name
                .as_deref(),
            Some("")
        );
        let (metadata, _) = text
            .focus_metadata(BTreeSet::from(["mod_focus", "mod_static"]), &AtomicBool::new(false))
            .unwrap();
        let sprites: Vec<_> = metadata.iter().map(|art| (art.token.as_str(), art.sprite.as_deref())).collect();
        assert_eq!(sprites, [("mod_focus", None), ("mod_static", Some("GFX_mod_static"))], "a triggered icon names no sprite");
    }

    #[test]
    fn each_focus_database_answers_only_for_its_own_focuses() {
        use replay_data::{rows::TableId, rows::TableRows};
        use replay_frame::{frame::DecodedFrame, frame::TableView};
        let frame = DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|index| {
                TableView::Complete(TableRows::empty(TableId::ALL[index])).into()
            }),
            metrics: Default::default(),
        };
        let files = |refused: bool| {
            let mut files = vec![
                (
                    "common/national_focus/mod.txt",
                    "focus_tree={focus={id=shared text=national_title cost=2} focus={id=national_only text=national_title cost=2}}",
                ),
                (
                    "common/continuous_focus/mod.txt",
                    "continuous_focus_palette={focus={id=shared text=continuous_title} focus={id=continuous_only text=continuous_title}}",
                ),
                (
                    "common/defines/mod.lua",
                    "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23},NFocus={FOCUS_POINT_DAYS=5}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n national_title:0 \"National title\"\n continuous_title:0 \"Continuous title\"\n",
                ),
            ];
            if refused {
                files.push((
                    "common/continuous_focus/zz_refused.txt",
                    "continuous_focus_palette={focus={text=continuous_title}}",
                ));
            }
            files
        };
        let contexts = |refused: bool| {
            let (catalogue, folders) = texture_catalogue(&files(refused), &[]);
            let catalogue = Arc::new(catalogue);
            let national =
                Context::load(Arc::clone(&catalogue), Kind::Focus(FocusDatabase::National))
                    .unwrap();
            let continuous =
                Context::load(catalogue, Kind::Focus(FocusDatabase::Continuous)).unwrap();
            (national, continuous, folders)
        };
        let texts = |context: &Context| {
            let tokens = BTreeSet::from(["continuous_only", "national_only", "shared"]);
            let (items, _) = context
                .text_catalogue(tokens, &AtomicBool::new(false))
                .unwrap();
            items
                .into_iter()
                .map(|item| (item.token, item.name, item.base_duration_days))
                .collect::<Vec<_>>()
        };
        let titles = |context: &mut Context, token: &str| {
            context
                .focus_titles_at(&[(token, "TAG")], &frame, |items| items.collect::<Vec<_>>())
                .remove(0)
        };
        let national_title = Some("National title".to_owned());
        let continuous_title = Some("Continuous title".to_owned());

        let (mut national, mut continuous, _folders) = contexts(false);
        assert_eq!(national.tokens().0, ["national_only", "shared"]);
        assert_eq!(continuous.tokens().0, ["continuous_only", "shared"]);
        assert_eq!(
            texts(&national),
            [
                ("continuous_only".to_owned(), None, None),
                ("national_only".to_owned(), national_title.clone(), Some(10.0)),
                ("shared".to_owned(), national_title.clone(), Some(10.0)),
            ]
        );
        assert_eq!(
            texts(&continuous),
            [
                ("continuous_only".to_owned(), continuous_title.clone(), None),
                ("national_only".to_owned(), None, None),
                ("shared".to_owned(), continuous_title.clone(), None),
            ]
        );
        let (art, reason) = titles(&mut national, "shared");
        assert_eq!((art.name, reason), (national_title.clone(), None));
        let (art, reason) = titles(&mut continuous, "shared");
        assert_eq!((art.name, reason), (continuous_title, None));
        let (art, reason) = titles(&mut continuous, "national_only");
        assert_eq!(art.name, None);
        assert_eq!(
            reason.as_deref(),
            Some("focus national_only: definition absent from recorded content")
        );

        let (mut national, mut continuous, _folders) = contexts(true);
        assert!(national.tokens().1.iter().all(|(_, note)| !note.contains("continuous_focus/")));
        assert!(continuous.tokens().1.iter().any(|(_, note)| note.contains("continuous_focus/zz_refused.txt")));
        assert_eq!(
            texts(&national),
            [
                ("continuous_only".to_owned(), None, None),
                ("national_only".to_owned(), national_title.clone(), Some(10.0)),
                ("shared".to_owned(), national_title.clone(), Some(10.0)),
            ]
        );
        let (art, reason) = titles(&mut national, "shared");
        assert_eq!((art.name, reason), (national_title, None));
        assert!(
            national
                .prepare("shared")
                .1
                .iter()
                .all(|finding| !finding.message.contains("continuous_focus"))
        );
        assert_eq!(
            texts(&continuous),
            [
                ("continuous_only".to_owned(), None, None),
                ("national_only".to_owned(), None, None),
                ("shared".to_owned(), None, None),
            ]
        );
        for token in ["shared", "national_only"] {
            let (art, reason) = titles(&mut continuous, token);
            assert_eq!(art.name, None);
            let reason = reason.unwrap();
            assert!(
                reason.contains("common/continuous_focus/zz_refused.txt"),
                "{reason}"
            );
            assert!(
                continuous.prepare(token).1.iter().any(|finding| finding
                    .message
                    .contains("common/continuous_focus/zz_refused.txt")),
                "{token}"
            );
        }
    }

    #[test]
    fn focus_display_projections_do_not_reach_unrequested_scoped_descriptions() {
        use replay_data::{rows::CountryLinkRow, rows::CountryRow, rows::TableId, rows::TableRows};
        use replay_frame::{frame::DecodedFrame, frame::TableView};
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/national_focus/mod.txt",
                    "focus_tree={focus={id=plain text=plain_title icon=GFX_mod} focus={id=empty text=empty_title icon=GFX_mod} focus={id=equal text=equal_title icon=GFX_mod} focus={id=scoped text=scoped_title icon=GFX_mod}}",
                ),
                (
                    "interface/art.gfx",
                    "spriteTypes={spriteType={name=GFX_mod textureFile=gfx/mod/icon.png}}",
                ),
                (
                    "common/defines/mod.lua",
                    "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}",
                ),
                (
                    "common/ideologies/mod.txt",
                    "ideologies={mod_group={types={mod_type={}}}}",
                ),
                (
                    "common/scripted_localisation/mod.txt",
                    "defined_text={name=Broken text={trigger={has_government=mod_group} localization_key=desc_known}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n plain_title:0 \"Authored literal title\"\n plain_title_desc:0 \"[Broken]\"\n empty_title:0 \"\"\n empty_title_desc:0 \"[Broken]\"\n equal_title:0 \"equal_title\"\n equal_title_desc:0 \"[Broken]\"\n scoped_title:0 \"[ROOT.GetTag] title\"\n scoped_title_desc:0 \"[Broken]\"\n desc_known:0 \"Reached description\"\n",
                ),
            ],
            &[(
                "gfx/mod/icon.png",
                png_bytes(&Rgba8Image {
                    width: 1,
                    height: 1,
                    pixels: vec![23, 0, 0, 255],
                }),
            )],
        );
        let mut context =
            Context::load(Arc::new(catalogue), Kind::Focus(FocusDatabase::National)).unwrap();
        let plain = ["plain".into(), "empty".into(), "equal".into()];
        assert!(!context.requested_focus_title_needs_frame(&plain));
        assert!(!context.requested_focus_display_needs_frame(&plain));
        assert!(context.requested_focus_text_needs_frame(&plain));
        assert!(context.requested_focus_needs_frame(&plain));
        assert!(context.requested_focus_title_needs_frame(&["scoped".into()]));
        let mut frame = DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|index| {
                TableView::Complete(TableRows::empty(TableId::ALL[index])).into()
            }),
            metrics: Default::default(),
        };
        *frame.tables[TableId::Countries.index()] =
            TableView::Complete(TableRows::Countries(vec![CountryRow {
                id: 7,
                canonical_slot: 2,
                tag: b"M0".to_vec(),
                ..Default::default()
            }]));
        *frame.tables[TableId::CountryLinks.index()] =
            TableView::Complete(TableRows::CountryLinks(vec![CountryLinkRow {
                id: 7,
                canonical_slot: 2,
            }]));
        *frame.tables[TableId::PoliticalParties.index()] = TableView::Unavailable {
            reason: "synthetic party read failed".into(),
        };
        let requests = [
            ("plain", "M0"),
            ("empty", "M0"),
            ("equal", "M0"),
            ("scoped", "M0"),
        ];
        let titles: Vec<_> = context.focus_titles_at(&requests, &frame, |items| items.collect());
        let expected = ["Authored literal title", "", "equal_title", "M0 title"];
        for ((art, reason), expected) in titles.iter().zip(expected) {
            assert_eq!(art.name.as_deref(), Some(expected));
            assert!(art.description.is_none());
            assert!(art.icon.is_none());
            assert!(reason.is_none(), "{reason:?}");
        }
        assert!(context.icon_cache.lock().unwrap().is_empty());
        let displayed: Vec<_> =
            context.focus_display_at(&requests, &frame, |items| items.collect());
        for ((art, reason), expected) in displayed.iter().zip(expected) {
            assert_eq!(art.name.as_deref(), Some(expected));
            assert!(art.description.is_none());
            assert_eq!(from_url(art.icon.as_deref().unwrap()).pixels[0], 23);
            assert!(reason.is_none(), "{reason:?}");
        }
        let detailed: Vec<_> = context.focus_text_at(&requests, &frame, |items| items.collect());
        for ((art, reason), expected) in detailed.iter().zip(expected) {
            assert_eq!(art.name.as_deref(), Some(expected));
            assert!(art.description.is_none());
            let reason = reason.as_deref().unwrap();
            assert!(reason.contains("_title_desc"), "{reason}");
            assert!(reason.contains("PoliticalParties"), "{reason}");
        }
    }

    #[test]
    fn focus_icons_evaluate_native_cases_and_preserve_caller_overrides() {
        use replay_data::{rows::CountryRow, rows::TableId, rows::TableRows, rows::tables};
        use replay_frame::{frame::DecodedFrame, frame::TableView};
        let icon = |value| Rgba8Image {
            width: 1,
            height: 1,
            pixels: vec![value, 0, 0, 255],
        };
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "interface/art.gfx",
                    "spriteTypes={spriteType={name=GFX_mod_scalar textureFile=gfx/mod/scalar.png} spriteType={name=GFX_mod_other textureFile=gfx/mod/other.png} spriteType={name=GFX_Z9_scalar textureFile=gfx/mod/other.png} spriteType={name=GFX_goal_unknown textureFile=gfx/mod/other.png}}",
                ),
                (
                    "common/national_focus/mod.txt",
                    "focus_tree={focus={id=scalar icon=GFX_mod_scalar} focus={id=alternate icon=GFX_mod_scalar alternate_icon=GFX_mod_other} focus={id=constant icon={GFX_mod_other=yes}} focus={id=conditional icon={GFX_mod_other={has_country_flag=mod_mark}}} focus={id=iconless}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n scalar:0 \"Scalar\"\n scalar_desc:0 \"Scalar description\"\n constant:0 \"Constant\"\n constant_desc:0 \"Constant description\"\n iconless:0 \"Iconless\"\n iconless_desc:0 \"Iconless description\"\n",
                ),
            ],
            &[
                ("gfx/mod/scalar.png", png_bytes(&icon(11))),
                ("gfx/mod/other.png", png_bytes(&icon(22))),
            ],
        );
        let mut context =
            Context::load(Arc::new(catalogue), Kind::Focus(FocusDatabase::National)).unwrap();
        assert!(context.focus_text_needs_frame());
        let mut frame = DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|i| {
                TableView::Complete(TableRows::empty(TableId::ALL[i])).into()
            }),
            metrics: Default::default(),
        };
        *frame.tables[TableId::Countries.index()] =
            TableView::Complete(TableRows::from_rows::<tables::Countries>(vec![
                CountryRow {
                    id: 7,
                    canonical_slot: 2,
                    tag: b"Z9".to_vec(),
                    ..Default::default()
                },
            ]));
        for token in ["scalar", "constant", "iconless"] {
            let (topic, _) = context.artwork(token).unwrap();
            let feed = context.focuses_at(&[(token, "Z9")], &frame).remove(0);
            assert_eq!(feed.0.icon, topic.icon, "{token}");
            assert_eq!(
                from_url(feed.0.icon.as_deref().unwrap()).pixels[0],
                if token == "scalar" { 11 } else { 22 }
            );
            assert!(feed.1.is_none());
        }
        let alternate = context.focuses_at(&[("alternate", "Z9")], &frame).remove(0);
        assert!(alternate.0.icon.is_none());
        assert!(alternate.1.unwrap().contains("focus-window mode"));
        let conditional = context
            .focuses_at(&[("conditional", "Z9")], &frame)
            .remove(0);
        assert!(conditional.0.icon.is_none());
        assert!(conditional.1.is_some());
        let tree =
            context.focus_tree_at(&[("scalar", "Z9")], &frame, |items| items.next().unwrap());
        assert_eq!(from_url(tree.0.icon.as_deref().unwrap()).pixels[0], 22);
        assert!(tree.1.is_none());
        let icons: Vec<_> = context.focus_icons_at(
            &[
                ("scalar", "Z9"),
                ("alternate", "Z9"),
                ("conditional", "Z9"),
                ("iconless", "Z9"),
                ("absent", "Z9"),
            ],
            &frame,
            |items| items.collect(),
        );
        assert_eq!(icons[0].0.icon, tree.0.icon);
        assert!(icons[0].1.is_none());
        assert!(icons[1].0.icon.is_none());
        assert!(icons[1].1.as_deref().unwrap().contains("focus-window mode"));
        assert!(icons[2].0.icon.is_none());
        assert!(icons[2].1.is_some());
        assert_eq!(from_url(icons[3].0.icon.as_deref().unwrap()).pixels[0], 22);
        assert!(icons[3].1.is_none());
        assert!(icons[4].0.icon.is_none());
        assert!(icons[4].1.as_deref().unwrap().contains("definition absent"));
        for (art, reason) in icons {
            assert!(art.name.is_none());
            assert!(art.description.is_none());
            assert!(!reason.is_some_and(|reason| reason.contains("focus localisation")));
        }
    }

    #[test]
    fn focus_art_uses_the_row_country_and_only_refuses_a_missing_random_seed() {
        use replay_data::{
            rows::ClockRow, rows::CountryLinkRow, rows::CountryRow, rows::PoliticalPartyRow,
            rows::TableId, rows::TableRows, rows::tables,
        };
        use replay_frame::{frame::DecodedFrame, frame::TableView};
        dashboard_text::catalogue::tests::script_game::enter();
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/national_focus/mod.txt",
                    "focus_tree={focus={id=scoped icon=GFX_scoped} focus={id=random icon=GFX_scoped} focus={id=promoted icon=GFX_scoped} focus={id=conditional icon={GFX_M0={tag=M0} GFX_other=yes}} focus={id=government icon={GFX_party={has_government=mod_party} GFX_other=yes}}}",
                ),
                ("common/ideologies/mod.txt", "ideologies={mod_party={}}"),
                (
                    "common/defines/mod.lua",
                    "NDefines={NGame={MAX_SCRIPTED_LOC_RECURSION=23}}",
                ),
                (
                    "common/scripted_localisation/mod.txt",
                    "a={name=Choice text={random_list={1={localization_key=A} 1={localization_key=B}}}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n scoped:0 \"[ROOT.GetTag] focus\"\n scoped_desc:0 \"For [FROM.GetTag]\"\n random:0 \"[Choice]\"\n random_desc:0 \"Plain description\"\n promoted:0 \"[ROOT.Choice]\"\n promoted_desc:0 \"\"\n A:0 \"A\"\n B:0 \"B\"\n",
                ),
            ],
            &[],
        );
        let mut context =
            Context::load(Arc::new(catalogue), Kind::Focus(FocusDatabase::National)).unwrap();
        assert!(context.focus_text_needs_frame());
        let (static_text, _) = context
            .text_catalogue(
                BTreeSet::from(["scoped", "random", "promoted"]),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(
            static_text
                .iter()
                .all(|item| item.name.is_none() && item.icon.is_none())
        );
        assert_eq!(
            static_text
                .iter()
                .find(|item| item.token == "random")
                .unwrap()
                .description
                .as_deref(),
            Some("Plain description")
        );
        assert!(context.refusal_lines().is_empty());
        let mut frame = DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|i| {
                TableView::Complete(TableRows::empty(TableId::ALL[i])).into()
            }),
            metrics: Default::default(),
        };
        *frame.tables[TableId::Countries.index()] =
            TableView::Complete(TableRows::from_rows::<tables::Countries>(vec![
                CountryRow {
                    id: 7,
                    canonical_slot: 2,
                    tag: b"M0".to_vec(),
                    ..Default::default()
                },
            ]));
        *frame.tables[TableId::CountryLinks.index()] =
            TableView::Complete(TableRows::from_rows::<tables::CountryLinks>(vec![
                CountryLinkRow {
                    id: 7,
                    canonical_slot: 2,
                },
            ]));
        *frame.tables[TableId::CountryCounterInputs.index()] =
            TableView::Complete(TableRows::CountryCounterInputs(vec![
                replay_data::rows::CountryCounterInputRow {
                    country: 7,
                    owned_state_count: 1,
                    ..Default::default()
                },
            ]));
        let result = context.focuses_at(
            &[("scoped", "M0"), ("random", "M0"), ("promoted", "M0")],
            &frame,
        );
        assert_eq!(result[0].0.name.as_deref(), Some("M0 focus"));
        assert_eq!(result[0].0.description.as_deref(), Some("For M0"));
        assert!(
            !result[0]
                .1
                .as_deref()
                .is_some_and(|reason| reason.contains("localisation"))
        );
        assert!(result[1].0.name.is_none());
        assert!(
            result[1]
                .1
                .as_deref()
                .unwrap()
                .contains("Clock seed is not recorded")
        );
        let conditional = context
            .focuses_at(&[("conditional", "M0")], &frame)
            .remove(0);
        assert!(
            conditional
                .1
                .as_deref()
                .unwrap()
                .contains("selected sprite GFX_M0 ")
        );
        *frame.tables[TableId::Clock.index()] =
            TableView::Complete(TableRows::from_rows::<tables::Clock>(vec![ClockRow {
                seed: 987,
                ..Default::default()
            }]));
        let seeded = context.focuses_at(&[("random", "M0")], &frame).remove(0);
        assert!(matches!(seeded.0.name.as_deref(), Some("A" | "B")));
        assert!(
            !seeded
                .1
                .as_deref()
                .is_some_and(|reason| reason.contains("localisation"))
        );
        assert_eq!(
            context.focuses_at(&[("random", "M0")], &frame)[0].0.name,
            seeded.0.name
        );
        let titles: Vec<_> =
            context.focus_titles_at(&[("random", "M0"), ("promoted", "M0")], &frame, |items| {
                items.collect()
            });
        assert_eq!(titles[0].0.name, seeded.0.name);
        assert_eq!(titles[1].0.name, result[2].0.name);
        assert!(
            titles
                .iter()
                .all(|(art, reason)| art.description.is_none() && reason.is_none())
        );
        assert!(matches!(result[2].0.name.as_deref(), Some("A" | "B")));
        assert!(
            !result[2]
                .1
                .as_deref()
                .is_some_and(|reason| reason.contains("localisation"))
        );
        assert_eq!(
            context
                .standing_warnings()
                .iter()
                .filter(
                    |warning| warning.contains("visual assets are outside content hash coverage")
                )
                .count(),
            1
        );
        assert!(
            !context
                .refusal_lines()
                .iter()
                .any(|reason| reason.contains("Clock seed is not recorded"))
        );
        frame.hour = 11;
        *frame.tables[TableId::Countries.index()] =
            TableView::Complete(TableRows::from_rows::<tables::Countries>(vec![
                CountryRow {
                    id: 7,
                    canonical_slot: 2,
                    tag: b"M1".to_vec(),
                    ..Default::default()
                },
                CountryRow {
                    id: 8,
                    canonical_slot: 3,
                    tag: b"M0".to_vec(),
                    ..Default::default()
                },
            ]));
        *frame.tables[TableId::CountryLinks.index()] =
            TableView::Complete(TableRows::CountryLinks(vec![
                CountryLinkRow {
                    id: 7,
                    canonical_slot: 2,
                },
                CountryLinkRow {
                    id: 8,
                    canonical_slot: 3,
                },
            ]));
        if let TableView::Complete(TableRows::CountryCounterInputs(rows)) =
            &mut *frame.tables[TableId::CountryCounterInputs.index()]
        {
            rows.push(replay_data::rows::CountryCounterInputRow {
                country: 8,
                owned_state_count: 1,
                ..Default::default()
            });
        }
        *frame.tables[TableId::States.index()] = TableView::Unavailable {
            reason: "synthetic state read failed".into(),
        };
        *frame.tables[TableId::Provinces.index()] = TableView::Unavailable {
            reason: "synthetic province read failed".into(),
        };
        *frame.tables[TableId::PoliticalParties.index()] =
            TableView::Complete(TableRows::PoliticalParties(vec![PoliticalPartyRow {
                country: 7,
                id: 1,
                is_ruling: 1,
                token: b"mod_party".to_vec(),
                ..Default::default()
            }]));
        let government = context
            .focuses_at(&[("government", "M1")], &frame)
            .remove(0);
        assert!(
            government
                .1
                .as_deref()
                .unwrap()
                .contains("selected sprite GFX_party ")
        );
        *frame.tables[TableId::PoliticalParties.index()] = TableView::Unavailable {
            reason: "synthetic party read failed".into(),
        };
        let government = context
            .focuses_at(&[("government", "M1")], &frame)
            .remove(0);
        let reason = government.1.as_deref().unwrap();
        assert!(reason.contains("PoliticalParties"), "{reason}");
        assert!(
            !reason.contains("selected sprite GFX_other "),
            "unknown first case must not be skipped"
        );
        let conditional = context
            .focuses_at(&[("conditional", "M1")], &frame)
            .remove(0);
        let reason = conditional.1.as_deref().unwrap();
        assert!(reason.contains("selected sprite GFX_other "), "{reason}");
        assert_eq!(
            context.focuses_at(&[("scoped", "M1")], &frame)[0]
                .0
                .name
                .as_deref(),
            Some("M1 focus")
        );
    }

    #[test]
    fn technology_limits_use_load_time_dlcs_empty_scope_and_unknown_globals() {
        use engine_native::cequipment_variant::FLOATING_HARBOR_CLASSIFIER_MASK;
        crate::test_fixtures::game::executable()
            .auxiliary(|tables| {
                tables.category_names.as_mut().unwrap().push(("fixture_dock", FLOATING_HARBOR_CLASSIFIER_MASK));
            })
            .enter();
        let feature_dlc = engine_dlc::dlc_feature_state::NAME_MASKS
            .iter()
            .find(|(_, mask)| *mask == 1 << 7)
            .unwrap()
            .0;
        let technologies = format!(
            "technologies={{dlc={{enable_equipments={{limit={{has_dlc=\"{feature_dlc}\"}} mod_child}}}} empty={{enable_equipments={{limit={{has_government=mod_party}} mod_child}}}} global={{enable_equipments={{limit={{has_global_flag=mod_flag}} mod_child}}}} invalid={{enable_equipments={{missing_child}}}} harbor={{enable_equipments={{mod_harbor}}}}}}"
        );
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/units/equipment/mod.txt",
                    "equipments={mod_child={} mod_harbor={type=fixture_dock}}",
                ),
                ("common/technologies/mod.txt", &technologies),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
            ],
            &[],
        );
        let layers = &catalogue.content.layers;
        let presentation = content_reader_military::technology::Database::read(layers).unwrap();
        let registry = layers.key_name_products().unwrap();
        let again = layers.key_name_products().unwrap();
        assert!(Arc::ptr_eq(
            registry.equipment_database().unwrap(),
            again.equipment_database().unwrap()
        ));
        let triggers = content_reader_script::trigger_keys::TriggerNames::default();
        let aliases = Arc::new(content_reader_key_names::tag_aliases::TagAliases::default());
        let empty = BTreeSet::new();
        let world = crate::triggers::World::unrecorded(&empty, &triggers, &aliases);
        let named = |key: &str| {
            presentation
                .definitions
                .iter()
                .find(|definition| definition.token == key)
                .unwrap()
                .presentation
                .as_ref()
                .unwrap()
        };
        let select = |key: &str| technology_name(named(key), Ok(&world), registry.equipment_database());
        assert_eq!(select("dlc").unwrap(), "dlc");
        assert_eq!(select("empty").unwrap(), "empty");
        let reason = select("global").unwrap_err();
        assert!(
            reason.contains("LexicalTokens") && reason.contains("content load"),
            "{reason}"
        );
        assert_eq!(select("invalid").unwrap(), "invalid");
        assert_eq!(select("harbor").unwrap(), "harbor");
        let enabled = BTreeSet::from([feature_dlc.to_owned()]);
        let world = crate::triggers::World::unrecorded(&enabled, &triggers, &aliases);
        assert_eq!(
            technology_name(named("dlc"), Ok(&world), registry.equipment_database()).unwrap(),
            "mod_child"
        );
        let enabled = BTreeSet::from([feature_dlc.to_owned()]);
        let world = crate::triggers::World::unrecorded(&enabled, &triggers, &aliases);
        assert_eq!(
            technology_name(named("harbor"), Ok(&world), registry.equipment_database()).unwrap(),
            "mod_harbor"
        );
        assert!(
            technology_name(
                named("harbor"),
                Err("DLC load state unavailable".into()),
                registry.equipment_database()
            )
            .unwrap_err()
            .contains("DLC load state")
        );
    }

    #[test]
    fn technology_catalogue_keeps_parent_text_and_skips_unrequested_children() {
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/technologies/mod.txt",
                    "technologies={mod_tech={parent=mod_parent} mod_unrequested={enable_equipments={limit={has_global_flag=mod_flag} missing_child}}}",
                ),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n mod_tech:0 \"Wrong id\"\n mod_parent:0 \"Parent\"\n mod_parent_short:0 \"Short parent\"\n M0_mod_parent:0 \"Country full\"\n M0_mod_parent_short:0 \"Wrong short\"\n M0_mod_parent_desc:0 \"Country description\"\n mod_parent_desc:0 \"Parent description\"\n mod_tech_desc:0 \"Token description\"\n",
                ),
            ],
            &[],
        );
        let mut context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        assert!(
            context
                .technology_names
                .names
                .values()
                .flatten()
                .all(|name| name.children.get().is_none())
        );
        let (topic, _) = context
            .catalogue(BTreeSet::from(["mod_tech"]), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(topic[0].name.as_deref(), Some("Parent"));
        assert_eq!(topic[0].description.as_deref(), Some("Parent description"));
        assert!(
            context.technology_names.names["mod_unrequested"][0]
                .children
                .get()
                .is_none()
        );
    }

    #[test]
    fn a_repeated_technology_is_named_by_its_recorded_template() {
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/technologies/mod.txt",
                    "technologies={repeat={parent=first_name} REPEAT={parent=second_name} same={} same={}}",
                ),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n first_name:0 \"First\"\n second_name:0 \"Second\"\n same:0 \"Same\"\n",
                ),
            ],
            &[],
        );
        let context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        let named = |token: &str, at: Option<u32>| {
            context
                .recorded_text_catalogue(BTreeMap::from([(token, at)]), &AtomicBool::new(false))
                .unwrap()
                .0
                .remove(0)
                .name
        };
        assert_eq!(named("repeat", Some(1)).as_deref(), Some("First"));
        assert_eq!(named("repeat", Some(2)).as_deref(), Some("Second"));
        assert_eq!(named("REPEAT", Some(1)).as_deref(), Some("First"));
        assert_eq!(named("repeat", None), None);
        assert!(
            context
                .technology("repeat", None)
                .err()
                .is_some_and(|reason| reason.contains("defined 2 times")),
        );
        assert_eq!(named("same", None).as_deref(), Some("Same"));
        let mut context = context;
        let mut requested = |uses: &[Option<u32>]| {
            context
                .recorded_catalogue(BTreeMap::from([("repeat", uses)]), &AtomicBool::new(false))
                .unwrap()
        };
        let unrecorded = |warnings: &[String]| {
            warnings
                .iter()
                .any(|warning| warning.starts_with("repeat: ") && warning.contains("defined 2 times"))
        };
        let (items, warnings) = requested(&[Some(2)]);
        assert_eq!(items[0].name.as_deref(), Some("Second"));
        assert!(!unrecorded(&warnings), "{warnings:?}");
        let (items, warnings) = requested(&[None, Some(2)]);
        assert_eq!(items[0].name, None);
        assert!(unrecorded(&warnings), "{warnings:?}");
    }

    #[test]
    fn a_template_past_a_file_not_read_whole_is_refused() {
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/technologies/00_broken.txt",
                    "technologies={repeat={parent=lost_name folder={name=a position={1}}}}",
                ),
                (
                    "common/technologies/mod.txt",
                    "technologies={repeat={parent=second_name} same={}}",
                ),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n second_name:0 \"Second\"\n same:0 \"Same\"\n",
                ),
            ],
            &[],
        );
        let context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        let named = |token: &str, at: Option<u32>| {
            context
                .recorded_text_catalogue(BTreeMap::from([(token, at)]), &AtomicBool::new(false))
                .unwrap()
                .0
                .remove(0)
                .name
        };
        assert_eq!(named("repeat", Some(1)), None);
        assert!(
            context
                .technology("repeat", Some(1))
                .err()
                .is_some_and(|reason| reason.contains("00_broken.txt")),
        );
        assert_eq!(named("repeat", None), None);
        assert_eq!(named("same", None).as_deref(), Some("Same"));
    }

    #[test]
    fn technology_descriptions_use_distinct_child_gate_and_archetype_fallback() {
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "common/units/equipment/mod.txt",
                    "equipments={mod_base={is_archetype=yes} mod_child={archetype=mod_base} mod_fallback={archetype=mod_base}} duplicate_archetypes={mod_copy={archetype=mod_base}}",
                ),
                (
                    "common/terrain/mod.txt",
                    "categories={mod_ground={movement_cost=1}}",
                ),
                (
                    "common/technologies/mod.txt",
                    "technologies={direct={parent=mod_parent enable_equipments={mod_child}} fallback={enable_equipments={mod_fallback}} duplicates={parent=mod_parent enable_equipments={mod_copy mod_child}} forced={parent=mod_parent enable_equipments={mod_child} force_use_small_tech_layout=yes}}",
                ),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n direct_desc:0 \"Wrong token description\"\n mod_child_desc:0 \"Child description\"\n M0_mod_child_desc:0 \"Country child description\"\n mod_base_desc:0 \"Archetype description\"\n M0_mod_base_desc:0 \"Country archetype description\"\n mod_parent_desc:0 \"Parent description\"\n",
                ),
            ],
            &[],
        );
        let context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        assert_eq!(
            context.description("direct").as_deref(),
            Some("Child description")
        );
        assert_eq!(
            context.description_key("direct", Some("M0")).unwrap(),
            "M0_mod_child_desc"
        );
        assert_eq!(
            context.description("forced").as_deref(),
            Some("Parent description")
        );
        assert_eq!(context.technology_root("duplicates").unwrap(), "mod_parent");
        assert_eq!(context.description("duplicates"), None);
        assert_eq!(
            context.description("fallback").as_deref(),
            Some("Archetype description")
        );
        assert_eq!(
            context.description_key("fallback", Some("M0")).unwrap(),
            "M0_mod_base_desc"
        );
        assert_eq!(
            context.description_key("duplicates", Some("M0")).unwrap(),
            "mod_copy_desc"
        );
    }

    #[test]
    fn nuke_artwork_uses_the_recorded_gui_sprite_and_authored_frame() {
        let sheet = Rgba8Image {
            width: 2,
            height: 1,
            pixels: vec![11, 0, 0, 255, 22, 0, 0, 255],
        };
        let (catalogue, _folders) = texture_catalogue(
            &[
                (
                    "interface/mod.gui",
                    "guiTypes={containerWindowType={name=topbar containerWindowType={name=nukes iconType={name=authored_stockpile spriteType=GFX_mod_stockpile frame=2}}}}",
                ),
                (
                    "interface/mod.gfx",
                    "spriteTypes={spriteType={name=GFX_mod_stockpile textureFile=gfx/mod/sheet.png noOfFrames=2}}",
                ),
            ],
            &[("gfx/mod/sheet.png", png_bytes(&sheet))],
        );
        assert_eq!(
            from_url(nuke_icon(&catalogue).unwrap().as_deref().unwrap()).pixels[0],
            22
        );
        assert_eq!(
            from_url(&window_icon(&catalogue, "topbar", "authored_stockpile", ICON_EDGE).unwrap()).pixels[0],
            22,
            "a window icon draws the frame its interface declares, as the fuel branches' icons do"
        );
        let (catalogue, _folders) = texture_catalogue(
            &[(
                "interface/mod.gui",
                "guiTypes={containerWindowType={name=topbar containerWindowType={name=nukes iconType={name=first} iconType={name=second}}}}",
            )],
            &[],
        );
        assert!(
            nuke_icon(&catalogue)
                .unwrap_err()
                .to_string()
                .contains("2 icons")
        );
    }

    #[test]
    fn mod_png_artwork_and_native_refusals_reach_the_catalogue_reply() {
        let image = Rgba8Image {
            width: 6,
            height: 3,
            pixels: (0..18).flat_map(|x| [x, 20, 30, 200]).collect(),
        };
        let (mut catalogue, _folders) = texture_catalogue(
            &[
                (
                    "interface/art.gfx",
                    concat!(
                        "spriteTypes = { ",
                        "spriteType = { name = GFX_mod_tech_medium textureFile = \"gfx/mod/sheet.png\" noOfFrames = 3 } ",
                        "spriteType = { name = GFX_bad_tech_medium textureFile = \"gfx/mod/noise.png\" } ",
                        "spriteType = { name = GFX_unknown_tech_medium textureFile = \"gfx/mod/tiny.png\" } ",
                        "spriteType = { name = GFX_absent_tech_medium textureFile = \"gfx/mod/absent.png\" } }",
                    ),
                ),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
            ],
            &[
                ("gfx/mod/sheet.png", png_bytes(&image)),
                ("gfx/mod/noise.png", vec![0x13; 20]),
                ("gfx/mod/tiny.png", b"DDS ".to_vec()),
            ],
        );
        catalogue.gui_decode = Ok(Decode {
            force_pow2_textures: true,
        });
        let mut context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        let tokens = BTreeSet::from(["mod_tech", "bad_tech", "unknown_tech", "absent_tech"]);
        let cancelled = AtomicBool::new(false);
        let (artwork, warnings) = context.catalogue(tokens.clone(), &cancelled).unwrap();
        let icon = artwork
            .iter()
            .find(|art| art.token == "mod_tech")
            .unwrap()
            .icon
            .as_deref()
            .unwrap();
        let frame = from_url(icon);
        assert_eq!((frame.width, frame.height), (2, 4));
        assert_eq!(&frame.pixels[..8], [0, 20, 30, 200, 1, 20, 30, 200]);
        assert_eq!(&frame.pixels[24..], [0; 8]);
        for (token, reason) in [
            ("bad_tech", "cannot be decoded"),
            ("unknown_tech", "undefined format behaviour: a 4-byte DDS"),
            ("absent_tech", r#"no layer holds any of ["gfx/mod/absent.dds", "gfx/mod/absent.tga", "gfx/mod/absent.png"]"#),
        ] {
            assert!(
                artwork
                    .iter()
                    .find(|art| art.token == token)
                    .unwrap()
                    .icon
                    .is_none()
            );
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains(token) && warning.contains(reason)),
                "{warnings:?}"
            );
        }
        let (_, cached_warnings) = context.catalogue(tokens, &cancelled).unwrap();
        assert_eq!(cached_warnings, warnings);
    }

    #[test]
    fn every_texture_refusal_reaches_artwork_warnings() {
        let refusals = crate::catalogue::texture_tests::refusal_textures();
        let sprites = format!(
            "spriteTypes = {{ {} }}",
            refusals
                .iter()
                .enumerate()
                .map(|(i, (path, _, _))| format!(
                    "spriteType = {{ name = GFX_mod_refusal_{i}_medium textureFile = \"{path}\" }}"
                ))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let textures: Vec<_> = refusals
            .iter()
            .filter(|(_, bytes, _)| !bytes.is_empty())
            .map(|(path, bytes, _)| (*path, bytes.clone()))
            .collect();
        let (catalogue, _folders) = texture_catalogue(
            &[
                ("interface/art.gfx", &sprites),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
            ],
            &textures,
        );
        let mut context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        let tokens: Vec<_> = (0..refusals.len())
            .map(|i| format!("mod_refusal_{i}"))
            .collect();
        let cancelled = AtomicBool::new(false);
        let (artwork, warnings) = context
            .catalogue(tokens.iter().map(String::as_str).collect(), &cancelled)
            .unwrap();
        assert!(artwork.iter().all(|art| art.icon.is_none()));
        let reasons = [
            "no layer holds any of",
            "has no extension",
            "cannot be decoded",
            "undefined format behaviour: a 4-byte DDS",
            "depends on device limits",
            "is a cube map, not a 2D UI texture",
            "first level runs past the file",
            "past the 16 MiB texture limit",
        ];
        assert_eq!(refusals.len(), reasons.len());
        for ((token, (path, _, _)), reason) in tokens.iter().zip(&refusals).zip(reasons) {
            let prefix = format!("{token}: icon unavailable: ");
            assert!(
                warnings.iter().any(|warning| {
                    warning.starts_with(&prefix) && warning.contains(path) && warning.contains(reason)
                }),
                "{warnings:?}"
            );
        }
        let (_, cached) = context
            .catalogue(tokens.iter().map(String::as_str).collect(), &cancelled)
            .unwrap();
        assert_eq!(cached, warnings);
    }

    #[test]
    fn unknown_gui_decode_is_the_items_problem_and_keeps_its_text() {
        let (mut catalogue, _folders) = texture_catalogue(
            &[
                (
                    "interface/art.gfx",
                    "spriteTypes = { spriteType = { name = GFX_mod_tech_medium textureFile = gfx/mod/icon.png } }",
                ),
                (
                    "common/terrain/mod.txt",
                    "categories = { mod_ground = { movement_cost = 1 } }",
                ),
                ("common/technologies/mod.txt", "technologies={mod_tech={}}"),
                (
                    "localisation/mod_l_english.yml",
                    "\u{feff}l_english:\n mod_tech:0 \"Mod technology\"\n mod_tech_desc:0 \"Mod description\"\n",
                ),
            ],
            &[],
        );
        let reason = "settings.txt is unavailable: test read failure";
        catalogue.gui_decode = Err(reason.into());
        let mut context = Context::load(Arc::new(catalogue), Kind::Technology).unwrap();
        let (artwork, warnings) = context
            .catalogue(BTreeSet::from(["mod_tech"]), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(artwork[0].name.as_deref(), Some("Mod technology"));
        assert_eq!(artwork[0].description.as_deref(), Some("Mod description"));
        assert!(artwork[0].icon.is_none());
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| **warning == format!("mod_tech: icon unavailable: {reason}"))
                .count(),
            1,
            "{warnings:?}"
        );
    }
}

#[cfg(test)]
mod interface_refusals {
    use crate::catalogue::{Catalogue, tests::folder_catalogue};
    use crate::products::artwork;
    use content_reader_layers::ResolvedLayers;
    use std::collections::BTreeSet;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn d16_fresh_text_only_reply_keeps_interface_refusal_summary() {
        let (_, folder) = folder_catalogue(&[
            (
                "interface/synthetic.gfx",
                "spriteTypes={spriteType={name=GFX_refused noOfFrames=-1}}",
            ),
            (
                "common/national_focus/synthetic.txt",
                "focus_tree={focus={id=synthetic}}",
            ),
            (
                "localisation/synthetic_l_english.yml",
                "\u{feff}l_english:\n synthetic:0 \"Synthetic focus\"\n synthetic_desc:0 \"Synthetic description\"\n",
            ),
        ]);
        let layers = ResolvedLayers::from_folders(&[(&folder.0, &[][..])]).unwrap();
        let catalogue = Arc::new(Catalogue::of_layers(layers, "english").unwrap());
        let context =
            artwork::Context::load(catalogue, artwork::Kind::Focus(crate::FocusDatabase::National))
                .unwrap();
        let (items, warnings) = context
            .text_catalogue(BTreeSet::from(["synthetic"]), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(items[0].name.as_deref(), Some("Synthetic focus"));
        assert_eq!(
            items[0].description.as_deref(),
            Some("Synthetic description")
        );
        let summary = warnings
            .iter()
            .filter(|line| line.contains("refusals in its interface files"))
            .collect::<Vec<_>>();
        assert_eq!(summary.len(), 1, "{warnings:?}");
        assert!(summary[0].contains("1 refusals"), "{summary:?}");
        assert!(summary[0].contains("GFX_refused"), "{summary:?}");
        assert!(summary[0].contains("synthetic.gfx"), "{summary:?}");
    }
}

#[cfg(test)]
mod presentation {
    use super::{Context, Kind};
    use crate::catalogue;
    use crate::test_fixtures::visual_mods::{
        BLUE, GREEN, Machine, RED, fallback_line, first_pixel, png, select,
    };
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicBool;

    fn empty_frame() -> replay_frame::frame::DecodedFrame {
        use replay_data::{rows::TableId, rows::TableRows};
        use replay_frame::frame::{DecodedFrame, TableView};
        DecodedFrame {
            branch: 1,
            hour: 10,
            tables: std::array::from_fn(|index| {
                TableView::Complete(TableRows::empty(TableId::ALL[index])).into()
            }),
            metrics: Default::default(),
        }
    }

    #[test]
    fn a_mods_focus_tree_override_is_not_read_and_its_art_that_cannot_draw_is_the_recordings() {
        let machine = Machine::new("focus_override");
        machine.write(
            "common/national_focus/tree.txt",
            b"focus_tree={focus={id=f_one icon=GFX_f_one_icon}}",
        );
        machine.write(
            "interface/focus.gfx",
            b"spriteTypes={spriteType={name=GFX_f_one_icon textureFile=gfx/a.png}}",
        );
        machine.write("gfx/a.png", &png(RED));
        for (name, art) in [("art", png(GREEN)), ("broken", b"not a png".to_vec())] {
            machine.write_mod(
                name,
                "interface/override.gfx",
                b"spriteTypes={spriteType={name=GFX_TAG_f_one textureFile=gfx/b.png}}",
            );
            machine.write_mod(name, "gfx/b.png", &png(BLUE));
            machine.write_mod(name, "gfx/a.png", &art);
        }
        let (source, mut options) = machine.open();
        let frame = empty_frame();
        let shown = |options: &crate::products::ContentOptions| {
            let mut context = Context::load(
                catalogue::catalogue(&source, options).unwrap(),
                Kind::Focus(crate::FocusDatabase::National),
            )
            .unwrap();
            (0..2)
                .map(|_| {
                    let (items, line) = fallback_line(|| {
                        context.focus_icons_at(&[("f_one", "TAG")], &frame, |items| {
                            items.collect::<Vec<_>>()
                        })
                    });
                    let (art, refusal) = &items[0];
                    assert_eq!(*refusal, None);
                    (first_pixel(art.icon.as_deref().unwrap()), line)
                })
                .collect::<Vec<_>>()
        };
        select(&source, &mut options, &[]);
        assert_eq!(shown(&options), vec![(RED, None), (RED, None)]);
        select(&source, &mut options, &["art"]);
        assert_eq!(shown(&options), vec![(GREEN, None), (GREEN, None)]);
        select(&source, &mut options, &["broken"]);
        for (colour, line) in shown(&options) {
            assert_eq!(colour, RED, "the recording's own choice");
            assert!(
                line.as_ref().is_some_and(|line| line.contains("focus f_one icon")),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_focus_icon_the_mod_cannot_draw_is_the_recordings_in_the_catalogue_reply() {
        let machine = Machine::new("focus_catalogue_icon");
        machine.write(
            "common/national_focus/tree.txt",
            b"focus_tree={focus={id=f_one icon=GFX_f_one_icon}}",
        );
        machine.write(
            "interface/focus.gfx",
            b"spriteTypes={spriteType={name=GFX_f_one_icon textureFile=gfx/a.png}}",
        );
        machine.write("gfx/a.png", &png(RED));
        machine.write_mod("broken", "gfx/a.png", b"not a png");
        let (source, mut options) = machine.open();
        let reply = |options: &crate::products::ContentOptions| {
            let mut context = Context::load(
                catalogue::catalogue(&source, options).unwrap(),
                Kind::Focus(crate::FocusDatabase::National),
            )
            .unwrap();
            (0..2)
                .map(|_| {
                    let ((art, _), line) = fallback_line(|| {
                        context
                            .catalogue(BTreeSet::from(["f_one"]), &AtomicBool::new(false))
                            .unwrap()
                    });
                    (first_pixel(art[0].icon.as_deref().unwrap()), line)
                })
                .collect::<Vec<_>>()
        };
        select(&source, &mut options, &[]);
        assert_eq!(reply(&options), vec![(RED, None), (RED, None)]);
        select(&source, &mut options, &["broken"]);
        for (colour, line) in reply(&options) {
            assert_eq!(colour, RED);
            assert!(
                line.as_ref().is_some_and(|line| line.contains("focus f_one icon")),
                "{line:?}"
            );
        }
    }

    #[test]
    fn tree_icon_plans_cover_the_recordings_own_choice() {
        let machine = Machine::new("focus_plans");
        machine.write(
            "common/national_focus/tree.txt",
            b"focus_tree={focus={id=f_one icon={GFX_f_one_icon={has_country_flag=f_flag}}}}",
        );
        machine.write(
            "interface/focus.gfx",
            b"spriteTypes={spriteType={name=GFX_f_one_icon textureFile=gfx/a.png}               spriteType={name=GFX_GER_f_one textureFile=gfx/a.png}}",
        );
        machine.write("gfx/a.png", &png(RED));
        machine.write_mod(
            "override",
            "interface/focus.gfx",
            b"spriteTypes={spriteType={name=GFX_f_one_icon textureFile=gfx/a.png}               spriteType={name=GFX_f_one textureFile=gfx/broken.png}}",
        );
        machine.write_mod("override", "gfx/a.png", &png(RED));
        machine.write_mod("override", "gfx/broken.png", b"not a png");
        let (source, mut options) = machine.open();
        select(&source, &mut options, &["override"]);
        let shown = catalogue::catalogue(&source, &options).unwrap();
        assert!(!shown.presentation.is_empty());
        assert!(shown.sprites.find("GFX_GER_f_one").unwrap().is_some());
        assert!(shown.sprites.find("GFX_f_one").unwrap().is_none());
        let context = Context::load(shown, Kind::Focus(crate::FocusDatabase::National)).unwrap();
        assert!(context.requested_tree_focus_needs_frame("f_one", "ITA", super::FocusProjection::Icons));
        assert!(context.focus_country_icon_override("f_one", "GER"));
        assert!(!context.focus_country_icon_override("f_one", "ITA"));
    }
}

pub(crate) fn shown_folders<'a>(
    definitions: &[(&str, Option<bool>)],
    placed: &BTreeMap<(String, Option<u32>), Vec<String>>,
    tokens: impl IntoIterator<Item = (&'a str, Option<u32>)>,
) -> (Vec<crate::ResearchTechnologyFolder>, Vec<String>) {
    let mut undecided: Vec<&str> = Vec::new();
    let mut undefined: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut shown = Vec::new();
    for (token, database_index) in tokens {
        let mut folder = None;
        let mut unknown = false;
        for key in placed
            .get(&(token.to_owned(), database_index))
            .into_iter()
            .flatten()
        {
            match definitions
                .iter()
                .find(|(defined, _)| *defined == key.as_str())
            {
                Some((_, Some(true))) => {
                    folder = Some(key.clone());
                    break;
                }
                Some((_, Some(false))) => {}
                Some((_, None)) => {
                    unknown = true;
                    break;
                }
                None => undefined.entry(key.as_str()).or_default().push(token),
            }
        }
        if unknown {
            undecided.push(token);
        }
        shown.push(crate::ResearchTechnologyFolder {
            token: token.to_owned(),
            database_index,
            folder,
        });
    }
    let mut notes = Vec::new();
    if !undecided.is_empty() {
        let one = undecided.len() == 1;
        notes.push(format!(
            "{} {} placed in a research folder whose availability cannot be decided from content before any available folder, so {} folder is unknown: {}.",
            undecided.len(),
            if one { "technology is" } else { "technologies are" },
            if one { "its" } else { "their" },
            undecided.join(", ")
        ));
    }
    for (key, tokens) in undefined {
        notes.push(format!(
            "Research folder {key} is not defined in common/technology_tags, so {} {} placed in it cannot be shown there: {}.",
            tokens.len(),
            if tokens.len() == 1 { "technology" } else { "technologies" },
            tokens.join(", ")
        ));
    }
    (shown, notes)
}
