use content_reader_country_names::country_names;
use content_reader_localisation::localisation::Argument;
use content_reader_map::province_geometry::ProvinceGeometry;
use dashboard_base::{Source, catalogue::{self, Catalogue}, presentation::Cancel, products::{LoadContext, at_hour::{self, Read}, read_ahead::Preparation}};
use dashboard_query::coverage::{Witness, reading_witness, witnessed_selection};
use dashboard_text::{metadata::{check_cancel, not_recorded}, naval_battle_name::{Picker, battle_name_candidates}, scope, scripted_text};
use replay_columnar::manifest::Status;
use replay_data::{codec::Decoder, rows::{Field, Opaque, StateRow, TableId, tables}, scope::CaptureMode, value::Value as _};
use replay_frame::frame::{DecodedFrame, DecodedSource, TableView, table_field};
use replay_lanes::lanes::{self, Cell, History, Keys, Reading, StatusRun};
use replay_recording::columnar_frames::{Recording, TableCoverage};
use std::{cell::{OnceCell, RefCell}, collections::{BTreeMap, BTreeSet, HashMap}, io, sync::{Arc, OnceLock, atomic::AtomicBool}};

#[cfg(test)]
mod tests;

pub const TABLES: [TableId; 7] = [
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::ReservedCountryNaming,
    TableId::Provinces,
    TableId::ProvinceNames,
    TableId::States,
];

