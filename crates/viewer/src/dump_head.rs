
#[cfg(test)]
mod tests;

use crate::{document::Document, playhead_host::{Entry, MapSide, views::{Need, Pace, Reader as ViewReader}}};
use viewer_layer_core::{layers::{Layer, View}, playhead_host};
use viewer_layers_political::political::Political;
use viewer_layers_infrastructure::strikes::Strikes;
use map_display_data::{
    landscape::Regions,
    province_field::{ProvinceField, Storage},
    value_index::ValueIndex,
};
use replay_data::{Hour, rows::TableId};
use replay_frame::frame::DecodedFrame;
use std::{io, sync::Arc, time::Duration};
use viewer_foundation::perspective;
use viewer_map_modes::{map_mode::MapMode, political::PoliticalLayers};
use viewer_models::strike_effects::Impacts;

const LIMIT: Duration = Duration::from_secs(600);

pub(crate) const FROM_LAYERS: &[&str] = &["political fill and border keys (default map mode, recorded perspective)", "nuclear impacts"];

pub(crate) const TODAY: &[&str] = &[
    "political fill of other map modes and perspectives, the sea regions' naval attributes",
    "map mode fill",
    "limitations and warnings",
    "snow and mud",
    "buildings",
    "aircraft",
    "supply trains",
    "orders",
    "unit models and convoys",
    "fog of war",
    "names",
    "counters, labels, raids, intel and mode icons",
    "combat window",
    "railways",
    "demilitarised zone",
    "map text",
    "map-mode picker",
    "weather effects",
    "counter tooltips",
    "province hover",
    "air rosters (the roster history, from today's cursor)",
];

pub(crate) struct DumpHead {
    side: MapSide,
    _reader: ViewReader,
    frame: Arc<DecodedFrame>,
    tables: usize,
    units: String,
}

pub(crate) fn open(document: &Document) -> io::Result<DumpHead> {
    let units = playhead_host::units::of(document.playback.replay(), document.playback.branch())?;
    let name = units.name.clone();
    let entry = playhead_host::open(units, playhead_host::settings())?;
    entry.set_capture(document.capture_mode());
    document.playback.read_rosters_of(&entry);
    let content = Arc::new(viewer_layers_political::political::Content::new(Arc::clone(&document.content.map), Arc::clone(&document.border_meshes)));
    let place_in = Arc::clone(&document.content);
    let layers: Vec<Box<dyn Layer>> = vec![Box::new(Political::new(content)), Box::new(Strikes::new(Arc::new(move |province| place_in.products.impact_place(province))))];
    at(entry, layers, document.playback.hour(), document.playback.branch(), dashboard_base::map_tables::selected_tables(), name)
}

pub(crate) fn at(entry: Arc<Entry>, layers: Vec<Box<dyn Layer>>, hour: Hour, branch: replay_data::BranchId, tables: &[TableId], units: String) -> io::Result<DumpHead> {
    let timeline = entry.playhead().timeline();
    if !(timeline.first()..=timeline.last()).contains(&hour) {
        return Err(io::Error::other(format!("hour {hour} is outside the playhead's units ({} to {})", timeline.first(), timeline.last())));
    }
    let (mut side, reader) = MapSide::with_readers(entry, layers, View::default(), |store| {
        store.handle().reader_at("dump", &tables.iter().copied().map(Need::every).collect::<Vec<Need>>(), Pace::Step)
    })?;
    side.set_branch(branch);
    if let Some((name, why)) = side.refusals().next() {
        return Err(io::Error::other(format!("The {name} layer is refused: {why}")));
    }
    side.anchor(hour, false, 0.0, true);
    side.catch_up(hour)?;
    let frame = reader.wait(|frame| frame.hour == hour, LIMIT)?;
    Ok(DumpHead { side, _reader: reader, frame, tables: tables.len(), units })
}

impl DumpHead {
    pub(crate) fn frame(&self) -> Arc<DecodedFrame> {
        Arc::clone(&self.frame)
    }

    fn layer<T: 'static>(&self) -> Option<&T> {
        self.side.parts()?.1.iter().find_map(|layer| layer.any().downcast_ref::<T>())
    }

    pub(crate) fn political(&self, mode: &MapMode, choice: &perspective::Choice, today: PoliticalLayers) -> io::Result<PoliticalLayers> {
        if *mode != MapMode::default() || *choice != perspective::Choice::default() {
            return Ok(today);
        }
        let layer = self.layer::<Political>().ok_or_else(|| io::Error::other("The political layer is not drawn at the dump's hour"))?;
        if let Some(why) = layer.refused() {
            return Err(io::Error::other(format!("The political layer refuses the hour: {why}")));
        }
        let field = |values: &[u32]| ProvinceField(Arc::new(Storage::Dense(Arc::new(values.to_vec()))));
        let mut warnings = layer.warnings();
        warnings.extend(today.regions.warnings.iter().skip(warnings.len()).cloned());
        let regions = Regions {
            country_of: field(layer.country_of()),
            owner_of: field(layer.owner_of()),
            state_of: field(layer.state_of()),
            country_colors: ValueIndex::try_from_iter(layer.colours().collect::<std::collections::BTreeMap<_, _>>())?,
            warnings,
            strategic_attributes: today.regions.strategic_attributes.clone(),
            mode: today.regions.mode.clone(),
        };
        Ok(PoliticalLayers { regions: Arc::new(regions), build: today.build })
    }

    pub(crate) fn impacts(&self) -> io::Result<Impacts> {
        self.layer::<Strikes>().map(Strikes::product).ok_or_else(|| io::Error::other("The strikes layer is not drawn at the dump's hour"))
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let (now, peak, _) = self.side.entry().playhead().held();
        let held = self.side.views().held();
        let megabytes = |bytes: usize| bytes as f64 / 1e6;
        vec![
            format!("Playhead: hour {} from {}, rows at the head of {} tables", self.frame.hour, self.units, self.tables),
            format!("Playhead: from layers: {}", FROM_LAYERS.join("; ")),
            format!("Playhead: today's calls over rows at the head: {}", TODAY.join("; ")),
            format!(
                "Playhead: held {:.1} MB of cells (peak {:.1}), rows {:.1} MB, map mirrors {:.1} MB",
                megabytes(now.total),
                megabytes(peak.total),
                megabytes(held.iter().map(|(_, bytes)| bytes).sum()),
                megabytes(self.side.held().total()),
            ),
        ]
    }

    #[cfg(test)]
    pub(crate) fn declared(&self) -> usize {
        self._reader.reads()
    }
}
