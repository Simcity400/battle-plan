
use viewer_layer_core::rebuilt::{Build, Seen, Whole};
use engine_executable::executable_tables::Tables;
use map_display_data::{
    landscape::Regions,
    province_field::{ProvinceField, Storage},
    retained::Retained,
};
use replay_data::rows::TableId;
use replay_frame::frame::DecodedFrame;
use std::{
    io,
    sync::{Arc, Mutex},
};
use viewer_foundation::perspective::Choice;
use viewer_runtime::presentation::Continuity;
use viewer_borders::{border_pass::ClassPlan, demilitarized::{Zone, ZonePlan}};
use viewer_map_modes::map_mode::{MapMode, Stock};

pub const TABLES: &[TableId] = &[
    TableId::Provinces,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::Subjects,
    TableId::Autonomy,
    TableId::StateFlags,
    TableId::States,
];

pub struct Fill {
    pub mode: MapMode,
    pub regions: Arc<Regions>,
    pub zone: Retained<Zone>,
    pub classes: ClassPlan,
    pub highlighted: ZonePlan,
}

impl Fill {
    fn bytes(&self) -> usize {
        let regions = &self.regions;
        (regions.country_of.len() + regions.owner_of.len() + regions.state_of.len()) * 4
    }
}

pub trait Paint: Send + Sync + 'static {
    fn tables(&self) -> Option<Tables>;
    fn row_tables(&self, mode: Stock) -> Vec<TableId>;
    fn paint(&self, frame: &DecodedFrame, choice: &Choice, mode: &MapMode, selected: Option<u32>) -> io::Result<Fill>;
}


fn draws(mode: &MapMode) -> bool {
    matches!(mode, MapMode::Stock(stock) if *stock != Stock::Default)
}

fn asked(view: &viewer_layer_core::layers::View) -> (MapMode, Option<u32>) {
    let selected = view.selected.filter(|_| view.map_mode == MapMode::Stock(Stock::Diplomacy));
    (view.map_mode.clone(), selected)
}

pub struct Fills {
    source: Arc<dyn Paint>,
    asked: Mutex<(MapMode, Option<u32>)>,
}

impl Fills {
    pub fn asked(&self) -> (MapMode, Option<u32>) {
        self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl Build for Fills {
    type Product = Fill;

    fn row_tables(&self, view: &viewer_layer_core::layers::View) -> Vec<TableId> {
        match view.map_mode {
            MapMode::Stock(mode) => self.source.row_tables(mode),
            MapMode::Scripted(_) => Vec::new(),
        }
    }

    fn hold_picture(&self) -> bool {
        true
    }

    fn tables(&self) -> Option<Tables> {
        self.source.tables()
    }

    fn build(&self, frame: &DecodedFrame, (choice, _): &Seen) -> io::Result<Fill> {
        let (mode, selected) = self.asked();
        if !draws(&mode) {
            return Err(io::Error::other(format!("The mode fills layer does not draw map mode {}", mode.token())));
        }
        self.source.paint(frame, choice, &mode, selected)
    }

    fn viewed(&self, view: &viewer_layer_core::layers::View) -> bool {
        let next = asked(view);
        let mut asked = self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *asked == next {
            return false;
        }
        *asked = next;
        true
    }

    fn install(&self, gpu: &mut viewer_layer_core::layers::Gpu<'_>, fill: Option<&Fill>, _continuity: Continuity) -> io::Result<Option<Fill>> {
        match fill {
            Some(fill) => {
                gpu.scene.set_political(gpu.device, gpu.queue, &fill.regions)?;
                gpu.scene.set_border_plans(gpu.device, &fill.classes, &fill.highlighted)?;
            }
            None if draws(&self.asked().0) => {
                let map = &gpu.content.map;
                let count = map.definitions.last_key_value().map_or(0, |(&id, _)| id as usize) + 1;
                let field = || ProvinceField(Arc::new(Storage::Dense(Arc::new(vec![0; count]))));
                let regions = Arc::new(Regions {
                    country_of: field(),
                    owner_of: field(),
                    state_of: field(),
                    country_colors: Default::default(),
                    warnings: Vec::new(),
                    strategic_attributes: Default::default(),
                    mode: None,
                });
                let zone = Zone::new(map, &regions, &Default::default());
                let meshes = &gpu.content.border_meshes;
                gpu.scene.set_political(gpu.device, gpu.queue, &regions)?;
                gpu.scene.set_border_plans(gpu.device, &ClassPlan::new(meshes, &regions, &zone), &ZonePlan::new(meshes, &regions, &zone))?;
            }
            None => {}
        }
        Ok(None)
    }
}

pub type ModeFills = Whole<Fills>;



pub fn lines(fills: &ModeFills) -> Vec<String> {
    if !fills.drawn() {
        return Vec::new();
    }
    match fills.product().filter(|_| viewer_layer_core::layers::Layer::mode_ready(fills)).map(|built| &built.product) {
        Some(Ok(fill)) => fill.regions.warnings.clone(),
        Some(Err(why)) => vec![format!("map mode fill: {why}")],
        None => vec![format!("map mode fill: loading, no picture of map mode {} at the head yet", fills.build().asked().0.token())],
    }
}

pub fn picture(fills: &ModeFills) -> Option<(Arc<Regions>, Retained<Zone>, replay_data::Hour)> {
    let built = fills.product().filter(|_| fills.drawn())?;
    let fill = built.product.as_ref().ok()?;
    Some((Arc::clone(&fill.regions), fill.zone.clone(), built.hour))
}

fn drawn(_: &Fills, view: &viewer_layer_core::layers::View) -> bool {
    draws(&view.map_mode)
}

pub fn new(source: Arc<dyn Paint>, timed: bool) -> io::Result<ModeFills> {
    let fills = Fills { source, asked: Mutex::new((MapMode::default(), None)) };
    Ok(Whole::new("mode_fills", fills, drawn, Fill::bytes, TABLES, timed)?.exact())
}

#[cfg(test)]
mod tests;