const STATE_FIELDS: &[&str] = &["owner", "custom_name", "owner_name"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvinceMode { Place, Land, Naval }

impl ProvinceMode {
    pub fn from_land(land: bool) -> Self { if land { Self::Land } else { Self::Place } }
}

pub fn reads(provinces: &[u32], states: &[u32], step: usize, held: &DecodedFrame) -> Vec<Read> {
    match step {
        0 => {
            let mut reads: Vec<Read> = TABLES[..4].iter().map(|&table| Read::all(table)).collect();
            if !provinces.is_empty() {
                reads.push(Read::these(TableId::Provinces, provinces.iter().map(|&id| u128::from(id))));
                reads.push(Read::these(TableId::ProvinceNames, provinces.iter().map(|&id| u128::from(id))));
            }
            reads
        }
        1 => {
            let mut wanted: BTreeSet<u32> = states.iter().copied().collect();
            wanted.extend(witnessed_selection::<tables::Provinces>(held).rows.iter().map(|row| row.state).filter(|&state| state != 0));
            if wanted.is_empty() { return Vec::new(); }
            vec![Read::these(TableId::States, wanted.into_iter().map(u128::from))]
        }
        _ => Vec::new(),
    }
}

pub struct Content {
    pub cat: Arc<Catalogue>,
    regions: OnceLock<Result<Regions, String>>,
    naval: OnceLock<Result<NavalContent, String>>,
}

struct Regions {
    definitions: Arc<content_reader_regions::regions::StrategicRegions>,
    by_region: BTreeMap<u32, OnceLock<Result<String, String>>>,
    region_of: BTreeMap<u32, Result<u32, String>>,
}

struct NavalContent {
    geometry: Arc<ProvinceGeometry>,
    defines: Result<[i64; 2], String>,
    candidates: BTreeMap<u32, Vec<u32>>,
}

#[derive(Default)]
struct Cached { held: [Option<Arc<Content>>; 2] }

type PreloadKey = (dashboard_base::content::LocalisationKey, u64);

#[derive(Default)]
struct Preload {
    held: [Option<(PreloadKey, Arc<Preparation>)>; 2],
}

impl Content {
    pub fn read(source: &Source, load: &LoadContext) -> io::Result<Arc<Self>> {
        let options = load.content.as_ref().ok_or_else(|| dashboard_core::refused("Place names need the recorded content stack and language"))?;
        let cat = catalogue::catalogue(source, options)?;
        let state = source.state::<Cached>();
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        let slot = &mut state.held[usize::from(catalogue::reads_recorded(options))];
        if let Some(content) = slot && Arc::ptr_eq(&content.cat, &cat) {
            return Ok(Arc::clone(content));
        }
        let content = Arc::new(Self { cat, regions: OnceLock::new(), naval: OnceLock::new() });
        *slot = Some(Arc::clone(&content));
        Ok(content)
    }

    pub(super) fn prepare(source: &Arc<Source>, load: &LoadContext) -> io::Result<Option<Arc<Preparation>>> {
        Self::prepare_with(source, load, || {})
    }

    fn prepare_with(source: &Arc<Source>, load: &LoadContext, before_load: impl FnOnce() + Send + 'static) -> io::Result<Option<Arc<Preparation>>> {
        let Some(options) = &load.content else { return Ok(None); };
        let binding = Arc::clone(options.presentation.binding());
        let still_selected = |binding: &dashboard_base::presentation::Binding| {
            check_cancel(binding.flag()).map_err(|_| dashboard_core::refused("Place names unavailable: the content selection changed during preparation"))
        };
        let has_content = source.bound_content(&options.options).is_some();
        still_selected(&binding)?;
        if !has_content { return Ok(None); }
        let key = (dashboard_base::content::localisation_key(options), binding.generation);
        let state = source.state::<Preload>();
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        let slot = &mut state.held[usize::from(catalogue::reads_recorded(options))];
        if let Some((held, ready)) = slot && *held == key { return ready.pending(); }
        let ready = Arc::<Preparation>::default();
        *slot = Some((key, Arc::clone(&ready)));
        drop(state);
        let source = Arc::downgrade(source);
        let options = options.clone();
        ready.start("row place names ahead", move || {
            let source = source.upgrade().ok_or_else(|| io::Error::other("Recording closed"))?;
            still_selected(&binding)?;
            before_load();
            source.in_scope(|| {
                if let Some(Ok(catalogue)) = catalogue::bound_catalogue(&source, &options) {
                    still_selected(&binding)?;
                    drop(catalogue.cached_states());
                }
                still_selected(&binding)
            })
        });
        ready.pending()
    }

    fn regions(&self) -> Result<&Regions, String> {
        self.regions.get_or_init(|| dashboard_base::content::strategic_regions(&self.cat.content.layers)
            .map(|regions| {
                let mut region_of = BTreeMap::new();
                for (&id, region) in &regions.regions {
                    for province in region.native_provinces(|_| true) {
                        if region_of.insert(province, Ok(id)).is_some() {
                            region_of.insert(province, Err(format!("province {province}: multiple strategic-region memberships refused")));
                        }
                    }
                }
                let by_region = regions.regions.keys().map(|&id| (id, OnceLock::new())).collect();
                Regions { definitions: regions, by_region, region_of }
            })
            .map_err(|error| format!("Strategic-region names unavailable: {error}")))
            .as_ref().map_err(Clone::clone)
    }

    pub fn region(&self, id: u32) -> Result<String, String> {
        let regions = self.regions()?;
        let name = regions.by_region.get(&id).ok_or_else(|| format!("region {id}: no content-defined strategic region"))?;
        name.get_or_init(|| crate::products::region_names::region_name(&self.cat, &regions.definitions, id)).clone()
    }

    fn province_region(&self, id: u32) -> Result<String, String> {
        self.province_region_or_none(id)?.ok_or_else(|| format!("province {id}: no content-defined strategic region"))
    }

    fn province_region_or_none(&self, id: u32) -> Result<Option<String>, String> {
        let regions = self.regions()?;
        regions.region_of.get(&id).cloned().transpose()?.map(|region| self.region(region)).transpose()
    }

    fn province_region_id(&self, id: u32) -> Result<u32, String> {
        let regions = self.regions()?;
        regions.region_of.get(&id).cloned().unwrap_or_else(|| Err(format!("province {id} is in no content strategic region")))
    }

    fn naval(&self) -> Result<&NavalContent, &str> {
        self.naval
            .get_or_init(|| {
                let regions = dashboard_base::content::strategic_regions(&self.cat.content.layers).map_err(|e| format!("Strategic regions unavailable: {e}"))?;
                let states = self.cat.cached_states()?;
                let geometry = ProvinceGeometry::shared(&self.cat.content.layers).map_err(|e| format!("Province geometry unavailable: {e}"))?;
                let defines = self.cat.defines().and_then(|d| Ok([
                    d.fixed5("NNavy", "BATTLE_NAME_VP_FACTOR").map_err(|e| e.to_string())?,
                    d.fixed5("NNavy", "BATTLE_NAME_VP_CUTOFF").map_err(|e| e.to_string())?,
                ]));
                let candidates = battle_name_candidates(&regions.regions, states.states.definitions(), &|p| geometry.centroids.contains_key(&p));
                Ok(NavalContent { geometry, defines, candidates })
            })
            .as_ref()
            .map_err(String::as_str)
    }

    pub fn naval_candidate_provinces(&self, provinces: impl IntoIterator<Item = u32>) -> Vec<u32> {
        let (Ok(naval), Ok(regions)) = (self.naval(), self.regions()) else { return provinces.into_iter().collect(); };
        let mut out = BTreeSet::new();
        for province in provinces {
            out.insert(province);
            if let Some(Ok(region)) = regions.region_of.get(&province) && let Some(candidates) = naval.candidates.get(region) {
                out.extend(candidates.iter().copied());
            }
        }
        out.into_iter().collect()
    }

    pub fn land_battle_title(&self, place: Name) -> Result<String, String> {
        if let Some(missing) = place.missing {
            return Err(missing);
        }
        match place.name.filter(|name| !name.trim().is_empty()) {
            Some(name) => Ok(format!("Battle of {name}")),
            None => Err("Province name not recorded".into()),
        }
    }

    pub fn naval_battle_title_text(&self, name: &str, port_strike: bool) -> Result<String, String> {
        if name.trim().is_empty() { return Err("Battle place name not recorded".into()); }
        let key = if port_strike { "NAVAL_PORT_STRIKE_TITLE" } else { "NAVAL_BATTLE_TITLE" };
        let text = self.cat.localisation.localise_with(key.as_bytes(), &[("NAME", Argument::Text(name))]).map_err(|e| e.to_string())?;
        catalogue::plain_text(&text)
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cat.verification.iter().chain(&self.cat.warnings).cloned().collect();
        match self.regions() {
            Ok(regions) => out.extend(regions.definitions.notes.iter().cloned()),
            Err(reason) => out.push(reason),
        }
        if let Err(reason) = self.cat.cached_states() { out.push(reason); }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub name: Option<String>,
    pub missing: Option<String>,
}

impl Name {
    fn found(name: String) -> Self { Self { name: Some(name), missing: None } }
    fn missing(reason: impl Into<String>) -> Self { Self { name: None, missing: Some(reason.into()) } }
    fn of(name: Result<String, String>) -> Self { match name { Ok(n) => Self::found(n), Err(r) => Self::missing(r) } }
}

fn resolve_province(
    content: &Content,
    id: u32,
    state: u32,
    mode: ProvinceMode,
    rename: Result<Option<&[u8]>, String>,
    vp_tag: impl FnOnce() -> Result<String, String>,
    state_name: impl FnOnce(u32) -> Result<String, String>,
) -> Name {
    let loc = &content.cat.localisation;
    let rename = match rename {
        Ok(rename) => rename,
        Err(reason) => return Name::missing(reason),
    };
    if let Some(raw) = rename.filter(|raw| !raw.is_empty()) {
        let translated = loc.localise(raw).map_err(|e| e.to_string()).and_then(|text| catalogue::plain_text(&text));
        return match translated {
            Ok(name) if !matches!(mode, ProvinceMode::Land) || !name.is_empty() => Name::found(name),
            Err(reason) => Name::missing(reason),
            Ok(_) => fallback_province(content, id, state, mode, state_name),
        };
    }
    let vp = vp_tag()
        .and_then(|tag| scripted_text::victory_point_name(loc, id, &tag))
        .and_then(|text| (!text.is_empty()).then(|| catalogue::plain_text(&text)).transpose());
    match vp {
        Ok(Some(name)) if !matches!(mode, ProvinceMode::Land | ProvinceMode::Naval) || !name.is_empty() => Name::found(name),
        Err(reason) if matches!(mode, ProvinceMode::Naval) => Name::missing(reason),
        Err(reason) => match fallback_province(content, id, state, mode, state_name) {
            Name { name: Some(name), missing: None } => Name { name: Some(name), missing: Some(reason) },
            Name { missing: Some(fb), .. } => Name::missing(format!("{reason}; {fb}")),
            _ => Name::missing(reason),
        },
        Ok(_) => fallback_province(content, id, state, mode, state_name),
    }
}

fn fallback_province(
    content: &Content,
    id: u32,
    state: u32,
    mode: ProvinceMode,
    state_name: impl FnOnce(u32) -> Result<String, String>,
) -> Name {
    if state != 0 {
        return Name::of(state_name(state));
    }
    match mode {
        ProvinceMode::Land => Name::missing(format!("province {id}: no state name available")),
        ProvinceMode::Place => Name::of(content.province_region(id)),
        ProvinceMode::Naval => match content.province_region_or_none(id) {
            Ok(Some(name)) => Name::found(name),
            Ok(None) => Name::found(id.to_string()),
            Err(reason) => Name::missing(reason),
        },
    }
}

pub struct Places<'f> {
    frame: &'f DecodedFrame,
    provinces: OnceCell<HashMap<u32, u32>>,
    renames: OnceCell<HashMap<u32, &'f [u8]>>,
    states: OnceCell<HashMap<u32, &'f StateRow>>,
    links: OnceCell<HashMap<u32, u32>>,
    tags: RefCell<HashMap<u32, Result<String, String>>>,
}

impl<'f> Places<'f> {
    pub fn new(frame: &'f DecodedFrame) -> Self {
        Self {
            frame,
            provinces: OnceCell::new(),
            renames: OnceCell::new(),
            states: OnceCell::new(),
            links: OnceCell::new(),
            tags: RefCell::new(HashMap::new()),
        }
    }

    fn view(&self, table: TableId) -> &'f TableView {
        let frame: &'f DecodedFrame = self.frame;
        &frame.tables[table.index()]
    }

    fn provinces(&self) -> &HashMap<u32, u32> {
        self.provinces.get_or_init(|| match self.view(TableId::Provinces) {
            TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.rows::<tables::Provinces>().unwrap_or_default().iter().map(|row| (row.id, row.state)).collect(),
            TableView::Unavailable { .. } | TableView::NotRecorded { .. } => HashMap::new(),
        })
    }

    fn renames(&self) -> &HashMap<u32, &'f [u8]> {
        self.renames.get_or_init(|| {
            let mut out = HashMap::new();
            if let TableView::Complete(rows) | TableView::Truncated { rows, .. } = self.view(TableId::ProvinceNames) {
                for row in rows.rows::<tables::ProvinceNames>().unwrap_or_default() {
                    out.entry(row.province).or_insert(&row.name[..]);
                }
            }
            out
        })
    }

    fn states(&self) -> &HashMap<u32, &'f StateRow> {
        self.states.get_or_init(|| {
            let mut out = HashMap::new();
            if let TableView::Complete(rows) | TableView::Truncated { rows, .. } = self.view(TableId::States) {
                for row in rows.rows::<tables::States>().unwrap_or_default() {
                    out.entry(row.id).or_insert(row);
                }
            }
            out
        })
    }

    fn links(&self) -> &HashMap<u32, u32> {
        self.links.get_or_init(|| match self.view(TableId::CountryLinks) {
            TableView::Complete(rows) | TableView::Truncated { rows, .. } => rows.rows::<tables::CountryLinks>().unwrap_or_default().iter().map(|link| (link.id, link.canonical_slot)).collect(),
            TableView::Unavailable { .. } | TableView::NotRecorded { .. } => HashMap::new(),
        })
    }

    fn tag(&self, slot: u32) -> Result<String, String> {
        if let Some(tag) = self.tags.borrow().get(&slot) {
            return tag.clone();
        }
        let tag = country_names::original_or_own_tag(self.frame, slot).map(str::to_owned);
        self.tags.borrow_mut().insert(slot, tag.clone());
        tag
    }

    pub fn province_state(&self, id: u32) -> Option<u32> {
        self.provinces().get(&id).copied()
    }

    fn rename(&self, id: u32) -> Result<Option<&'f [u8]>, String> {
        match self.renames().get(&id) {
            Some(name) => Ok(Some(name)),
            None if (self.frame as &dyn DecodedSource).complete_status(TableId::ProvinceNames).is_ok() => Ok(None),
            None => Err(format!("GetName of province {id}: province names {}", scope::gap(self.view(TableId::ProvinceNames)))),
        }
    }

    fn victory_point_tag(&self, id: u32, state: u32) -> Result<String, String> {
        if state == 0 {
            return self.tag(0);
        }
        let row = self.states().get(&state).ok_or_else(|| format!("province {id}: state {state} has no States row: {}", scope::gap(self.view(TableId::States))))?;
        if row.owner == 0 {
            return self.tag(0);
        }
        match self.links().get(&row.owner) {
            Some(&slot) => self.tag(slot),
            None if (self.frame as &dyn DecodedSource).complete_status(TableId::CountryLinks).is_ok() => Err(format!("province {id}: state owner's link {} resolves to no country", row.owner)),
            None => Err(format!("province {id}: state owner's link {} is unknown: {}", row.owner, scope::gap(self.view(TableId::CountryLinks)))),
        }
    }

    fn state_display(&self, content: &Content, id: u32, absent: impl FnOnce() -> String) -> Result<String, String> {
        if let TableView::NotRecorded { mode } = self.view(TableId::States) {
            return Err(not_recorded(self.frame.hour, TableId::States, mode));
        }
        let row = self.states().get(&id).ok_or_else(absent)?;
        let states = content.cat.cached_states();
        let names = states.as_ref().map(|states| &states.names).map_err(String::as_str);
        scripted_text::state_display_name(row, &content.cat, names, scripted_text::StateNameUsage::Place)
    }

    pub fn state(&self, content: &Content, id: u32) -> Name {
        Name::of(self.state_display(content, id, || format!("Not in this recording: state {id} at this hour")))
    }

    pub fn province(&self, content: &Content, id: u32, land: bool) -> Name {
        let Some(state) = self.province_state(id) else {
            return Name::missing(format!("Not in this recording: province {id} at this hour"));
        };
        resolve_province(content, id, state, ProvinceMode::from_land(land), self.rename(id), || self.victory_point_tag(id, state), |s| self.state_display(content, s, || format!("state {s}: Not in this recording")))
    }
}

