
use dashboard_base::products::{LoadContext, Product};
use dashboard_base::products::artwork::{ArtFile, ArtResolution, Searched, art_file};
use dashboard_base::{Source, catalogue::Catalogue, content};
use dashboard_text::flag_choices::Chain;
use dashboard_core::invalid;
use content_reader_layers::content::diagnostics::Once as OnceLock;
use content_reader_layers::{ResolveOptions, content::ResolvedContent};
use content_reader_gui::flags::FlagSet;
use content_reader_image::Rgba8Image;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

pub struct CountryFlags;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum CountryFlagSize {
    #[default]
    Full,
    Medium,
    Small,
    Shield,
    Panel,
}

impl CountryFlagSize {
    pub fn sets(self) -> &'static [FlagSet] {
        match self {
            Self::Full => &[FlagSet::Normal],
            Self::Medium => &[FlagSet::Medium, FlagSet::Normal],
            Self::Small => &[FlagSet::Small],
            Self::Shield | Self::Panel => &[],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryFlagChoice {
    #[doc = " Each atlas's key; null where no candidate gives it a file it draws."]
    pub keys: [Option<String>; 3],
    #[doc = " Why each atlas has no key, or what was refused on the way to its key; null when nothing."]
    pub reasons: [Option<String>; 3],
    #[doc = " True where the atlas has no key because a completed search of every candidate found no file."]
    #[serde(default, skip_serializing_if = "no_search", deserialize_with = "dashboard_base::products::artwork::relayed_searches")]
    #[cfg_attr(any(test, feature = "export-types"), ts(as = "Option<[Option<bool>; 3]>", optional))]
    pub searched: [Option<Searched>; 3],
}

fn no_search(searched: &[Option<Searched>; 3]) -> bool {
    searched.iter().all(Option::is_none)
}

fn atlas(set: FlagSet) -> usize {
    match set {
        FlagSet::Normal => 0,
        FlagSet::Medium => 1,
        FlagSet::Small => 2,
    }
}

const TOP_BAR: &str = "topbar";
const PLAYER_FLAG: &str = "player_flag";
const MAX_FLAG_EDGE: u32 = 128;
const MAX_FLAGS: usize = 32;
const UNAVAILABLE: &str = "flag artwork unavailable in the recorded content";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountryFlagParams {
    #[doc = " The choices to draw, as `country-facts` gives them."]
    pub flags: Vec<CountryFlagChoice>,
    #[serde(default)]
    pub size: CountryFlagSize,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CountryFlagImage {
    #[doc = " The choice drawn: its index in the request's `flags`."]
    pub flag: u32,
    #[doc = " The flag file drawn, as the content names it."]
    pub asset: String,
    #[doc = " What the flag shows: its file and a digest of its pixels, so two flags share it only"]
    #[doc = " when they show the same picture (the visual mods' file and the recording's of one path"]
    #[doc = " are two)."]
    pub key: String,
    pub width: u32,
    pub height: u32,
    #[doc = " The flag as its artwork URL, at most 128 pixels on the longer side."]
    pub png: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub resolution: Option<dashboard_base::products::artwork::ArtResolution>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CountryFlagUnavailable {
    #[doc = " The choice: its index in the request's `flags`."]
    pub flag: u32,
    pub reason: String,
    pub resolution: dashboard_base::products::artwork::ArtResolution,
}

fn unavailable_flag(flag: u32, resolution: dashboard_base::products::artwork::ArtResolution) -> CountryFlagUnavailable {
    CountryFlagUnavailable {
        flag,
        reason: resolution.reason().unwrap_or(UNAVAILABLE).to_owned(),
        resolution,
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CountryFlagReply {
    pub size: CountryFlagSize,
    pub flags: Vec<CountryFlagImage>,
    pub unavailable: Vec<CountryFlagUnavailable>,
    pub warnings: Vec<String>,
}

impl Product for CountryFlags {
    const KEY: &'static str = "country-flags";
    const CONTENT_DEPENDENT: bool = true;
    const INTERACTIVE: bool = true;
    const PREPARE: Option<dashboard_base::products::read_ahead::Prepare<Self::Params>> = Some(|source, params, load| {
        if !matches!(params.size, CountryFlagSize::Shield | CountryFlagSize::Panel) {
            return Ok(None);
        }
        dashboard_base::products::artwork::prepare_artwork_for::<ShieldStyle>(source, load, |source, catalogue| {
            let style = shield_for(source, catalogue);
            if let Ok(style) = style {
                let layers = style.recorded.as_ref().map_or(catalogue.content.layers.presentation(), |(layers, _)| layers);
                style.shield.prepare(layers, style.decode);
            }
        })
    });
    type Params = CountryFlagParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![flags, size];
    type Reply = CountryFlagReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            CountryFlagSize::decl(),
            CountryFlagImage::decl(),
            CountryFlagUnavailable::decl(),
            CountryFlagParams::decl(),
            CountryFlagReply::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        if params.flags.is_empty() || params.flags.len() > MAX_FLAGS {
            return Err("Invalid country flag selection");
        }
        if params
            .flags
            .iter()
            .flat_map(|choice| choice.keys.iter().flatten())
            .any(|key| !safe_flag_key(key))
        {
            return Err("Invalid country flag key");
        }
        Ok(())
    }

    fn load(
        source: &Source,
        params: &Self::Params,
        context: &LoadContext,
        cancelled: &AtomicBool,
    ) -> io::Result<Self::Reply> {
        let content = context.content()?;
        let presentation = content.presentation.identity().clone();
        let presented = source.presented_for(content)?;
        let stack_key = presented.content.layers.art_stack_key();
        let shared = source.state::<FlagState>();
        let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
        let held = state
            .held()
            .find(|cached| cached.matches(&content.options, &presentation, stack_key));
        let context = match held {
            Some(held) => {
                state.make_current(&held);
                held
            }
            None => {
                let resolved = Arc::clone(&presented.content);
                let cells = state
                    .held()
                    .find(|cached| Arc::ptr_eq(&cached.content, &resolved))
                    .map(|cached| {
                        (
                            cached.cells,
                            cached.cell_fallbacks.clone(),
                            Arc::clone(&cached.files),
                        )
                    });
                drop(state);
                let (cells, cell_fallbacks, files) = match cells {
                    Some(cells) => cells,
                    None => {
                        let (cells, fallbacks) =
                            content_reader_layers::content::diagnostics::observe(|| {
                                flag_cells(&resolved.layers)
                            });
                        (cells?, fallbacks, Arc::default())
                    }
                };
                if cancelled.load(Ordering::Acquire) {
                    return Err(invalid("Flag request cancelled"));
                }
                let mut loaded = FlagContext::load(
                    source,
                    resolved,
                    cells,
                    content.options.clone(),
                    content.allow_unverified,
                )?;
                loaded.presentation = presented.presentation.clone();
                loaded.cell_fallbacks = cell_fallbacks;
                loaded.files = files;
                let loaded = Arc::new(loaded);
                state = shared.lock().unwrap_or_else(|error| error.into_inner());
                state.make_current(&loaded);
                loaded
            }
        };
        drop(state);
        context.cell_fallbacks.credit();
        let load_shield = || -> io::Result<Arc<ShieldStyle>> {
            let catalogue = dashboard_base::catalogue::catalogue(source, content)?;
            shield_for(source, &catalogue)
        };
        let shield = matches!(
            params.size,
            CountryFlagSize::Shield | CountryFlagSize::Panel
        )
        .then(|| load_shield().map_err(|error| error.to_string()));
        context.reply(
            params,
            shield
                .as_ref()
                .map(|style| style.as_ref().map(Arc::as_ref).map_err(String::as_str)),
            cancelled,
        )
    }
}

fn shield_for(source: &Source, catalogue: &Catalogue) -> io::Result<Arc<ShieldStyle>> {
    let state = source.state::<Option<CachedShield>>();
    cached_shield(&mut state.lock().unwrap_or_else(|error| error.into_inner()), catalogue)
}

fn cached_shield(
    slot: &mut Option<CachedShield>,
    catalogue: &Catalogue,
) -> io::Result<Arc<ShieldStyle>> {
    let decode = *catalogue.gui_decode.as_ref().map_err(|why| {
        invalid(format!(
            "the top bar's {PLAYER_FLAG} shield artwork is unavailable: {why}"
        ))
    })?;
    let current = slot
        .as_ref()
        .filter(|(owner, setting, _)| Arc::ptr_eq(owner, &catalogue.content) && *setting == decode)
        .map(|(_, _, style)| style.get().clone());
    let style = match current {
        Some(style) => style,
        None => {
            let drawn = dashboard_base::catalogue::Drawn::new(|| {
                ShieldStyle::load(catalogue)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            });
            let style = drawn.get().clone();
            *slot = Some((Arc::clone(&catalogue.content), decode, drawn));
            style
        }
    };
    style.map_err(invalid)
}

#[derive(Default)]
struct FlagState {
    current: Option<Arc<FlagContext>>,
    earlier: Vec<Weak<FlagContext>>,
}

impl FlagState {
    fn held(&self) -> impl Iterator<Item = Arc<FlagContext>> + '_ {
        self.current
            .iter()
            .cloned()
            .chain(self.earlier.iter().filter_map(Weak::upgrade))
    }

    fn make_current(&mut self, context: &Arc<FlagContext>) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, context))
        {
            return;
        }
        if let Some(earlier) = self.current.replace(Arc::clone(context)) {
            self.earlier.push(Arc::downgrade(&earlier));
        }
        self.earlier.retain(|earlier| {
            earlier.strong_count() > 0 && !std::ptr::eq(earlier.as_ptr(), Arc::as_ptr(context))
        });
    }
}

type CachedShield = (
    Arc<ResolvedContent>,
    content_reader_image::native::Decode,
    dashboard_base::catalogue::Drawn<Result<Arc<ShieldStyle>, String>>,
);

pub struct ShieldStyle {
    shield: dashboard_base::catalogue::MaskedShield,
    panel: dashboard_base::catalogue::MaskedShield,
    recorded: Option<(content_reader_layers::ResolvedLayers, [[u32; 2]; 3])>,
    recorded_files: Arc<FlagFiles>,
    decode: content_reader_image::native::Decode,
    pub warnings: Vec<String>,
}

impl ShieldStyle {
    pub fn load(catalogue: &Catalogue) -> io::Result<Self> {
        let decode = *catalogue.gui_decode.as_ref().map_err(|why| {
            invalid(format!(
                "the top bar's {PLAYER_FLAG} shield artwork is unavailable: {why}"
            ))
        })?;
        let layers = &catalogue.content.layers;
        let mut warnings = Vec::new();
        let (shield, recorded) = if !layers.presentation().shows_visual_mods() {
            (Self::shown(catalogue, decode, &mut warnings)?, None)
        } else {
            let (shown, noted) = dashboard_base::catalogue::capture_isolated(|| {
                content_reader_layers::content::strict(|| Self::shown(catalogue, decode, &mut warnings))
            });
            match shown {
                Ok(shield) => {
                    dashboard_base::catalogue::credit(&noted);
                    (shield, None)
                }
                Err(error) => {
                    let recorded = layers.recorded_only();
                    warnings.clear();
                    let guis = content_reader_gui::gui::native::read_layers(&recorded)?;
                    let name = content_reader_gui::gui::native::window_button_sprite(
                        &guis,
                        TOP_BAR,
                        PLAYER_FLAG,
                        &mut warnings,
                    )?;
                    let sprites = dashboard_base::catalogue::recorded_sprites(&recorded);
                    let sprites = (*sprites).as_ref().map_err(|why| invalid(why.clone()))?;
                    let sprite = dashboard_base::catalogue::created_gui_sprite(sprites, &name)?;
                    let shield = dashboard_base::catalogue::MaskedShield::load(&recorded, sprite, decode)?;
                    let cells = flag_cells(&recorded)?;
                    layers.note_overlay_refusal(format!("shield {PLAYER_FLAG} ({error})"));
                    (shield, Some((recorded, cells)))
                }
            }
        };
        let panel = match shield.overlay()? {
            Some(overlay) => shield.with_overlay(even_overlay(&overlay)),
            None => shield.clone(),
        };
        Ok(Self {
            shield,
            panel,
            recorded,
            recorded_files: Arc::default(),
            decode,
            warnings,
        })
    }

