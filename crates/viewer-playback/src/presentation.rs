
pub fn map_lines(side: &crate::map_open::MapSide) -> [Vec<String>; 2] {
    cached_map_lines(side, &mut Default::default())
}

pub(crate) fn cached_map_lines(side: &crate::map_open::MapSide, cache: &mut crate::limitations::FactsCache) -> [Vec<String>; 2] {
    let limitations = crate::limitations::cached_lines(side, cache).unwrap_or_else(|| vec![MAP_LINES_LOADING.to_owned()]);
    let mut lines = Vec::new();
    if let Some((_, layers)) = side.parts() {
        let layer = |name: &str| layers.iter().find(|layer| layer.name() == name && side.draws(name));
        if let Some(orders) = layer("orders").and_then(|layer| layer.any().downcast_ref::<viewer_layers_forces::orders::Orders>()) {
            lines.extend(orders.warnings());
        }
        if let Some(buildings) = layer("buildings").and_then(|layer| layer.any().downcast_ref::<viewer_layers_infrastructure::buildings::BuildingsFed>()) {
            lines.extend_from_slice(buildings.warnings());
        }
        match layer("units").and_then(|layer| layer.any().downcast_ref::<viewer_layers_forces::units::Units>()) {
            Some(units) => lines.extend(units.warnings()),
            None => {
                if let Some(ships) = layer("ships").and_then(|layer| layer.any().downcast_ref::<viewer_layers_forces::ships::Ships>()) {
                    lines.extend_from_slice(ships.warnings());
                }
            }
        }
        if let Some(aircraft) = layer("aircraft").and_then(|layer| layer.any().downcast_ref::<viewer_layers_forces::aircraft::Aircraft>()) {
            lines.extend(viewer_layers_forces::aircraft::warning(aircraft));
        }
    }
    [limitations, lines]
}

pub const MAP_LINES_LOADING: &str = "Map limitations: loading, the playback head has reached no hour yet";