pub fn read_hour(query: &dashboard_query::Query<'_>, frames: &mut at_hour::Frames<'_>, hour: i32, provinces: &[u32], states: &[u32]) -> Result<DecodedFrame, String> {
    if query.timeline.at(hour).is_none() {
        let reason = query.timeline.unheld.iter().find(|unheld| unheld.first <= hour && hour <= unheld.last).map_or("the hour was not recorded", |unheld| unheld.reason.as_str());
        return Err(format!("Not in this recording: {reason}"));
    }
    let mut frame = DecodedFrame { branch: query.timeline.id, hour, tables: std::array::from_fn(|_| replay_frame::frame::not_selected()), metrics: Default::default() };
    for step in 0.. {
        let reads = reads(provinces, states, step, &frame);
        if reads.is_empty() {
            break;
        }
        let read = frames.frame(hour, &reads, "Place names").map_err(|error| format!("Not in this recording: {error}"))?;
        for table in reads.iter().map(|read| read.table.index()) {
            frame.tables[table] = read.tables[table].clone();
        }
    }
    Ok(frame)
}

fn status_at(history: &History, hour: i32) -> (Status, Option<&str>) {
    let at = history.statuses.partition_point(|run| run.last < hour);
    match history.statuses.get(at).filter(|run| run.first <= hour) {
        Some(run) => match run.status {
            status @ Status::Unavailable { reason } => (status, history.reasons.get(&reason).map(String::as_str)),
            status => (status, None),
        },
        None => (Status::NotRecorded, None),
    }
}