    fn shown(
        catalogue: &Catalogue,
        decode: content_reader_image::native::Decode,
        warnings: &mut Vec<String>,
    ) -> io::Result<dashboard_base::catalogue::MaskedShield> {
        let layers = &catalogue.content.layers;
        let name = content_reader_gui::gui::native::window_button_sprite(
            catalogue.guis()?,
            TOP_BAR,
            PLAYER_FLAG,
            warnings,
        )?;
        let sprite = dashboard_base::catalogue::created_gui_sprite(&catalogue.sprites, &name)?;
        let shield = dashboard_base::catalogue::MaskedShield::load(layers.presentation(), sprite, decode)?;
        if layers.presentation().shows_visual_mods() {
            let cells = flag_cells(layers)?;
            let cell = cells[if shield.small() { 2 } else { 0 }];
            let probe = Rgba8Image {
                width: cell[0],
                height: cell[1],
                pixels: vec![255; (cell[0] * cell[1] * 4) as usize],
            };
            shield.draw(layers.presentation(), &[&probe], decode)?;
        }
        Ok(shield)
    }

    fn flags_from<'a>(
        &'a self,
        layers: &'a content_reader_layers::ResolvedLayers,
        cells: &'a [[u32; 2]; 3],
        files: &'a Arc<FlagFiles>,
    ) -> (
        &'a content_reader_layers::ResolvedLayers,
        &'a [[u32; 2]; 3],
        &'a Arc<FlagFiles>,
    ) {
        match &self.recorded {
            Some((recorded, cells)) => (recorded, cells, &self.recorded_files),
            None => (layers, cells, files),
        }
    }

    pub(crate) fn sets(&self) -> &'static [FlagSet] {
        if self.shield.small() {
            &[FlagSet::Small]
        } else {
            &[FlagSet::Normal]
        }
    }

    fn draw(
        &self,
        layers: &content_reader_layers::ResolvedLayers,
        flags: &[&Rgba8Image],
        panel: bool,
    ) -> io::Result<Vec<Rgba8Image>> {
        let stack = match &self.recorded {
            Some((recorded, _)) => recorded,
            None => layers.presentation(),
        };
        let shield = if panel { &self.panel } else { &self.shield };
        shield.draw(stack, flags, self.decode)
    }
}

fn safe_flag_key(key: &str) -> bool {
    key.split('/').all(|part| {
        !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', '\0', ':'])
    })
}

#[derive(Debug)]
pub enum FlagSearch {
    Found {
        asset: String,
        image: Arc<Rgba8Image>,
        origin: dashboard_base::products::artwork::ArtOrigin,
    },
    Unresolvable,
    Refused(String),
    Absent(Searched),
}

pub fn flag_cells(layers: &content_reader_layers::ResolvedLayers) -> io::Result<[[u32; 2]; 3]> {
    let recorded = dashboard_base::catalogue::defines_for_layers(layers)?;
    let graphics = recorded.graphics_for(layers);
    layers.graphics_define("flag cells", &graphics, &recorded, |defines| {
        Ok::<_, io::Error>([
            FlagSet::Normal.cell(defines)?,
            FlagSet::Medium.cell(defines)?,
            FlagSet::Small.cell(defines)?,
        ])
    })
}

#[derive(Clone)]
enum FlagFile {
    Missing(Searched),
    Unresolvable(String),
    Refused(content_reader_gui::flags::DecodeFailure),
    Image {
        image: Arc<Rgba8Image>,
        origin: dashboard_base::products::artwork::ArtOrigin,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stack {
    Shown,
    Recorded,
}

#[derive(Clone, Default)]
struct Reported {
    refused: BTreeSet<(Stack, usize, String)>,
    substitutions: BTreeSet<String>,
    unresolvable: BTreeSet<String>,
}

#[derive(Default)]
pub struct FlagCache {
    files: Arc<FlagFiles>,
    reported: Reported,
}

impl FlagFiles {
    pub(crate) fn probing() -> Self {
        Self {
            probing: true,
            ..Self::default()
        }
    }
}

fn probed() -> Arc<Rgba8Image> {
    static PROBED: std::sync::OnceLock<Arc<Rgba8Image>> = std::sync::OnceLock::new();
    Arc::clone(PROBED.get_or_init(|| {
        Arc::new(Rgba8Image {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        })
    }))
}

impl FlagCache {
    pub(crate) fn over(files: Arc<FlagFiles>) -> Self {
        Self {
            files,
            reported: Reported::default(),
        }
    }
}

#[derive(Default)]
pub(crate) struct FlagFiles {
    files: Mutex<
        BTreeMap<
            (Stack, usize, String, [u32; 2]),
            (FlagFile, content_reader_layers::content::diagnostics::Diagnostics),
        >,
    >,
    probing: bool,
    recorded: OnceLock<(
        (
            content_reader_layers::ResolvedLayers,
            Result<[[u32; 2]; 3], String>,
        ),
        content_reader_layers::content::diagnostics::Diagnostics,
    )>,
}

fn draw_each(
    images: &[&Rgba8Image],
    draw: impl Fn(&[&Rgba8Image]) -> io::Result<Vec<Rgba8Image>>,
) -> io::Result<Vec<io::Result<Rgba8Image>>> {
    match draw(images) {
        Ok(drawn) => Ok(drawn.into_iter().map(Ok).collect()),
        Err(_) if images.len() > 1 => Ok(images
            .iter()
            .map(|image| {
                draw(&[*image])?
                    .pop()
                    .ok_or_else(|| invalid("The shield drew no flag"))
            })
            .collect()),
        Err(error) if images.is_empty() => Err(error),
        Err(error) => Ok(vec![Err(error)]),
    }
}

fn flag_file(
    files: &FlagFiles,
    stack: Stack,
    layers: &content_reader_layers::ResolvedLayers,
    set_index: usize,
    asset: &str,
    cell: [u32; 2],
) -> io::Result<FlagFile> {
    let key = (stack, set_index, asset.to_owned(), cell);
    if let Some((file, noted)) = lock(&files.files).get(&key) {
        noted.credit();
        return Ok(file.clone());
    }
    let (file, noted) = content_reader_layers::content::diagnostics::observe(|| {
        io::Result::Ok(match art_file(layers, asset.as_bytes())? {
            ArtFile::Unaddressable => FlagFile::Unresolvable(format!("flag {asset} is not a path inside the content")),
            ArtFile::Unmounted { layer, reason } => {
                let origin =
                    dashboard_base::products::artwork::ArtOrigin::from_layer(&layers.layers[layer]);
                FlagFile::Unresolvable(format!(
                    "{} is not mounted ({reason}); flag {asset} cannot be resolved without risking a substitute",
                    origin.display_name()
                ))
            }
            ArtFile::Missing(searched) => FlagFile::Missing(searched),
            ArtFile::Mounted { layer, bytes, .. } => {
                let origin =
                    dashboard_base::products::artwork::ArtOrigin::from_layer(&layers.layers[layer]);
                match content_reader_gui::flags::decode_checked(&bytes, cell) {
                    Ok(_) if files.probing => FlagFile::Image {
                        image: probed(),
                        origin,
                    },
                    Ok(image) => FlagFile::Image {
                        image: Arc::new(image),
                        origin,
                    },
                    Err(reason) => FlagFile::Refused(reason),
                }
            }
        })
    });
    let file = file?;
    lock(&files.files).insert(key, (file.clone(), noted));
    Ok(file)
}

fn unreadable(asset: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("flag file {asset} could not be read: {error}"),
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub fn find_flag(
    layers: &content_reader_layers::ResolvedLayers,
    sets: &[FlagSet],
    cells: &[[u32; 2]; 3],
    candidates: &[String],
    cache: &mut FlagCache,
) -> io::Result<(FlagSearch, Vec<dashboard_core::findings::Finding>)> {
    let FlagCache { files, reported } = cache;
    let files = &**files;
    let presentation = layers.presentation();
    let plain = |files: &FlagFiles, reported: &mut Reported| {
        let Ok(found) = ladder(
            Stack::Shown,
            presentation,
            None,
            sets,
            cells,
            candidates,
            files,
            reported,
        )?
        else {
            unreachable!("only the visual mods' search breaks");
        };
        io::Result::Ok(found)
    };
    if !presentation.shows_visual_mods() {
        return plain(files, reported);
    }
    let ((recorded, recorded_cells), noted) = files.recorded.get_or_init(|| {
        content_reader_layers::content::diagnostics::observe(|| {
            let recorded = layers.recorded_only();
            let cells = flag_cells(&recorded).map_err(|error| error.to_string());
            (recorded, cells)
        })
    });
    noted.credit();
    let Ok(recorded_cells) = recorded_cells.as_ref() else {
        return plain(files, reported);
    };
    let mut shown = reported.clone();
    match ladder(
        Stack::Shown,
        presentation,
        Some((recorded, recorded_cells)),
        sets,
        cells,
        candidates,
        files,
        &mut shown,
    )? {
        Ok(found) => {
            *reported = shown;
            Ok(found)
        }
        Err(Broken { asset, why }) => {
            let Ok(found) = ladder(
                Stack::Recorded,
                recorded,
                None,
                sets,
                recorded_cells,
                candidates,
                files,
                reported,
            )?
            else {
                unreachable!("only the visual mods' search breaks");
            };
            if matches!(found.0, FlagSearch::Found { .. }) {
                layers.note_overlay_refusal(format!("flag {asset} ({why})"));
            }
            Ok(found)
        }
    }
}

struct Broken {
    asset: String,
    why: String,
}

fn ladder(
    stack: Stack,
    layers: &content_reader_layers::ResolvedLayers,
    recorded: Option<(&content_reader_layers::ResolvedLayers, &[[u32; 2]; 3])>,
    sets: &[FlagSet],
    cells: &[[u32; 2]; 3],
    candidates: &[String],
    files: &FlagFiles,
    reported: &mut Reported,
) -> io::Result<Result<(FlagSearch, Vec<dashboard_core::findings::Finding>), Broken>> {
    use dashboard_core::findings::{Finding, Kind};
    let mut findings = Vec::new();
    let mut searched = None;
    let mut decoder_refused = None;
    for &set in sets {
        let set_index = match set {
            FlagSet::Normal => 0,
            FlagSet::Medium => 1,
            FlagSet::Small => 2,
        };
        let cell = cells[set_index];
        for key in candidates {
            if !safe_flag_key(key) {
                if reported.unresolvable.insert(key.clone()) {
                    findings.push(Finding::new(Kind::Refused, key,
                        format!("flag name {key:?} cannot be resolved inside the flag folder, so the country's flag is unavailable")));
                }
                return Ok(Ok((FlagSearch::Unresolvable, findings)));
            }
            let asset = format!("{}{key}.tga", set.directory());
            let read = flag_file(files, stack, layers, set_index, &asset, cell);
            if let Some((recorded, recorded_cells)) = recorded
                && !matches!(read, Ok(FlagFile::Image { .. }))
            {
                let breaks = match &read {
                    Ok(FlagFile::Refused(_)) if layers.supplied_by_visual_mod(&asset) => true,
                    Err(_) => true,
                    _ => matches!(
                        flag_file(
                            files,
                            Stack::Recorded,
                            recorded,
                            set_index,
                            &asset,
                            recorded_cells[set_index],
                        ),
                        Ok(FlagFile::Image { .. })
                    ),
                };
                if breaks {
                    let why = match &read {
                        Ok(FlagFile::Refused(reason)) => format!("{reason:?}"),
                        Err(error) => error.to_string(),
                        _ => "the visual mods give no such file".to_owned(),
                    };
                    return Ok(Err(Broken { asset, why }));
                }
            }
            match read.map_err(|error| unreadable(&asset, error))? {
                FlagFile::Missing(miss) => searched = Some(miss),
                FlagFile::Unresolvable(why) => {
                    let mut finding = Finding::new(Kind::Refused, key, why);
                    finding.file = Some(asset);
                    findings.push(finding);
                    return Ok(Ok((FlagSearch::Unresolvable, findings)));
                }
                FlagFile::Refused(reason) => {
                    let message = match reason {
                        content_reader_gui::flags::DecodeFailure::EngineRefused => format!(
                            "flag decoder refused {asset} at cell {}x{}; this file was skipped",
                            cell[0], cell[1]
                        ),
                        content_reader_gui::flags::DecodeFailure::Truncated => format!(
                            "flag decoder cannot safely read {asset} at cell {}x{}; this reader refuses this input but the game does not check read status",
                            cell[0], cell[1]
                        ),
                    };
                    decoder_refused.get_or_insert_with(|| message.clone());
                    if !reported.refused.insert((stack, set_index, asset.clone())) {
                        continue;
                    }
                    let mut finding = Finding::new(
                        match reason {
                            content_reader_gui::flags::DecodeFailure::EngineRefused => Kind::Content,
                            content_reader_gui::flags::DecodeFailure::Truncated => Kind::Refused,
                        },
                        key,
                        message,
                    );
                    finding.file = Some(asset.clone());
                    findings.push(finding);
                }
                FlagFile::Image { image, origin } => {
                    if sets.first() == Some(&FlagSet::Medium)
                        && set == FlagSet::Normal
                        && reported.substitutions.insert(asset.clone())
                    {
                        let mut finding = Finding::new(
                            Kind::Fallback,
                            key,
                            format!(
                                "medium flag is missing or undecodable; normal flag {asset} is shown"
                            ),
                        );
                        finding.file = Some(asset.clone());
                        findings.push(finding);
                    }
                    return Ok(Ok((FlagSearch::Found { asset, image, origin }, findings)));
                }
            }
        }
    }
    findings.push(Finding::new(
        Kind::Fallback,
        candidates.first().cloned().unwrap_or_default(),
        "no decodable flag in the requested sets; flag artwork is unavailable",
    ));
    let search = match (decoder_refused, searched) {
        (Some(refused), _) => FlagSearch::Refused(refused),
        (None, Some(searched)) => FlagSearch::Absent(searched),
        (None, None) => FlagSearch::Unresolvable,
    };
    Ok(Ok((search, findings)))
}

pub(crate) fn flag_key(asset: &str, pixels: &[u8]) -> String {
    format!("{asset}#{}", blake3::hash(pixels).to_hex())
}

fn bounded(width: u32, height: u32, edge: u32) -> (u32, u32) {
    let scale = (width.max(height) as f64 / edge as f64).max(1.0);
    (
        (width as f64 / scale).round().max(1.0) as u32,
        (height as f64 / scale).round().max(1.0) as u32,
    )
}

fn sample(image: &Rgba8Image, x: u32, y: u32, width: u32, height: u32) -> [u8; 4] {
    let at = (((y * image.height / height) * image.width + x * image.width / width) * 4) as usize;
    image.pixels[at..at + 4].try_into().unwrap()
}

fn resize(image: &Rgba8Image, width: u32, height: u32) -> Rgba8Image {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&sample(image, x, y, width, height));
        }
    }
    Rgba8Image {
        width,
        height,
        pixels,
    }
}

const EVEN_WINDOW_DIVISOR: u32 = 4;