fn is_complete(history: &History, hour: i32) -> bool { matches!(status_at(history, hour).0, Status::Complete) }

fn status_gap(status: Status, reason: Option<&str>, mode: CaptureMode) -> String {
    match status {
        Status::Complete => "the complete table has no row for it".into(),
        Status::Truncated { missing } => format!("the table is truncated ({missing} rows missing)"),
        Status::Unavailable { .. } => format!("the table is unavailable: {}", reason.unwrap_or_default()),
        Status::NotRecorded => format!("the table is not recorded in a {mode} recording"),
    }
}

fn unavailable_history(from: i32, to: i32, reason: String) -> History {
    History {
        to,
        statuses: vec![StatusRun { first: from, last: to, status: Status::Unavailable { reason: 0 } }],
        reasons: HashMap::from([(0, reason)]),
        ..History::default()
    }
}

fn read_history(recording: &Recording, run: u32, table: TableId, fields: &[&str], keys: Keys<'_>, from: i32, to: i32) -> io::Result<History> {
    if recording.predates(table.index()).is_some() { return Ok(History::default()); }
    let (first, withheld) = match recording.table_coverage(table.index(), run) {
        TableCoverage::Valid => (from, None),
        TableCoverage::WithheldAll(reason) => return Ok(unavailable_history(from, to, reason.to_owned())),
        TableCoverage::WithheldBefore { ordinal, reason } => {
            let valid = recording.run(run)?.first_hour + ordinal as i32;
            if to < valid { return Ok(unavailable_history(from, to, reason.to_owned())); }
            (from.max(valid), (from < valid).then_some(reason))
        }
    };
    let columns = match fields.iter().map(|name| recording.field(table, name)).collect::<io::Result<Vec<_>>>() {
        Ok(columns) => columns,
        Err(error) => return Ok(unavailable_history(from, to, error.to_string())),
    };
    let mut history = dashboard_query::cost::storage(|| lanes::history(recording, run, table, &columns, keys, first, to))?;
    if columns != history.columns && !history.keys.is_empty() {
        let order: Vec<_> = columns.iter().map(|column| history.columns.binary_search(column).expect("a recorded field was requested")).collect();
        for step in history.keys.iter_mut().flat_map(|(_, steps)| steps) {
            if let Some(cells) = &mut step.cells {
                *cells = order.iter().map(|&index| cells[index]).collect();
            }
        }
        history.columns = columns;
    }
    if let Some(reason) = withheld {
        let id = history.reasons.keys().copied().max().map_or(0, |max| max + 1);
        history.reasons.insert(id, reason.to_owned());
        history.statuses.insert(0, StatusRun { first: from, last: first - 1, status: Status::Unavailable { reason: id } });
    }
    Ok(history)
}

#[derive(Default)]
pub(super) struct RowHistory {
    pub(super) bucket: Option<i64>,
    entries: Vec<((u32, i32, i32), TableId, Option<Vec<u128>>, History)>,
    bytes: usize,
    #[cfg(test)]
    pub reads: usize,
}

impl RowHistory {
    pub(super) fn select(&mut self, bucket: i64) {
        if self.bucket != Some(bucket) { *self = Self { bucket: Some(bucket), ..Self::default() }; }
    }

    pub(super) fn holds(&self, stretch: (u32, i32, i32)) -> bool {
        self.entries.iter().any(|(at, ..)| *at == stretch)
    }

    pub(super) fn read(&mut self, recording: &Recording, window: (u32, i32, i32), to: i32, (table, fields, keys): (TableId, &[&str], Keys<'_>)) -> io::Result<History> {
        let (run, from, end) = window;
        let asked = match keys {
            Keys::All => None,
            Keys::These(keys) => Some(keys),
            _ => return read_history(recording, run, table, fields, keys, from, to),
        };
        let covers = |saved: &Option<Vec<u128>>| match (saved, asked) {
            (None, _) => true,
            (Some(saved), Some(asked)) => asked.iter().all(|key| saved.binary_search(key).is_ok()),
            _ => false,
        };
        if let Some((.., history)) = self.entries.iter().rev().find(|(at, id, saved, _)| *at == window && *id == table && covers(saved)) {
            return Ok(history_prefix(history, asked, to));
        }
        #[cfg(test)]
        { self.reads += 1; }
        let full = match read_history(recording, run, table, fields, keys, from, end) {
            Ok(full) => full,
            Err(_) if to < end => return read_history(recording, run, table, fields, keys, from, to),
            Err(error) => return Err(error),
        };
        let prefix = history_prefix(&full, asked, to);
        let bytes = std::mem::size_of_val(&full)
            + asked.map_or(0, std::mem::size_of_val)
            + full.columns.capacity() * std::mem::size_of::<usize>()
            + full.keys.capacity() * std::mem::size_of::<(u128, Vec<lanes::Step>)>()
            + full.keys.iter().map(|(_, steps)| steps.capacity() * std::mem::size_of::<lanes::Step>()
                + steps.iter().filter_map(|step| step.cells.as_deref()).map(std::mem::size_of_val).sum::<usize>()).sum::<usize>()
            + full.texts.capacity() * std::mem::size_of::<Box<[u8]>>()
            + full.texts.iter().map(|text| text.len()).sum::<usize>()
            + full.statuses.capacity() * std::mem::size_of::<StatusRun>()
            + full.reasons.capacity() * std::mem::size_of::<(u32, String)>()
            + full.reasons.values().map(String::capacity).sum::<usize>();
        if self.bytes.saturating_add(bytes) <= dashboard_base::products::MAX_PRODUCT_REPLY_BYTES / 2 {
            self.entries.push((window, table, asked.map(<[u128]>::to_vec), full));
            self.bytes += bytes;
        }
        Ok(prefix)
    }
}

fn history_prefix(history: &History, keys: Option<&[u128]>, to: i32) -> History {
    let key_and_steps = |(key, steps): &(u128, Vec<lanes::Step>)| {
        let end = steps.partition_point(|step| step.hour <= to);
        (end != 0).then(|| (*key, steps[..end].to_vec()))
    };
    let keys = match keys {
        Some(keys) => keys.iter().filter_map(|key| history.keys.binary_search_by_key(key, |(id, _)| *id).ok())
            .filter_map(|at| key_and_steps(&history.keys[at])).collect(),
        None => history.keys.iter().filter_map(key_and_steps).collect(),
    };
    History {
        to,
        keys,
        statuses: history.statuses.iter().take_while(|run| run.first <= to)
            .map(|run| StatusRun { last: run.last.min(to), ..*run }).collect(),
        columns: history.columns.clone(),
        texts: history.texts.clone(),
        reasons: history.reasons.clone(),
    }
}

fn decode_written_tags(recording: &Recording, run: u32, table: TableId, history: &History, orig: usize, ref_col: usize) -> io::Result<HashMap<(i32, u128), Option<Vec<u8>>>> {
    let stored = recording.run(run)?;
    let mut asks = Vec::new();
    for (key, steps) in &history.keys {
        for step in steps {
            if let Some(cells) = &step.cells
                && matches!(cells[ref_col], Cell::Number(r) if r > 0)
                && let Cell::Written(hour) = cells[orig]
                && stored.ordinal(hour).is_some()
            {
                asks.push((hour, *key));
            }
        }
    }
    asks.sort_unstable();
    asks.dedup();
    let mut out = HashMap::new();
    if asks.is_empty() { return Ok(out); }
    let column = history.columns[orig];
    let format = recording.format();
    dashboard_query::cost::storage(|| {
        lanes::at_each(recording, run, table, Some(&[column]), &asks, &mut |hour, key, row| {
            if let Some((view, slot)) = row
                && let Field::Opaque(Opaque::Encoded(bytes, _)) = table_field(table, &view, slot, column, format)?
            {
                let mut input = Decoder::new(bytes);
                input.format = format;
                out.insert((hour, key), Option::<Vec<u8>>::decode(&mut input)?);
            }
            Ok(())
        })
    })?;
    Ok(out)
}

fn resolve_original_or_own(what: &str, ref_cell: Cell, orig_cell: Cell, key: u128, own: &[u8], written: &HashMap<(i32, u128), Option<Vec<u8>>>, utf8_error: &str) -> Result<String, String> {
    let reference = match ref_cell {
        Cell::Number(value) => value,
        Cell::Missing => return Err(format!("original tag reference of {what} capture refused")),
        _ => return Err(format!("original tag reference of {what} not recorded")),
    };
    let bytes = if reference > 0 {
        match orig_cell {
            Cell::Written(hour) => match written.get(&(hour, key)) {
                Some(Some(bytes)) => bytes.as_slice(),
                Some(None) => return Err(format!("original tag of {what} capture refused")),
                None => return Err(format!("original tag of {what} not recorded")),
            },
            Cell::Missing => return Err(format!("original tag of {what} capture refused")),
            _ => return Err(format!("original tag of {what} not recorded")),
        }
    } else {
        own
    };
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| utf8_error.to_owned())
}

fn push_step<T: PartialEq>(steps: &mut Vec<(i32, T)>, hour: i32, value: T) {
    if steps.last().is_some_and(|(at, _)| *at == hour) { steps.pop(); }
    if steps.last().is_none_or(|(_, held)| *held != value) { steps.push((hour, value)); }
}

fn push_status_edges(out: &mut Vec<i32>, statuses: &[StatusRun], first: i32, last: i32) {
    for run in statuses {
        if run.last >= first && run.first <= last {
            if run.first > first { out.push(run.first); }
            if run.last < last { out.push(run.last + 1); }
        }
    }
}

fn push_reading_starts<T>(out: &mut Vec<i32>, readings: &[(i32, i32, T)], first: i32, last: i32) {
    for (start, end, _) in readings {
        if *end >= first && *start <= last && *start > first { out.push(*start); }
    }
}

fn push_tag_hours<T>(out: &mut Vec<i32>, steps: &[(i32, T)], first: i32, last: i32) {
    let start = steps.partition_point(|(h, _)| *h <= first);
    out.extend(steps[start..].iter().take_while(|(h, _)| *h <= last).map(|(h, _)| *h));
}