fn even_overlay(overlay: &Rgba8Image) -> Rgba8Image {
    let (width, height) = (overlay.width as usize, overlay.height as usize);
    let radius = (overlay.width.min(overlay.height) / EVEN_WINDOW_DIVISOR).max(1) as usize;
    let mut sums = vec![0_u64; (width + 1) * (height + 1)];
    for y in 0..height {
        for x in 0..width {
            sums[(y + 1) * (width + 1) + x + 1] =
                u64::from(overlay.pixels[(y * width + x) * 4 + 3])
                    + sums[y * (width + 1) + x + 1]
                    + sums[(y + 1) * (width + 1) + x]
                    - sums[y * (width + 1) + x];
        }
    }
    let mean = |x: usize, y: usize| {
        let (left, top) = (x.saturating_sub(radius), y.saturating_sub(radius));
        let (right, bottom) = ((x + radius + 1).min(width), (y + radius + 1).min(height));
        let total = sums[bottom * (width + 1) + right] + sums[top * (width + 1) + left]
            - sums[top * (width + 1) + right]
            - sums[bottom * (width + 1) + left];
        total as f64 / ((right - left) * (bottom - top)) as f64
    };
    let centre = mean(width / 2, height / 2);
    let mut pixels = overlay.pixels.clone();
    for y in 0..height {
        for x in 0..width {
            let local = mean(x, y);
            if local > 0.0 {
                let alpha = &mut pixels[(y * width + x) * 4 + 3];
                *alpha = (f64::from(*alpha) * centre / local)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
    Rgba8Image {
        width: overlay.width,
        height: overlay.height,
        pixels,
    }
}

pub struct FlagFolders {
    layers: content_reader_layers::ResolvedLayers,
    cells: [[u32; 2]; 3],
    files: Arc<FlagFiles>,
    chosen: Mutex<HashMap<Chain, Arc<OnceLock<Option<CountryFlagChoice>>>>>,
}

impl FlagFolders {
    pub fn read(content: &ResolvedContent) -> io::Result<Self> {
        let layers = content.layers.recorded_only();
        let cells = flag_cells(&layers)?;
        Ok(Self {
            layers,
            cells,
            files: Arc::new(FlagFiles::probing()),
            chosen: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn of(layers: content_reader_layers::ResolvedLayers, cells: [[u32; 2]; 3]) -> Self {
        Self {
            layers,
            cells,
            files: Arc::new(FlagFiles::probing()),
            chosen: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, chain: &Chain) -> io::Result<(Arc<OnceLock<Option<CountryFlagChoice>>>, bool)> {
        let mut chosen = lock(&self.chosen);
        if let Some(slot) = chosen.get(chain) {
            return Ok((Arc::clone(slot), false));
        }
        let slot = Arc::new(OnceLock::new());
        engine_allocation::reserve_map(&mut *chosen, 1)?;
        chosen.insert(chain.clone(), Arc::clone(&slot));
        Ok((slot, true))
    }

    pub fn warm(&self, chain: &Chain) -> io::Result<()> {
        let (slot, claimed) = self.slot(chain)?;
        if claimed {
            slot.get_or_init(|| self.search(chain).ok());
        }
        Ok(())
    }

    pub fn warm_all(&self, chains: &[Chain]) {
        if chains.len() <= 1 {
            for chain in chains {
                let _ = self.warm(chain);
            }
            return;
        }
        let workers = std::thread::available_parallelism().map_or(4, usize::from).min(8).min(chains.len());
        let chunk = chains.len().div_ceil(workers);
        std::thread::scope(|scope| {
            for part in chains.chunks(chunk) {
                scope.spawn(move || {
                    for chain in part {
                        let _ = self.warm(chain);
                    }
                });
            }
        });
    }

    pub fn choose(&self, chain: &Chain) -> io::Result<CountryFlagChoice> {
        let (slot, _) = self.slot(chain)?;
        match slot.get_or_init(|| self.search(chain).ok()) {
            Some(choice) => Ok(choice.clone()),
            None => self.search(chain),
        }
    }

    fn search(&self, chain: &Chain) -> io::Result<CountryFlagChoice> {
        let mut choice = CountryFlagChoice::default();
        for set in [FlagSet::Normal, FlagSet::Medium, FlagSet::Small] {
            let at = atlas(set);
            let (found, findings) = find_flag(
                &self.layers,
                &[set],
                &self.cells,
                &chain.candidates,
                &mut FlagCache::over(Arc::clone(&self.files)),
            )
            .map_err(|error| match error.kind() {
                io::ErrorKind::InvalidData => io::Error::other(error.to_string()),
                _ => error,
            })?;
            let refused: Vec<String> = findings
                .into_iter()
                .filter(|finding| finding.kind == dashboard_core::findings::Kind::Refused)
                .map(|finding| finding.message)
                .collect();
            let refused = (!refused.is_empty()).then(|| refused.join("; "));
            match found {
                FlagSearch::Found { asset, .. } => {
                    choice.keys[at] = asset
                        .strip_prefix(set.directory())
                        .and_then(|key| key.strip_suffix(".tga"))
                        .map(str::to_owned);
                    choice.reasons[at] = refused;
                }
                FlagSearch::Unresolvable => {
                    choice.reasons[at] = Some(
                        refused
                            .or_else(|| chain.refusal.clone())
                            .unwrap_or_else(|| UNAVAILABLE.into()),
                    );
                }
                FlagSearch::Refused(decoder) => {
                    choice.reasons[at] = Some(match refused {
                        Some(named) if named.contains(&decoder) => named,
                        Some(named) => format!("{named}; {decoder}"),
                        None => decoder,
                    });
                }
                FlagSearch::Absent(searched) => {
                    if chain.refusal.is_none() && refused.is_none() {
                        choice.searched[at] = Some(searched);
                    }
                    choice.reasons[at] = Some(
                        chain
                            .refusal
                            .clone()
                            .or(refused)
                            .unwrap_or_else(|| UNAVAILABLE.into()),
                    );
                }
            }
        }
        Ok(choice)
    }
}

pub struct FlagContext {
    options: ResolveOptions,
    presentation: dashboard_base::presentation::Presentation,
    stack_key: [u8; 32],
    language: String,
    content: Arc<ResolvedContent>,
    cells: [[u32; 2]; 3],
    cell_fallbacks: content_reader_layers::content::diagnostics::Diagnostics,
    files: Arc<FlagFiles>,
    warnings: Vec<String>,
}

impl FlagContext {
    fn matches(
        &self,
        options: &ResolveOptions,
        presentation: &dashboard_base::presentation::Presentation,
        stack_key: [u8; 32],
    ) -> bool {
        self.stack_key == stack_key
            && self.presentation == *presentation
            && self.options.game_dir == options.game_dir
            && self.options.user_dir == options.user_dir
            && self.options.cache_dir == options.cache_dir
    }

    fn load(
        source: &Source,
        content: Arc<ResolvedContent>,
        cells: [[u32; 2]; 3],
        options: ResolveOptions,
        allow_unverified: bool,
    ) -> io::Result<Self> {
        let mut warnings = vec![dashboard_base::products::artwork::CURRENT_INSTALL_ART_NOTICE.to_owned()];
        content::check_replay(source.replay_ref(), allow_unverified, &mut warnings)?;
        let stack_key = content.layers.art_stack_key();
        let language = content::recorded_language(source.replay_ref());
        Ok(Self {
            options,
            presentation: Default::default(),
            stack_key,
            language,
            content,
            cells,
            cell_fallbacks: Default::default(),
            files: Arc::default(),
            warnings,
        })
    }

    fn reply(
        &self,
        request: &CountryFlagParams,
        shield_style: Option<Result<&ShieldStyle, &str>>,
        cancelled: &AtomicBool,
    ) -> io::Result<CountryFlagReply> {
        let mut flags = Vec::new();
        let mut unavailable = Vec::new();
        let mut warnings = self.warnings.clone();
        let shield_style = match shield_style {
            Some(Ok(style)) => Some(style),
            Some(Err(reason)) => {
                warnings.push(format!("Shield artwork unavailable: {reason}"));
                return Ok(CountryFlagReply {
                    size: request.size,
                    flags,
                    unavailable: (0..request.flags.len() as u32)
                        .map(|flag| unavailable_flag(flag, dashboard_base::products::artwork::ArtResolution::Refused {
                            reason: format!("Shield artwork unavailable: {reason}"),
                            construct: "flag-shield-style".to_owned(),
                            origin: None,
                            file: None,
                        }))
                        .collect(),
                    warnings,
                });
            }
            None => None,
        };
        let (flag_layers, cells, files) = shield_style
            .map_or((&self.content.layers, &self.cells, &self.files), |style| {
                style.flags_from(&self.content.layers, &self.cells, &self.files)
            });
        if let Some(style) = shield_style {
            warnings.extend(style.warnings.iter().cloned());
        }
        let sets = shield_style.map_or_else(|| request.size.sets(), ShieldStyle::sets);
        let mut shielded = Vec::new();
        let mut reported_substitutions = BTreeSet::new();
        for (flag, choice) in (0_u32..).zip(&request.flags) {
            if cancelled.load(Ordering::Acquire) {
                return Err(invalid("Flag request cancelled"));
            }
            let mut found = None;
            let mut reasons = Vec::new();
            let mut refused = false;
            let mut searched = Some(None::<Searched>);
            for &set in sets {
                let at = atlas(set);
                let Some(key) = &choice.keys[at] else {
                    reasons.extend(choice.reasons[at].clone());
                    searched = searched.zip(choice.searched[at].clone()).map(|(_, miss)| Some(miss));
                    continue;
                };
                let (search, findings) = find_flag(
                    flag_layers,
                    &[set],
                    cells,
                    std::slice::from_ref(key),
                    &mut FlagCache::over(Arc::clone(files)),
                )?;
                for finding in findings {
                    if finding.kind == dashboard_core::findings::Kind::Refused {
                        warnings.push(format!("{key}: {}", finding.message));
                        reasons.push(finding.message);
                        refused = true;
                    }
                }
                match search {
                    FlagSearch::Found { asset, image, origin } => {
                        if sets.first() == Some(&FlagSet::Medium)
                            && set == FlagSet::Normal
                            && reported_substitutions.insert(asset.clone())
                        {
                            warnings.push(format!(
                                "{key}: medium flag is missing or undecodable; normal flag {asset} is shown"
                            ));
                        }
                        found = Some((asset, image, origin));
                        break;
                    }
                    FlagSearch::Absent(miss) => searched = searched.map(|_| Some(miss)),
                    FlagSearch::Refused(decoder) => {
                        if !reasons.contains(&decoder) {
                            reasons.push(decoder);
                        }
                        refused = true;
                        searched = None;
                    }
                    FlagSearch::Unresolvable => searched = None,
                }
            }
            match found {
                Some((asset, image, origin)) => shielded.push((flag, asset, image, origin)),
                None => {
                    let reason = if reasons.is_empty() { UNAVAILABLE.into() } else { reasons.join("; ") };
                    unavailable.push(unavailable_flag(flag, match (refused, searched.flatten()) {
                        (false, Some(searched)) => ArtResolution::confirmed_missing(searched, reason, None, None),
                        (true, _) | (false, None) => ArtResolution::Refused { reason, construct: "flag-file".to_owned(), origin: None, file: None },
                    }));
                }
            }
        }
        let drawn: Vec<io::Result<Rgba8Image>> = match shield_style {
            Some(style) => {
                let panel = request.size == CountryFlagSize::Panel;
                let images: Vec<_> = shielded
                    .iter()
                    .map(|(_, _, image, _)| image.as_ref())
                    .collect();
                match draw_each(&images, |batch| {
                    style.draw(&self.content.layers, batch, panel)
                }) {
                    Ok(drawn) => drawn,
                    Err(error) => {
                        warnings.push(format!("Shield artwork unavailable: {error}"));
                        shielded.iter().map(|_| Err(io::Error::other(error.to_string()))).collect()
                    }
                }
            }
            None => shielded
                .iter()
                .map(|(_, _, image, _)| Ok(image.as_ref().clone()))
                .collect(),
        };
        let scope_key = dashboard_base::products::artwork::art_scope_key(flag_layers, &self.language);
        for ((flag, asset, raw, origin), drawn) in shielded.into_iter().zip(drawn) {
            let image = match drawn {
                Ok(image) => image,
                Err(error) => {
                    let reason = format!("Shield artwork unavailable: {error}");
                    if !warnings.contains(&reason) {
                        warnings.push(reason.clone());
                    }
                    unavailable.push(unavailable_flag(flag, dashboard_base::products::artwork::ArtResolution::Failed { reason, retryable: false }));
                    continue;
                }
            };
            let (width, height) = bounded(image.width, image.height, MAX_FLAG_EDGE);
            let image = resize(&image, width, height);
            let key = flag_key(&asset, &image.pixels);
            match dashboard_text::artwork::png_url(&image) {
                Ok(png) => {
                    let asset_id = png
                        .strip_prefix(dashboard_text::artwork::base())
                        .unwrap_or(&png)
                        .to_owned();
                    let provenance =
                        dashboard_base::products::artwork::ArtProvenance::current_install(
                            &origin,
                            &scope_key,
                        );
                    let resolution = dashboard_base::products::artwork::ArtResolution::Ready {
                        asset_id,
                        url: png.clone(),
                        origin,
                        file: asset.clone(),
                        frame: dashboard_base::products::artwork::ArtFrame {
                            frame_index: 0,
                            frame_count: 1,
                            frame_width: raw.width,
                            frame_height: raw.height,
                            texture_width: raw.width,
                            texture_height: raw.height,
                            output_width: image.width,
                            output_height: image.height,
                        },
                        provenance,
                    };
                    flags.push(CountryFlagImage {
                        flag,
                        key,
                        asset,
                        width: image.width,
                        height: image.height,
                        png,
                        resolution: Some(resolution),
                    });
                }
                Err(error) => unavailable.push(unavailable_flag(flag, dashboard_base::products::artwork::ArtResolution::Failed {
                    reason: format!("flag artwork could not be encoded: {error}"),
                    retryable: false,
                })),
            }
        }
        Ok(CountryFlagReply {
            size: request.size,
            flags,
            unavailable,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(image: &CountryFlagImage) -> Vec<u8> {
        let bytes = dashboard_text::artwork::url_bytes(&image.png).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(&*bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut pixels).unwrap();
        assert_eq!(pixels.len(), (image.width * image.height * 4) as usize);
        pixels
    }

    fn context(
        content: &Arc<ResolvedContent>,
        cells: [[u32; 2]; 3],
        warnings: Vec<String>,
    ) -> FlagContext {
        FlagContext {
            presentation: Default::default(),
            stack_key: content.layers.art_stack_key(),
            language: "english".into(),
            options: ResolveOptions {
                game_dir: "G".into(),
                user_dir: "U".into(),
                cache_dir: "C".into(),
            },
            content: Arc::clone(content),
            cells,
            cell_fallbacks: Default::default(),
            files: Arc::default(),
            warnings,
        }
    }

    fn every(key: &str) -> CountryFlagChoice {
        CountryFlagChoice {
            keys: [0; 3].map(|_| Some(key.to_owned())),
            ..CountryFlagChoice::default()
        }
    }

    fn chain(candidates: &[&str]) -> Chain {
        Chain {
            candidates: candidates.iter().map(|&key| key.to_owned()).collect(),
            refusal: None,
        }
    }

    #[test]
    fn each_atlas_chooses_its_own_key_and_names_what_it_refused() {
        let image = vec![
            0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 0x28, 30, 20, 10, 255,
        ];
        let mut skipped = image.clone();
        skipped[2] = 10;
        let mut truncated = image.clone();
        truncated.pop();
        let (catalogue, _folders) = dashboard_base::catalogue::texture_tests::texture_catalogue(
            &[],
            &[
                ("gfx/flags/mod_normal.tga", image.clone()),
                ("gfx/flags/medium/mod_good.tga", image),
                ("gfx/flags/medium/mod_skipped.tga", skipped),
                ("gfx/flags/medium/mod_truncated.tga", truncated),
            ],
        );
        let folders = FlagFolders::of(catalogue.content.layers.clone(), [[1, 1]; 3]);
        for (candidates, medium, refused, asset, reason) in [
            (
                vec!["mod_normal"],
                None,
                Some(UNAVAILABLE),
                Some("gfx/flags/mod_normal.tga"),
                None,
            ),
            (
                vec!["mod_skipped", "mod_good"],
                Some("mod_good"),
                None,
                Some("gfx/flags/medium/mod_good.tga"),
                None,
            ),
            (
                vec!["mod_truncated", "mod_good"],
                Some("mod_good"),
                Some("game does not check read status"),
                Some("gfx/flags/medium/mod_good.tga"),
                None,
            ),
            (
                vec!["mod_truncated"],
                None,
                Some("game does not check read status"),
                None,
                Some("game does not check read status"),
            ),
            (
                vec!["mod_absent"],
                None,
                Some(UNAVAILABLE),
                None,
                Some(UNAVAILABLE),
            ),
        ] {
            let choice = folders.choose(&chain(&candidates)).unwrap();
            assert_eq!(choice.keys[1].as_deref(), medium, "{candidates:?}");
            match refused {
                Some(refused) => assert!(
                    choice.reasons[1]
                        .as_deref()
                        .is_some_and(|reason| reason.contains(refused)),
                    "{candidates:?}: {:?}",
                    choice.reasons
                ),
                None => assert_eq!(choice.reasons[1], None, "{candidates:?}"),
            }
            let request = CountryFlagParams {
                flags: vec![choice],
                size: CountryFlagSize::Medium,
            };
            let reply = context(&catalogue.content, [[1, 1]; 3], Vec::new())
                .reply(&request, None, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(reply.flags.first().map(|flag| flag.asset.as_str()), asset);
            if asset == Some("gfx/flags/mod_normal.tga") {
                assert!(
                    reply.warnings.iter().any(|w| w.contains("medium flag is missing or undecodable") && w.contains("gfx/flags/mod_normal.tga")),
                    "{:?}",
                    reply.warnings
                );
            } else {
                assert!(reply.warnings.is_empty(), "{:?}", reply.warnings);
            }
            match reason {
                Some(reason) => {
                    assert_eq!(reply.unavailable.len(), 1);
                    assert_eq!(reply.unavailable[0].flag, 0);
                    assert!(
                        reply.unavailable[0].reason.contains(reason),
                        "{:?}",
                        reply.unavailable
                    );

                }
                None => assert!(reply.unavailable.is_empty(), "{:?}", reply.unavailable),
            }
        }
    }

    #[test]
    fn a_flag_file_the_decoder_refused_is_a_refusal_never_a_confirmed_miss() {
        let image = vec![
            0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 0x28, 30, 20, 10, 255,
        ];
        let mut skipped = image;
        skipped[2] = 10;
        let (_, folder) = dashboard_base::catalogue::tests::localisation_catalogue(&[]);
        let file = folder.0.join("gfx/flags/medium/mod_skipped.tga");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, skipped).unwrap();
        let layers = content_reader_layers::ResolvedLayers::from_folders(&[(&folder.0, &[][..])]).unwrap();
        let content = Arc::new(ResolvedContent { layers: layers.clone(), stats: Default::default(), changed: Vec::new(), notes: Vec::new() });
        let folders = FlagFolders::of(layers, [[1, 1]; 3]);
        for (candidates, searched_every_layer) in [(vec!["mod_skipped", "mod_absent"], false), (vec!["mod_skipped"], false), (vec!["mod_absent"], true)] {
            let choice = folders.choose(&chain(&candidates)).unwrap();
            assert_eq!(choice.keys[1], None, "{candidates:?}");
            assert_eq!(choice.searched[1].is_some(), searched_every_layer, "{candidates:?}: a file found and refused is not a miss");
            assert_eq!(
                choice.reasons[1].as_deref().is_some_and(|reason| reason.contains("flag decoder refused gfx/flags/medium/mod_skipped.tga")),
                !searched_every_layer,
                "{candidates:?}: {:?}",
                choice.reasons
            );
            let request = CountryFlagParams { flags: vec![choice], size: CountryFlagSize::Medium };
            let reply = context(&content, [[1, 1]; 3], Vec::new()).reply(&request, None, &AtomicBool::new(false)).unwrap();
            let [unavailable] = &reply.unavailable[..] else { panic!("{candidates:?}: {:?}", reply.unavailable) };
            match (&unavailable.resolution, searched_every_layer) {
                (ArtResolution::ConfirmedMissing { .. }, true) => {}
                (ArtResolution::Refused { reason, .. }, false) => assert!(reason.contains("flag decoder refused"), "{reason}"),
                (other, _) => panic!("{candidates:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_mod_without_a_matching_flag_chooses_nothing_and_names_why() {
        let image = vec![
            0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 0x28, 30, 20, 10, 255,
        ];
        let (catalogue, _folders) = dashboard_base::catalogue::texture_tests::texture_catalogue(
            &[],
            &[
                ("gfx/flags/GER.tga", image.clone()),
                ("gfx/flags/small/GER.tga", image),
            ],
        );
        let folders = FlagFolders::of(catalogue.content.layers.clone(), [[1, 1]; 3]);
        let choice = folders
            .choose(&chain(&[
                "MOD_COS_fascism",
                "MOD_COS",
                "MOD_fascism",
                "MOD",
            ]))
            .unwrap();
        assert_eq!(choice.keys, [None, None, None]);
        assert_eq!(choice.reasons, [0; 3].map(|_| Some(UNAVAILABLE.to_owned())));
        let refused = Chain {
            candidates: Vec::new(),
            refusal: Some("country 7 flag needs its Politics row".into()),
        };
        let choice = folders.choose(&refused).unwrap();
        assert_eq!(choice.keys, [None, None, None]);
        assert_eq!(
            choice.reasons,
            [0; 3].map(|_| Some("country 7 flag needs its Politics row".to_owned()))
        );
        let tail = Chain {
            candidates: vec!["GER".into()],
            refusal: Some("original tag of country appearance capture refused".into()),
        };
        let choice = folders.choose(&tail).unwrap();
        assert_eq!(choice.keys, [Some("GER".into()), None, Some("GER".into())]);
        assert_eq!(
            choice.reasons[1].as_deref(),
            Some("original tag of country appearance capture refused")
        );
    }

    #[test]
    fn each_choices_refusal_is_its_own_whatever_else_is_drawn() {
        let mut truncated = vec![
            0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 0x28, 30, 20, 10, 255,
        ];
        truncated.pop();
        let (catalogue, _folders) = dashboard_base::catalogue::texture_tests::texture_catalogue(
            &[],
            &[("gfx/flags/medium/mod_truncated.tga", truncated)],
        );
        let folders = FlagFolders::of(catalogue.content.layers.clone(), [[1, 1]; 3]);
        let choices: Vec<CountryFlagChoice> =
            ["mod_truncated", "mod_truncated", "../escape", "../escape"]
                .iter()
                .map(|&candidate| folders.choose(&chain(&[candidate])).unwrap())
                .collect();
        let context = context(&catalogue.content, [[1, 1]; 3], Vec::new());
        let reply = |flags: &[CountryFlagChoice]| {
            let request = CountryFlagParams {
                flags: flags.to_vec(),
                size: CountryFlagSize::Medium,
            };
            context
                .reply(&request, None, &AtomicBool::new(false))
                .unwrap()
        };
        let together = reply(&choices);
        for (at, choice) in choices.iter().enumerate() {
            let alone = reply(std::slice::from_ref(choice));
            let reason = alone.unavailable[0].reason.clone();
            assert!(
                reason.contains("read status") || reason.contains("cannot be resolved"),
                "{at}: {reason}"
            );
            assert_eq!(together.unavailable[at].flag, at as u32);
            assert_eq!(together.unavailable[at].reason, reason, "{at}");
        }
    }

    #[test]
    fn unresolvable_flag_keys_report_once_across_sets_and_groups() {
        let (catalogue, _folder) = dashboard_base::catalogue::tests::folder_catalogue(&[]);
        let mut cache = FlagCache::default();
        for (index, set) in [FlagSet::Normal, FlagSet::Medium, FlagSet::Small]
            .into_iter()
            .enumerate()
        {
            let (found, findings) = find_flag(
                &catalogue.content.layers,
                &[set],
                &[[1, 1]; 3],
                &["../mod_country".into(), "safe_fallback".into()],
                &mut cache,
            )
            .unwrap();
            assert!(matches!(found, FlagSearch::Unresolvable));
            assert_eq!(findings.len(), usize::from(index == 0));
        }
    }

    #[test]
    fn a_refused_shield_batch_is_drawn_flag_by_flag() {
        let image = |value: u8| Rgba8Image {
            width: 1,
            height: 1,
            pixels: vec![value; 4],
        };
        let images = [image(1), image(2), image(3)];
        let batch: Vec<&Rgba8Image> = images.iter().collect();
        let draw = |batch: &[&Rgba8Image]| -> io::Result<Vec<Rgba8Image>> {
            match batch {
                [] => Err(io::Error::other("no shield program")),
                [image] if image.pixels[0] == 2 => Err(io::Error::other("refused")),
                [image] => Ok(vec![(*image).clone()]),
                _ => Err(io::Error::other("readback past the device's limit")),
            }
        };
        let told = |drawn: Vec<io::Result<Rgba8Image>>| {
            drawn
                .into_iter()
                .map(|drawn| drawn.map(|image| image.pixels[0]).map_err(|error| error.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            told(draw_each(&batch, &draw).unwrap()),
            [Ok(1), Err("refused".to_owned()), Ok(3)]
        );
        assert_eq!(
            told(draw_each(&batch[1..2], &draw).unwrap()),
            [Err("refused".to_owned())]
        );
        assert_eq!(
            draw_each(&[], &draw).unwrap_err().to_string(),
            "no shield program"
        );
    }

    #[test]
    fn unknown_decode_settings_refuse_the_shield_by_name() {
        let (mut catalogue, _folder) = dashboard_base::catalogue::tests::folder_catalogue(&[]);
        catalogue.gui_decode = Err("settings.txt is unavailable".into());
        let why = ShieldStyle::load(&catalogue).err().unwrap().to_string();
        assert!(why.contains("player_flag shield artwork"), "{why}");
        assert!(why.contains("settings.txt"), "{why}");
        assert_shield_warning(&catalogue, &why);
    }

    fn assert_shield_warning(catalogue: &dashboard_base::catalogue::Catalogue, reason: &str) {
        let context = context(
            &catalogue.content,
            [[1, 1]; 3],
            vec!["standing warning".into()],
        );
        let request = CountryFlagParams {
            flags: vec![every("MOD tag")],
            size: CountryFlagSize::Shield,
        };
        let reply = context
            .reply(&request, Some(Err(reason)), &AtomicBool::new(false))
            .unwrap();
        assert!(reply.flags.is_empty());
        assert_eq!(reply.unavailable.len(), 1);
        assert_eq!(reply.unavailable[0].flag, 0);
        assert_eq!(
            reply.unavailable[0].reason,
            format!("Shield artwork unavailable: {reason}")
        );
        assert!(
            matches!(reply.unavailable[0].resolution, dashboard_base::products::artwork::ArtResolution::Refused { .. }),
            "an unsupported shield is refused, never confirmed missing: {:?}",
            reply.unavailable[0].resolution
        );
        assert_eq!(
            reply.warnings,
            [
                "standing warning",
                &format!("Shield artwork unavailable: {reason}")
            ]
        );
    }

    #[test]
    fn shield_png_padding_and_every_refusal_use_the_shared_gui_path() {
        use dashboard_base::catalogue::texture_tests::{png_bytes, refusal_textures, texture_catalogue};
        let mask = Rgba8Image {
            width: 3,
            height: 2,
            pixels: vec![200; 24],
        };
        let refusals = refusal_textures();
        let mut textures: Vec<_> = refusals
            .iter()
            .filter(|(_, bytes, _)| !bytes.is_empty())
            .map(|(path, bytes, _)| (*path, bytes.clone()))
            .collect();
        textures.extend([
            ("gfx/mod/mask.png", png_bytes(&mask)),
            ("gfx/mod/overlay.png", png_bytes(&mask)),
        ]);
        let (mut catalogue, _folders) = texture_catalogue(
            &[
                (
                    "interface/topbar.gui",
                    "guiTypes = { containerWindowType = { name = topbar buttonType = { name = player_flag spriteType = GFX_mod_shield } } }",
                ),
                (
                    "interface/art.gfx",
                    "spriteTypes = { maskedShieldType = { name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = gfx/mod/mask.png effectFile = gfx/FX/mod.lua } }",
                ),
            ],
            &textures,
        );
        catalogue.gui_decode = Ok(content_reader_image::native::Decode {
            force_pow2_textures: true,
        });
        let loaded = ShieldStyle::load(&catalogue).unwrap();
        assert_eq!(
            [
                loaded.shield.overlay().unwrap().unwrap().width,
                loaded.panel.overlay().unwrap().unwrap().width
            ],
            [4, 4]
        );
        for (path, _, expected) in refusals {
            let facts = content_reader_gui::gui::native::sprites::read_sprites(format!(
                "spriteTypes = {{ maskedShieldType = {{ name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = \"{path}\" effectFile = gfx/FX/mod.lua }} }}"
            ).as_bytes());
            catalogue.sprites =
                content_reader_gfx::gfx::SpriteSet::new(&[("interface/art.gfx".into(), facts)]);
            let why = ShieldStyle::load(&catalogue).err().unwrap().to_string();
            assert!(why.contains(expected), "{path}: {why}");
            assert_shield_warning(&catalogue, &why);
        }
    }

    #[test]
    fn the_shield_s_descriptor_chooses_its_flag_set_and_panel_evens_only_its_overlay() {
        use dashboard_base::catalogue::texture_tests::{png_bytes, texture_catalogue};
        let overlay = Rgba8Image {
            width: 8,
            height: 8,
            pixels: (0..64)
                .flat_map(|at: u32| {
                    let (x, y) = (at % 8, at / 8);
                    let edge = x.min(y).min(7 - x).min(7 - y) == 0;
                    [10, 10, 10, if edge { 200 } else { 50 }]
                })
                .collect(),
        };
        for (small, sets) in [("no", [FlagSet::Normal]), ("yes", [FlagSet::Small])] {
            let gfx = format!(
                "spriteTypes = {{ maskedShieldType = {{ name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = gfx/mod/mask.png effectFile = gfx/FX/mod.lua small = {small} }} }}"
            );
            let (catalogue, _folders) = texture_catalogue(
                &[
                    (
                        "interface/topbar.gui",
                        "guiTypes = { containerWindowType = { name = topbar buttonType = { name = player_flag spriteType = GFX_mod_shield } } }",
                    ),
                    ("interface/art.gfx", &gfx),
                ],
                &[
                    ("gfx/mod/mask.png", png_bytes(&overlay)),
                    ("gfx/mod/overlay.png", png_bytes(&overlay)),
                ],
            );
            let style = ShieldStyle::load(&catalogue).unwrap();
            assert_eq!(style.sets(), sets);
            assert_eq!(style.shield.overlay().unwrap(), Some(overlay.clone()));
            assert_eq!(style.panel.overlay().unwrap(), Some(even_overlay(&overlay)));
        }
    }

    #[test]
    #[ignore = "requires DXC and a GPU; synthetic content only"]
    fn shield_and_panel_flags_draw_the_descriptor_s_flag_set_through_its_passes() {
        use dashboard_base::catalogue::texture_tests::{png_bytes, shield_programs};
        let flag = |[width, height]: [u32; 2], rgb: [u8; 3]| {
            let mut bytes = vec![0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            bytes.extend((width as u16).to_le_bytes());
            bytes.extend((height as u16).to_le_bytes());
            bytes.extend([24, 0x20]);
            for _ in 0..width * height {
                bytes.extend([rgb[2], rgb[1], rgb[0]]);
            }
            bytes
        };
        let square = |pixel: &dyn Fn(u32, u32) -> [u8; 4]| Rgba8Image {
            width: 8,
            height: 8,
            pixels: (0..64).flat_map(|at| pixel(at % 8, at / 8)).collect(),
        };
        let mask = square(&|_, _| [255; 4]);
        let overlay = square(&|x, y| {
            let edge = x.min(y).min(7 - x).min(7 - y) == 0;
            [0, 0, 0, if edge { 200 } else { 0 }]
        });
        let programs = shield_programs();
        let mut files: Vec<_> = programs
            .iter()
            .map(|(path, text)| (*path, text.as_str()))
            .collect();
        files.extend([
            (
                "interface/topbar.gui",
                "guiTypes = { containerWindowType = { name = topbar buttonType = { name = player_flag spriteType = GFX_mod_shield } } }",
            ),
            (
                "interface/art.gfx",
                "spriteTypes = { maskedShieldType = { name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = gfx/mod/mask.png effectFile = gfx/FX/shield_flag.lua small = yes } }",
            ),
        ]);
        let (_, folder) = dashboard_base::catalogue::tests::folder_catalogue(&files);
        let cells = [[4, 2], [2, 2], [2, 1]];
        for (path, bytes) in [
            ("gfx/mod/mask.png", png_bytes(&mask)),
            ("gfx/mod/overlay.png", png_bytes(&overlay)),
            ("gfx/flags/MOD.tga", flag(cells[0], [0, 0, 200])),
            ("gfx/flags/small/MOD.tga", flag(cells[2], [0, 200, 0])),
        ] {
            let file = folder.0.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
        let catalogue = dashboard_base::catalogue::tests::layers_catalogue(
            content_reader_layers::ResolvedLayers::from_folders(&[(&folder.0, &[][..])]).unwrap(),
        );
        let style = ShieldStyle::load(&catalogue).unwrap();
        let context = context(&catalogue.content, cells, Vec::new());
        let pixel = |reply: &CountryFlagReply, x: u32, y: u32| {
            let image = &reply.flags[0];
            let at = ((y * image.width + x) * 4) as usize;
            <[u8; 4]>::try_from(&decoded(image)[at..at + 4]).unwrap()
        };
        let mut drawn = Vec::new();
        for size in [CountryFlagSize::Shield, CountryFlagSize::Panel] {
            let request = CountryFlagParams {
                flags: vec![every("MOD")],
                size,
            };
            let reply = context
                .reply(&request, Some(Ok(&style)), &AtomicBool::new(false))
                .unwrap();
            assert!(reply.unavailable.is_empty(), "{:?}", reply.unavailable);
            assert_eq!(reply.flags[0].asset, "gfx/flags/small/MOD.tga");
            assert_eq!([reply.flags[0].width, reply.flags[0].height], [8, 8]);
            assert_eq!(pixel(&reply, 4, 4), [0, 200, 0, 255]);
            drawn.push(pixel(&reply, 0, 4));
        }
        let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 1);
        assert!(near(drawn[0], [0, 43, 0, 255]), "{:?}", drawn[0]);
        assert!(near(drawn[1], [0, 200, 0, 255]), "{:?}", drawn[1]);
    }

    #[test]
    fn shared_defines_cold_flag_cells_do_not_read_the_script_again() {
        use std::sync::atomic::AtomicUsize;

        let (folder, cells) = recorded_shield();
        let reads = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&reads);
        let layers = content_reader_layers::ResolvedLayers::from_folders(&[(&folder.0, &[])]).unwrap()
            .with_read_trace(Arc::new(move |read| {
                if read.path == b"common/defines/00_flags.lua" {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        let catalogue = dashboard_base::catalogue::tests::layers_catalogue(layers.clone());
        catalogue.defines().unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert_eq!(flag_cells(&layers).unwrap(), cells);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn shared_defines_give_flags_the_mod_cell_width_or_name_the_unreadable_size() {
        let read = |value| {
            let (_machine, source, load) = dashboard_base::test_fixtures::recorded_mod_defines(
                "NDefines={NGraphics={}} for _,p in ipairs({'COUNTRY_FLAG_TEX','COUNTRY_FLAG_MEDIUM_TEX','COUNTRY_FLAG_SMALL_TEX'}) do NDefines.NGraphics[p..'_WIDTH']=4 NDefines.NGraphics[p..'_HEIGHT']=2 NDefines.NGraphics[p..'_MAX_SIZE']=16 end",
                &format!("NDefines.NGraphics.COUNTRY_FLAG_TEX_WIDTH={value}"),
                &[],
            );
            let resolved = source.content(&load.content().unwrap().options).unwrap();
            FlagFolders::read(&resolved).map(|folders| folders.cells)
        };
        assert_eq!(read("11").unwrap(), [[11, 2], [4, 2], [4, 2]]);
        let error = read("nil").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "NDefines.NGraphics.COUNTRY_FLAG_TEX_WIDTH is not a positive size");
    }

    fn recorded_shield() -> (dashboard_base::catalogue::tests::Folder, [[u32; 2]; 3]) {
        use dashboard_base::catalogue::texture_tests::shield_programs;
        let programs = shield_programs();
        let mut files: Vec<_> = programs
            .iter()
            .map(|(path, text)| (*path, text.as_str()))
            .collect();
        files.extend([
            (
                "common/defines/00_flags.lua",
                "NDefines = { NGraphics = { COUNTRY_FLAG_TEX_WIDTH = 4, COUNTRY_FLAG_TEX_HEIGHT = 2, COUNTRY_FLAG_TEX_MAX_SIZE = 8, COUNTRY_FLAG_MEDIUM_TEX_WIDTH = 2, COUNTRY_FLAG_MEDIUM_TEX_HEIGHT = 2, COUNTRY_FLAG_MEDIUM_TEX_MAX_SIZE = 8, COUNTRY_FLAG_SMALL_TEX_WIDTH = 2, COUNTRY_FLAG_SMALL_TEX_HEIGHT = 1, COUNTRY_FLAG_SMALL_TEX_MAX_SIZE = 8 } }",
            ),
            (
                "interface/topbar.gui",
                "guiTypes = { containerWindowType = { name = topbar buttonType = { name = player_flag spriteType = GFX_mod_shield } } }",
            ),
            (
                "interface/art.gfx",
                "spriteTypes = { maskedShieldType = { name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = gfx/mod/mask.png effectFile = gfx/FX/shield_flag.lua small = yes } }",
            ),
        ]);
        let (_, folder) = dashboard_base::catalogue::tests::folder_catalogue(&files);
        let cells = [[4, 2], [2, 2], [2, 1]];
        let mut written = vec![
            ("gfx/mod/mask.png", shield_square(&|_, _| [255; 4])),
            (
                "gfx/mod/overlay.png",
                shield_square(&|x, y| {
                    let edge = x.min(y).min(7 - x).min(7 - y) == 0;
                    [0, 0, 0, if edge { 200 } else { 0 }]
                }),
            ),
        ];
        written.extend([
            ("gfx/flags/MOD.tga", shield_flag(cells[0], [0, 0, 200])),
            (
                "gfx/flags/small/MOD.tga",
                shield_flag(cells[2], [0, 200, 0]),
            ),
        ]);
        for (path, bytes) in written {
            let file = folder.0.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
        (folder, cells)
    }

    fn shield_square(pixel: &dyn Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        dashboard_base::catalogue::texture_tests::png_bytes(&Rgba8Image {
            width: 8,
            height: 8,
            pixels: (0..64).flat_map(|at| pixel(at % 8, at / 8)).collect(),
        })
    }

    fn shield_flag([width, height]: [u32; 2], rgb: [u8; 3]) -> Vec<u8> {
        let mut bytes = vec![0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        bytes.extend((width as u16).to_le_bytes());
        bytes.extend((height as u16).to_le_bytes());
        bytes.extend([24, 0x20]);
        for _ in 0..width * height {
            bytes.extend([rgb[2], rgb[1], rgb[0]]);
        }
        bytes
    }

    fn shield_pixels(
        layers: content_reader_layers::ResolvedLayers,
        cells: [[u32; 2]; 3],
    ) -> ([u8; 4], [u8; 4], BTreeSet<String>) {
        let ((centre, edge), fallbacks) = content_reader_layers::content::observe_overlay_refusals(|| {
            let catalogue = Catalogue::of_layers(layers, "english").unwrap();
            let style = ShieldStyle::load(&catalogue).unwrap();
            let context = context(&catalogue.content, cells, Vec::new());
            let request = CountryFlagParams {
                flags: vec![every("MOD")],
                size: CountryFlagSize::Shield,
            };
            let reply = context
                .reply(&request, Some(Ok(&style)), &AtomicBool::new(false))
                .unwrap();
            assert!(reply.unavailable.is_empty(), "{:?}", reply.unavailable);
            let image = &reply.flags[0];
            let pixels = decoded(image);
            let pixel = |x: u32, y: u32| {
                let at = ((y * image.width + x) * 4) as usize;
                <[u8; 4]>::try_from(&pixels[at..at + 4]).unwrap()
            };
            (pixel(4, 4), pixel(0, 4))
        });
        (centre, edge, fallbacks)
    }

    fn with_visual_mod(
        mut layers: content_reader_layers::ResolvedLayers,
        user: &std::path::Path,
        mod_root: &std::path::Path,
    ) -> content_reader_layers::ResolvedLayers {
        let descriptor = user.join("mod/shield.mod");
        std::fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
        std::fs::write(
            &descriptor,
            format!(
                "name=\"Shield\"\npath=\"{}\"\n",
                mod_root.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        let options = ResolveOptions {
            game_dir: user.to_owned(),
            user_dir: user.to_owned(),
            cache_dir: user.join("cache"),
        };
        let choice = content_reader_layers::content::visual::Choice {
            id: "mod/shield.mod".into(),
            name: "Shield".into(),
        };
        let plan = content_reader_layers::content::visual::apply(
            &mut layers,
            &options,
            &[choice],
            &[],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        layers
    }

    #[test]
    fn the_shield_follows_the_selected_visual_mods_and_falls_back_by_name() {
        let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 1);
        let (recorded, cells) = recorded_shield();
        let base =
            || content_reader_layers::ResolvedLayers::from_folders(&[(&recorded.0, &[][..])]).unwrap();
        let (centre, edge, fallbacks) = shield_pixels(base(), cells);
        assert_eq!(centre, [0, 200, 0, 255]);
        assert!(near(edge, [0, 43, 0, 255]), "{edge:?}");
        assert!(fallbacks.is_empty(), "{fallbacks:?}");
        let visual = |name: &str, files: &[(&str, Vec<u8>)]| {
            let (_, root) = dashboard_base::catalogue::tests::folder_catalogue(&[]);
            for (path, bytes) in files {
                let file = root.0.join(name).join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, bytes).unwrap();
            }
            let layers = with_visual_mod(base(), &root.0.join("user"), &root.0.join(name));
            shield_pixels(layers, cells)
        };
        let named = |name: &str, overlay: &str, mask: &str, effect: &str, small: &str| {
            format!(
                "spriteTypes = {{ maskedShieldType = {{ name = {name} textureFile1 = {overlay} textureFile2 = {mask} effectFile = {effect} small = {small} }} }}"
            )
            .into_bytes()
        };
        let declared = |overlay: &str, mask: &str, effect: &str| {
            named("GFX_mod_shield", overlay, mask, effect, "yes")
        };
        let fell_back = |fallbacks: &BTreeSet<String>| {
            fallbacks
                .iter()
                .any(|why| why.starts_with("shield player_flag ("))
        };
        let clear = shield_square(&|_, _| [0, 0, 0, 0]);
        let (centre, edge, fallbacks) = visual(
            "own",
            &[
                (
                    "interface/art.gfx",
                    declared(
                        "gfx/visual/overlay.png",
                        "gfx/mod/mask.png",
                        "gfx/FX/shield_flag.lua",
                    ),
                ),
                ("gfx/visual/overlay.png", clear),
            ],
        );
        assert_eq!((centre, edge), ([0, 200, 0, 255], [0, 200, 0, 255]));
        assert!(fallbacks.is_empty(), "{fallbacks:?}");
        let (centre, edge, fallbacks) = visual(
            "missing",
            &[(
                "interface/art.gfx",
                declared(
                    "gfx/mod/overlay.png",
                    "gfx/visual/absent_mask.png",
                    "gfx/FX/shield_flag.lua",
                ),
            )],
        );
        assert_eq!(centre, [0, 200, 0, 255]);
        assert!(near(edge, [0, 43, 0, 255]), "{edge:?}");
        assert!(fell_back(&fallbacks), "{fallbacks:?}");
        let (centre, edge, fallbacks) = visual(
            "renamed",
            &[
                (
                    "interface/topbar.gui",
                    b"guiTypes = { containerWindowType = { name = topbar buttonType = { name = player_flag spriteType = GFX_visual_shield } } }"
                        .to_vec(),
                ),
                (
                    "interface/visual.gfx",
                    named(
                        "GFX_visual_shield",
                        "gfx/mod/overlay.png",
                        "gfx/visual/absent_mask.png",
                        "gfx/FX/shield_flag.lua",
                        "yes",
                    ),
                ),
            ],
        );
        assert_eq!(centre, [0, 200, 0, 255]);
        assert!(near(edge, [0, 43, 0, 255]), "{edge:?}");
        assert!(fell_back(&fallbacks), "{fallbacks:?}");
        let (centre, edge, fallbacks) = visual(
            "untouched",
            &[
                (
                    "interface/other.gfx",
                    b"spriteTypes = { spriteType = { name = GFX_other textureFile = gfx/visual/other.png } }"
                        .to_vec(),
                ),
                ("gfx/visual/other.png", shield_square(&|_, _| [9, 9, 9, 255])),
            ],
        );
        assert_eq!(centre, [0, 200, 0, 255]);
        assert!(near(edge, [0, 43, 0, 255]), "{edge:?}");
        assert!(fallbacks.is_empty(), "{fallbacks:?}");
        let (centre, edge, fallbacks) = visual(
            "undrawn",
            &[(
                "interface/art.gfx",
                named(
                    "GFX_mod_shield",
                    "gfx/mod/overlay.png",
                    "gfx/mod/mask.png",
                    "gfx/FX/absent_flag.lua",
                    "no",
                ),
            )],
        );
        assert_eq!(centre, [0, 200, 0, 255]);
        assert!(near(edge, [0, 43, 0, 255]), "{edge:?}");
        assert!(fell_back(&fallbacks), "{fallbacks:?}");
    }

    #[test]
    fn a_kept_shield_belongs_to_the_catalogue_it_came_from() {
        let (recorded, _) = recorded_shield();
        let base =
            || content_reader_layers::ResolvedLayers::from_folders(&[(&recorded.0, &[][..])]).unwrap();
        let (_, root) = dashboard_base::catalogue::tests::folder_catalogue(&[]);
        let broken = root.0.join("broken");
        std::fs::create_dir_all(broken.join("interface")).unwrap();
        std::fs::write(
            broken.join("interface/art.gfx"),
            "spriteTypes = { maskedShieldType = { name = GFX_mod_shield textureFile1 = gfx/mod/overlay.png textureFile2 = gfx/mod/mask.png effectFile = gfx/FX/absent_flag.lua small = yes } }",
        )
        .unwrap();
        let shown = Catalogue::of_layers(base(), "english").unwrap();
        let selected = Catalogue::of_layers(
            with_visual_mod(base(), &root.0.join("user"), &broken),
            "english",
        )
        .unwrap();
        let mut slot = None;
        let first = cached_shield(&mut slot, &shown).unwrap();
        assert!(first.recorded.is_none());
        assert!(Arc::ptr_eq(
            &first,
            &cached_shield(&mut slot, &shown).unwrap()
        ));
        let switched = cached_shield(&mut slot, &selected).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &switched),
            "another catalogue loads its own"
        );
        assert!(
            switched.recorded.is_some(),
            "the selected mods' broken shield falls back"
        );
    }

    #[test]
    fn panel_overlay_loses_its_edge_shading_and_keeps_its_texture() {
        let mut pixels = Vec::new();
        for y in 0..16_u32 {
            for x in 0..16_u32 {
                let edge = x.min(y).min(15 - x).min(15 - y) < 2;
                let alpha = if edge {
                    150
                } else if (x + y) % 2 == 0 {
                    40
                } else {
                    60
                };
                pixels.extend_from_slice(&[10, 10, 10, alpha]);
            }
        }
        let overlay = Rgba8Image {
            width: 16,
            height: 16,
            pixels,
        };
        let even = even_overlay(&overlay);
        let alpha = |image: &Rgba8Image, x: u32, y: u32| {
            u32::from(image.pixels[((y * 16 + x) * 4 + 3) as usize])
        };
        for (x, y) in [(0, 0), (0, 8), (15, 15)] {
            assert!(
                alpha(&even, x, y) * 5 < alpha(&overlay, x, y) * 3,
                "the dark edge at ({x}, {y}) is evened out"
            );
        }
        assert_eq!(
            (alpha(&even, 7, 8), alpha(&even, 8, 8)),
            (alpha(&overlay, 7, 8), alpha(&overlay, 8, 8)),
            "the centre's texture is unchanged"
        );
        assert!(
            alpha(&even, 5, 4) > alpha(&even, 4, 4),
            "the weave keeps its contrast"
        );
        assert_eq!(
            &even.pixels[..3],
            &overlay.pixels[..3],
            "colours are unchanged"
        );
    }

    #[test]
    fn flag_params_default_to_full_and_reject_unsafe_keys_and_unbounded_batches() {
        let value = serde_json::json!({
            "flags": [{ "keys": ["GER", null, "GER"], "reasons": [null, "absent", null] }],
        });
        let params: CountryFlagParams = serde_json::from_value(value).unwrap();
        assert_eq!(params.size, CountryFlagSize::Full);
        assert!(CountryFlags::validate(&params).is_ok());
        let relayed: CountryFlagChoice = serde_json::from_value(serde_json::json!({
            "keys": [null, null, null], "reasons": ["absent", "absent", "absent"], "searched": [false, null, true],
        }))
        .unwrap();
        assert_eq!(relayed.searched.clone().map(|searched| searched.is_some()), [false, false, true], "only an affirmative relay is evidence of a search");
        assert_eq!(serde_json::to_value(&relayed).unwrap()["searched"], serde_json::json!([null, null, true]));
        let mut params = CountryFlagParams {
            flags: vec![every("variants/red"), every("mod country tag")],
            size: CountryFlagSize::Full,
        };
        assert!(CountryFlags::validate(&params).is_ok());
        for key in ["../escape", "", "a\\b", "c:flag"] {
            params.flags = vec![every(key)];
            assert!(CountryFlags::validate(&params).is_err(), "{key:?}");
        }
        params.flags = Vec::new();
        assert!(CountryFlags::validate(&params).is_err());
        params.flags = vec![every("GER"); 33];
        assert!(CountryFlags::validate(&params).is_err());
    }

    #[test]
    fn medium_flag_falls_back_to_normal_with_deduplicated_warning() {
        let (_, folder) = dashboard_base::catalogue::tests::folder_catalogue(&[]);
        let cells = [[4, 2], [2, 2], [2, 1]];
        let file = folder.0.join("gfx/flags/MOD.tga");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, shield_flag(cells[0], [10, 120, 220])).unwrap();
        let catalogue = dashboard_base::catalogue::tests::layers_catalogue(
            content_reader_layers::ResolvedLayers::from_folders(&[(&folder.0, &[][..])]).unwrap(),
        );
        let ctx = context(&catalogue.content, cells, Vec::new());
        let choice = CountryFlagChoice {
            keys: [Some("MOD".into()), None, None],
            reasons: [None, Some("missing".into()), None],
            searched: Default::default(),
        };
        let request = CountryFlagParams {
            flags: vec![choice.clone(), choice],
            size: CountryFlagSize::Medium,
        };
        let reply = ctx.reply(&request, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(reply.flags.len(), 2);
        assert!(reply.flags.iter().all(|f| f.asset == "gfx/flags/MOD.tga"));
        let sub_warnings: Vec<_> = reply
            .warnings
            .iter()
            .filter(|w| w.contains("medium flag is missing or undecodable") && w.contains("gfx/flags/MOD.tga"))
            .collect();
        assert_eq!(sub_warnings.len(), 1, "substitution warning emitted once per asset: {:?}", reply.warnings);
    }
}