fn step_at<T>(steps: &[(i32, T)], hour: i32) -> Option<&T> {
    steps.partition_point(|(h, _)| *h <= hour).checked_sub(1).map(|i| &steps[i].1)
}

fn cell_number(cell: Cell) -> Option<i64> { match cell { Cell::Number(v) => Some(v), _ => None } }

fn build_slot_tags(
    recording: &Recording,
    run: u32,
    countries: &History,
    appearance: &History,
    reserved: &History,
    mode: CaptureMode,
    first: i32,
    last: i32,
) -> io::Result<HashMap<u32, Vec<(i32, Result<String, String>)>>> {
    let written_res = decode_written_tags(recording, run, TableId::ReservedCountryNaming, reserved, 1, 2)?;
    let written_app = decode_written_tags(recording, run, TableId::CountryAppearance, appearance, 0, 1)?;
    let mut by_slot: HashMap<u32, Vec<u128>> = HashMap::new();
    for (key, steps) in &countries.keys {
        for step in steps {
            if let Some(cells) = &step.cells && let Some(slot) = cell_number(cells[0]).and_then(|v| u32::try_from(v).ok()) {
                by_slot.entry(slot).or_default().push(*key);
            }
        }
    }
    let mut status_edges = Vec::new();
    push_status_edges(&mut status_edges, &countries.statuses, first, last);
    push_status_edges(&mut status_edges, &appearance.statuses, first, last);
    let mut out = HashMap::with_capacity(by_slot.len() + 1);
    let mut res_hours = vec![first];
    push_reading_starts(&mut res_hours, &reserved.readings(0), first, last);
    push_status_edges(&mut res_hours, &reserved.statuses, first, last);
    res_hours.sort_unstable();
    res_hours.dedup();
    let mut res_steps = Vec::new();
    for hour in res_hours {
        let value = match status_at(reserved, hour) {
            (Status::Complete, _) => match reserved.reading(0, hour) {
                Reading::Cells(c) => resolve_original_or_own("reserved country naming", c[2], c[1], 0, reserved.text(c[0]).unwrap_or(&[]), &written_res, "reserved original-or-own tag is not UTF-8"),
                _ => Err("reserved country naming has no singleton row for country zero".into()),
            },
            (status, reason) => Err(format!("reserved country naming is unavailable this hour: {}", status_gap(status, reason, mode))),
        };
        push_step(&mut res_steps, hour, value);
    }
    out.insert(0, res_steps);
    for (slot, mut ids) in by_slot {
        ids.sort_unstable();
        ids.dedup();
        let mut hours = vec![first];
        hours.extend_from_slice(&status_edges);
        for &id in &ids {
            for history in [countries, appearance] {
                if let Ok(at) = history.keys.binary_search_by_key(&id, |(k, _)| *k) {
                    hours.extend(history.keys[at].1.iter().filter(|s| s.hour > first && s.hour <= last).map(|s| s.hour));
                }
            }
        }
        hours.sort_unstable();
        hours.dedup();
        let mut steps = Vec::new();
        for hour in hours {
            let found = ids.iter().rev().find_map(|&id| {
                let Reading::Cells(c) = countries.reading(id, hour) else { return None };
                (cell_number(c[0]).and_then(|v| u32::try_from(v).ok()) == Some(slot)).then(|| (id, countries.text(c[1]).unwrap_or(&[])))
            });
            let value = match found {
                None => Err(if is_complete(countries, hour) { format!("country slot {slot} has no Countries row") } else { "countries is incomplete this hour: a country is unknown".to_owned() }),
                Some((id, own)) => match appearance.reading(id, hour) {
                    Reading::Cells(app) => resolve_original_or_own("country appearance", app[1], app[0], id, own, &written_app, "country original-or-own tag is not UTF-8"),
                    _ => Err(if is_complete(appearance, hour) { format!("country slot {slot} has no country appearance row") } else { "country appearance is incomplete this hour: a country is unknown".to_owned() }),
                },
            };
            push_step(&mut steps, hour, value);
        }
        out.insert(slot, steps);
    }
    Ok(out)
}

pub struct PlaceHistory {
    mode: CaptureMode,
    provinces: History,
    states: History,
    renames: History,
    links: History,
    country_statuses: Vec<StatusRun>,
    slot_tags: HashMap<u32, Vec<(i32, Result<String, String>)>>,
    with_victory_points: bool,
}

impl PlaceHistory {
    #[expect(clippy::too_many_arguments, reason = "stretch bounds, key sets and mode flags come from distinct callers")]
    pub fn load(recording: &Recording, run: u32, first: i32, last: i32, province_keys: Keys<'_>, state_keys: Keys<'_>, with_victory_points: bool, cancelled: &AtomicBool) -> io::Result<Self> {
        Self::load_with(recording, (run, first, last), (province_keys, state_keys), (with_victory_points, false), cancelled,
            |table, fields, keys| read_history(recording, run, table, fields, keys, first, last))
    }

    pub(super) fn load_with(recording: &Recording, (run, first, last): (u32, i32, i32), (province_keys, state_keys): (Keys<'_>, Keys<'_>), (with_victory_points, warm): (bool, bool), cancelled: &AtomicBool, mut read: impl FnMut(TableId, &[&str], Keys<'_>) -> io::Result<History>) -> io::Result<Self> {
        check_cancel(cancelled)?;
        let mode = CaptureMode::from_byte(recording.root_identity().capture_mode)?;
        let want_provinces = !matches!(province_keys, Keys::These([]));
        let province_fields: &[&str] = if with_victory_points { &["state", "victory_points"] } else { &["state"] };
        let provinces = if want_provinces { read(TableId::Provinces, province_fields, province_keys)? } else { History::default() };
        check_cancel(cancelled)?;
        let renames = if want_provinces { read(TableId::ProvinceNames, &["name"], province_keys)? } else { History::default() };
        check_cancel(cancelled)?;
        let states = match (want_provinces, state_keys) {
            (false, Keys::These([])) => History::default(),
            (_, Keys::These(asked)) => {
                let mut needed: Vec<u128> = asked.to_vec();
                for (_, steps) in &provinces.keys {
                    for step in steps {
                        if let Some(c) = &step.cells && let Some(s) = cell_number(c[0]).and_then(|v| u32::try_from(v).ok()) && s != 0 {
                            needed.push(u128::from(s));
                        }
                    }
                }
                needed.sort_unstable();
                needed.dedup();
                if needed.is_empty() { History::default() } else { read(TableId::States, STATE_FIELDS, Keys::These(&needed))? }
            }
            _ => read(TableId::States, STATE_FIELDS, Keys::All)?,
        };
        check_cancel(cancelled)?;
        let (links, country_statuses, slot_tags) = if want_provinces {
            let nonzero_id = |step: &lanes::Step| u32::try_from(cell_number(step.cells.as_deref()?[0])?).ok().filter(|&id| id != 0);
            let selected = |history: &History| history.keys.iter().flat_map(|(_, steps)| steps.iter().filter_map(nonzero_id)).collect::<BTreeSet<_>>();
            let owners: Vec<_> = selected(&states).into_iter().map(u128::from).collect();
            let links = (!owners.is_empty()).then(|| read(TableId::CountryLinks, &["canonical_slot"], Keys::These(&owners))).transpose()?.unwrap_or_default();
            let slots = selected(&links);
            let mut countries = (!slots.is_empty()).then(|| read(TableId::Countries, &["canonical_slot", "tag"], Keys::All)).transpose()?.unwrap_or_default();
            countries.keys.retain(|(_, steps)| steps.iter().filter_map(nonzero_id).any(|slot| slots.contains(&slot)));
            let ids: Vec<_> = countries.keys.iter().map(|(id, _)| *id).collect();
            let appearance = (!ids.is_empty()).then(|| read(TableId::CountryAppearance, &["original_tag", "original_tag_reference"], Keys::These(&ids))).transpose()?.unwrap_or_default();
            let reserved = read(TableId::ReservedCountryNaming, &["tag", "original_tag", "original_tag_reference"], Keys::All)?;
            let slot_tags = if warm { HashMap::new() } else { build_slot_tags(recording, run, &countries, &appearance, &reserved, mode, first, last)? };
            (links, countries.statuses, slot_tags)
        } else {
            (History::default(), Vec::new(), HashMap::new())
        };
        Ok(Self { mode, provinces, states, renames, links, country_statuses, slot_tags, with_victory_points })
    }

    fn countries_complete(&self, hour: i32) -> bool {
        let at = self.country_statuses.partition_point(|run| run.last < hour);
        self.country_statuses.get(at).is_some_and(|run| run.first <= hour && matches!(run.status, Status::Complete))
    }

    fn victory_point_tag_at(&self, id: u32, state: u32, hour: i32) -> Result<String, String> {
        let slot = if state == 0 { 0 } else { self.state_owner_slot_at(id, state, hour)? };
        if let Some(tag) = self.slot_tags.get(&slot).and_then(|steps| step_at(steps, hour)) {
            return tag.clone();
        }
        Err(if slot == 0 {
            "reserved country naming has no singleton row for country zero".into()
        } else if self.countries_complete(hour) {
            format!("country slot {slot} has no Countries row")
        } else {
            "countries is incomplete this hour: a country is unknown".to_owned()
        })
    }

    fn rename_at(&self, id: u32, hour: i32) -> Result<Option<&[u8]>, String> {
        match self.renames.reading(u128::from(id), hour) {
            Reading::Cells(c) => Ok(Some(self.renames.text(c[0]).unwrap_or(&[]))),
            Reading::Absent => Ok(None),
            Reading::Unread { status, reason } => Err(format!("GetName of province {id}: province names {}", status_gap(status, reason, self.mode))),
        }
    }

    fn state_owner_slot_at(&self, id: u32, state: u32, hour: i32) -> Result<u32, String> {
        let Reading::Cells(c) = self.states.reading(u128::from(state), hour) else {
            let (status, reason) = status_at(&self.states, hour);
            return Err(format!("province {id}: state {state} has no States row: {}", status_gap(status, reason, self.mode)));
        };
        let owner = cell_number(c[0]).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        if owner == 0 { return Ok(0); }
        match self.links.reading(u128::from(owner), hour) {
            Reading::Cells(link) => match cell_number(link[0]).and_then(|v| u32::try_from(v).ok()) {
                Some(slot) => Ok(slot),
                None => Err(format!("province {id}: state owner's link {owner} resolves to no country")),
            },
            Reading::Absent => Err(format!("province {id}: state owner's link {owner} resolves to no country")),
            Reading::Unread { status, reason } => Err(format!("province {id}: state owner's link {owner} is unknown: {}", status_gap(status, reason, self.mode))),
        }
    }

    fn state_display_cells(&self, content: &Content, id: u32, cells: &[Cell], scoped: impl FnOnce(u32, &str) -> Option<Result<String, String>>) -> Result<String, String> {
        let row = StateRow { id, custom_name: self.states.text(cells[1]).unwrap_or(&[]).to_vec(), owner_name: self.states.text(cells[2]).unwrap_or(&[]).to_vec(), ..StateRow::default() };
        let states = content.cat.cached_states();
        let names = states.as_ref().map(|states| &states.names).map_err(String::as_str);
        let plain = scripted_text::state_display_name(&row, &content.cat, names, scripted_text::StateNameUsage::Place);
        if plain.is_err() && row.custom_name.is_empty() && row.owner_name.is_empty()
            && let Some(Ok(key)) = names.ok().and_then(|names| names.get(&id))
            && let Some(result) = scoped(id, key)
        {
            return result;
        }
        plain
    }

    fn state_display_at(&self, content: &Content, id: u32, hour: i32, absent: impl FnOnce() -> String, scoped: impl FnOnce(u32, &str) -> Option<Result<String, String>>) -> Result<String, String> {
        if matches!(status_at(&self.states, hour).0, Status::NotRecorded) {
            return Err(not_recorded(hour, TableId::States, &self.mode));
        }
        let Reading::Cells(c) = self.states.reading(u128::from(id), hour) else { return Err(absent()); };
        self.state_display_cells(content, id, c, scoped)
    }

    pub fn state_reading(&self, content: &Content, id: u32, hour: i32, scoped: impl FnOnce(u32, &str) -> Option<Result<String, String>>) -> Result<Option<Name>, String> {
        match reading_witness(TableId::States, self.states.reading(u128::from(id), hour)) {
            Witness::Row(c) => Ok(Some(Name::of(self.state_display_cells(content, id, c, scoped)))),
            Witness::Absent => Ok(None),
            Witness::NotRecorded(reason) => Err(reason),
        }
    }

    pub fn province_reading(&self, content: &Content, id: u32, mode: ProvinceMode, hour: i32, scoped: impl FnOnce(u32, &str) -> Option<Result<String, String>>) -> Result<Option<Name>, String> {
        match reading_witness(TableId::Provinces, self.provinces.reading(u128::from(id), hour)) {
            Witness::Row(c) => {
                let state = cell_number(c[0]).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                Ok(Some(resolve_province(content, id, state, mode, self.rename_at(id, hour), || self.victory_point_tag_at(id, state, hour), |s| self.state_display_at(content, s, hour, || format!("state {s}: Not in this recording"), scoped))))
            }
            Witness::Absent => Ok(None),
            Witness::NotRecorded(reason) => Err(reason),
        }
    }

    pub fn province_name(&self, content: &Content, id: u32, mode: ProvinceMode, hour: i32) -> Name {
        match self.province_reading(content, id, mode, hour, |_, _| None) {
            Ok(Some(name)) => name,
            Ok(None) => Name::missing(format!("Not in this recording: province {id} at this hour")),
            Err(reason) => Name::missing(reason),
        }
    }

    pub fn land_battle_title(&self, content: &Content, province: u32, hour: i32) -> Result<String, String> {
        content.land_battle_title(self.province_name(content, province, ProvinceMode::Land, hour))
    }

    pub fn naval_battle_title(&self, content: &Content, province: u32, port_strike: bool, hour: i32) -> Result<String, String> {
        let region = content.province_region_id(province)?;
        let region_name = content.region(region)?;
        if port_strike { return content.naval_battle_title_text(&region_name, true); }
        let naval = content.naval().map_err(str::to_owned)?;
        let [factor, cutoff] = *naval.defines.as_ref().map_err(|r| format!("the NNavy battle name defines are unavailable ({r})"))?;
        let candidates = naval.candidates.get(&region).map_or(&[][..], Vec::as_slice);
        let position = |id: u32| naval.geometry.centroids.get(&id).copied().unwrap_or([0, 0]);
        let victory_points = |id: u32| -> Result<i32, String> {
            if !self.with_victory_points { return Err("victory points were not loaded for naval battle naming".into()); }
            match self.provinces.reading(u128::from(id), hour) {
                Reading::Cells(c) => c.get(1).copied().and_then(cell_number).and_then(|v| i32::try_from(v).ok()).ok_or_else(|| format!("province {id}'s victory points are unavailable")),
                Reading::Absent => Err(format!("province {id} has no Provinces row")),
                Reading::Unread { .. } => Err(format!("the Provinces table is incomplete this hour, so province {id}'s victory points are unknown")),
            }
        };
        let picker = Picker { width: naval.geometry.width, factor, cutoff, position: &position, victory_points: &victory_points };
        let place = match picker.pick(province, candidates)? {
            None => region_name,
            Some(picked) => {
                let resolved = self.province_name(content, picked, ProvinceMode::Naval, hour);
                resolved.name.ok_or_else(|| resolved.missing.unwrap_or_else(|| format!("province {picked}: name unavailable")))?
            }
        };
        content.naval_battle_title_text(&place, false)
    }

    pub fn state_change_hours(&self, id: u32, first: i32, last: i32) -> Vec<i32> {
        let mut hours = vec![first];
        push_reading_starts(&mut hours, &self.states.readings(u128::from(id)), first, last);
        hours.sort_unstable();
        hours.dedup();
        hours
    }

    pub fn province_change_hours(&self, id: u32, first: i32, last: i32) -> Vec<i32> {
        let mut hours = vec![first];
        let push_slot = |hours: &mut Vec<i32>, slot: u32, a: i32, b: i32| {
            if let Some(steps) = self.slot_tags.get(&slot) {
                push_tag_hours(hours, steps, a, b);
            } else {
                push_status_edges(hours, &self.country_statuses, a, b);
            }
        };
        push_reading_starts(&mut hours, &self.renames.readings(u128::from(id)), first, last);
        for (p_start, p_end, p_read) in self.provinces.readings(u128::from(id)) {
            let (p_first, p_last) = (p_start.max(first), p_end.min(last));
            if p_first > p_last { continue; }
            if p_first > first { hours.push(p_first); }
            let Reading::Cells(p_cells) = p_read else { continue };
            let state = cell_number(p_cells[0]).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            if state == 0 {
                push_slot(&mut hours, 0, p_first, p_last);
                continue;
            }
            for (s_start, s_end, s_read) in self.states.readings(u128::from(state)) {
                let (s_first, s_last) = (s_start.max(p_first), s_end.min(p_last));
                if s_first > s_last { continue; }
                if s_first > first { hours.push(s_first); }
                let Reading::Cells(s_cells) = s_read else { continue };
                let owner = cell_number(s_cells[0]).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                if owner == 0 {
                    push_slot(&mut hours, 0, s_first, s_last);
                    continue;
                }
                for (l_start, l_end, l_read) in self.links.readings(u128::from(owner)) {
                    let (l_first, l_last) = (l_start.max(s_first), l_end.min(s_last));
                    if l_first > l_last { continue; }
                    if l_first > first { hours.push(l_first); }
                    if let Reading::Cells(link) = l_read && let Some(slot) = cell_number(link[0]).and_then(|v| u32::try_from(v).ok()) {
                        push_slot(&mut hours, slot, l_first, l_last);
                    }
                }
            }
        }
        hours.sort_unstable();
        hours.dedup();
        hours
    }
}
