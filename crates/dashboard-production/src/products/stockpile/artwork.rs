
use dashboard_base::products::{Country, validate_country};
use super::{Content, archetype_of};
use dashboard_base::{Source, catalogue::{self, Catalogue}, products::{LoadContext, at_hour::{At, AtHour, Edits, Keys, Owner, Read, ReadsAt, before, field_columns, warnings_through}}};
use dashboard_world::products::equipment_names as names;
use dashboard_base::products::{
    artwork::ICON_EDGE,
    equipment_icon::{self, IconChoice, IconContent, IconHour},
};
use dashboard_text::{
    metadata::link_slot,
    variant_names::{Tags, VariantOrder, compare_variants, raw_variant, variant_names, variant_order},
};
use replay_data::rows::{TableId, tables};
use replay_frame::frame::DecodedFrame;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, Mutex, PoisonError, atomic::AtomicBool},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

const TABLES: [TableId; 14] = [
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::ReservedCountryNaming,
    TableId::PoliticalParties,
    TableId::Politics,
    TableId::Factions,
    TableId::FactionMembers,
    TableId::States,
    TableId::LexicalTokens,
    TableId::VariantDefinitions,
    TableId::VariantUpgrades,
    TableId::AvailableEquipmentVariants,
    TableId::SimulationFeatures,
];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StockpileArtworkParams {
    pub branch_id: String,
    pub country: Country,
    #[doc = " Designs that need their own pictures, instead of a type's best."]
    #[serde(default)]
    pub variants: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StockpileArt {
    #[doc = " `type:<archetype>` or `variant:<handle>`."]
    pub key: String,
    pub variant: String,
    pub name: Option<String>,
    #[doc = " The picture; null with `reason` where none is drawn."]
    pub icon: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StockpileArtworkReply {
    pub tag: String,
    pub entries: Vec<StockpileArt>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(StockpileArtworkReply);

#[derive(Default)]
struct Held {
    catalogue: Option<Arc<Catalogue>>,
    icons: Option<Result<Arc<IconContent>, String>>,
    drawn: BTreeMap<IconChoice, (Result<Option<String>, String>, catalogue::OwnRefusals)>,
}

pub struct ArtContext {
    content: Content,
    held: Option<Arc<Mutex<Held>>>,
}

fn held(source: &Source, catalogue: &Arc<Catalogue>) -> Arc<Mutex<Held>> {
    let state = source.state::<Held>();
    {
        let mut held = state.lock().unwrap_or_else(PoisonError::into_inner);
        if !held.catalogue.as_ref().is_some_and(|kept| Arc::ptr_eq(kept, catalogue)) {
            *held = Held { catalogue: Some(Arc::clone(catalogue)), ..Held::default() };
        }
    }
    state
}

fn best_variants(frame: &DecodedFrame, units: &content_reader_key_names::equipment::Database, owner: Owner) -> Result<BTreeMap<String, u64>, String> {
    if let Some(why) = &units.lost {
        return Err(format!("equipment database is refused: {why}"));
    }
    let links = frame.complete::<tables::CountryLinks>().map_err(|why| why.to_string())?;
    let lexical = frame.complete::<tables::LexicalTokens>().map_err(|why| why.to_string())?;
    let definitions = frame.complete::<tables::VariantDefinitions>().map_err(|why| why.to_string())?;
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut candidates = Vec::new();
    for row in frame.complete::<tables::AvailableEquipmentVariants>().map_err(|why| why.to_string())? {
        if row.roster_kind != 2 {
            continue;
        }
        match link_slot(links, row.country) {
            Some(slot) if slot == owner.slot => candidates.push(row),
            Some(_) => {}
            None => *failures.entry(format!("best-candidate country link {} is not recorded", row.country)).or_default() += 1,
        }
    }
    candidates.sort_by_key(|row| row.ordinal);
    let mut best: BTreeMap<String, (u64, VariantOrder)> = BTreeMap::new();
    for candidate in candidates {
        let operands = (|| {
            let at = definitions.binary_search_by_key(&candidate.variant, |row| row.id).map_err(|_| format!("best candidate {} has no recorded definition", candidate.variant))?;
            let definition = &definitions[at];
            let token = lexical
                .binary_search_by_key(&definition.equipment_token, |row| row.token)
                .ok()
                .and_then(|at| std::str::from_utf8(&lexical[at].literal).ok())
                .ok_or_else(|| format!("best candidate {} has an unspelled equipment token", candidate.variant))?;
            let index = units.get(token).ok_or_else(|| format!("best candidate {} names equipment {token:?} absent from the recorded content", candidate.variant))?;
            let rank = variant_order(units, index, definition, frame)?;
            let key = archetype_of(units, token)?.0;
            Ok::<_, String>((key, rank))
        })();
        let (key, rank) = match operands {
            Ok(operands) => operands,
            Err(why) => {
                *failures.entry(why).or_default() += 1;
                continue;
            }
        };
        let replaces = best.get(&key).is_none_or(|(_, previous)| compare_variants(owner.id as i32, owner.slot as i32, rank, *previous).is_lt());
        if replaces {
            best.insert(key, (candidate.variant, rank));
        }
    }
    if !failures.is_empty() {
        return Err(failures.into_iter().map(|(why, count)| format!("{count} refused: {why}")).collect::<Vec<_>>().join("; "));
    }
    Ok(best.into_iter().map(|(key, (variant, _))| (key, variant)).collect())
}

fn picture(catalogue: &Arc<Catalogue>, state: &Mutex<Held>, choice: IconChoice, id: u64) -> Result<Option<String>, String> {
    if let Some((drawn, noted)) = state.lock().unwrap_or_else(PoisonError::into_inner).drawn.get(&choice) {
        catalogue::credit(noted);
        return drawn.clone();
    }
    let (made, noted) = catalogue::capture(|| catalogue::draw(
        || format!("equipment icon of design {id}"),
        catalogue,
        |shown| match choice.shown(shown)? {
            Some(icon) => equipment_icon::image(shown, &icon, ICON_EDGE).map(Some).map_err(|error| error.to_string()),
            None => Ok(None),
        },
    )
    .get()
    .clone());
    catalogue::credit(&noted);
    state.lock().unwrap_or_else(PoisonError::into_inner).drawn.insert(choice, (made.clone(), noted));
    made
}

fn reply(frame: &DecodedFrame, params: &StockpileArtworkParams, tag: String, owner: Owner, context: &ArtContext, cancelled: &AtomicBool) -> io::Result<StockpileArtworkReply> {
    let mut reply = StockpileArtworkReply { tag, entries: Vec::new(), warnings: Vec::new() };
    let (catalogue, units, state) = match (&context.content.catalogue, &context.content.units, &context.held) {
        (Ok(catalogue), Ok(units), Some(state)) => (catalogue, units, state),
        (Err(why), _, _) | (_, Err(why), _) => {
            reply.warnings.push(format!("Stockpile artwork is unavailable: {why}"));
            return Ok(reply);
        }
        (_, _, None) => {
            reply.warnings.push("Stockpile artwork is unavailable: no catalogue is bound".to_owned());
            return Ok(reply);
        }
    };
    let icons = {
        let kept = state.lock().unwrap_or_else(PoisonError::into_inner).icons.clone();
        match kept {
            Some(icons) => icons,
            None => {
                let loaded = IconContent::with_equipment(Arc::clone(catalogue), Some(Arc::clone(units))).map(Arc::new).map_err(|error| format!("Equipment icons are unavailable: {error}"));
                state.lock().unwrap_or_else(PoisonError::into_inner).icons = Some(loaded.clone());
                loaded
            }
        }
    };
    let icons = match icons {
        Ok(icons) => icons,
        Err(why) => {
            reply.warnings.push(why);
            return Ok(reply);
        }
    };
    reply.warnings.extend(icons.warnings());
    let mut requested: BTreeMap<String, u64> = match best_variants(frame, units, owner) {
        Ok(best) => best.into_iter().map(|(key, id)| (format!("type:{key}"), id)).collect(),
        Err(why) => {
            reply.warnings.push(format!("1 best-design selection refused: {why}"));
            BTreeMap::new()
        }
    };
    for id in &params.variants {
        requested.insert(format!("variant:{id}"), id.parse().map_err(|_| dashboard_core::invalid("Invalid stockpile artwork design"))?);
    }
    let tables = (frame.complete::<tables::Countries>(), frame.complete::<tables::CountryLinks>(), frame.complete::<tables::LexicalTokens>(), frame.complete::<tables::VariantDefinitions>());
    let (Ok(countries), Ok(links), Ok(lexical), Ok(definitions)) = tables else {
        reply.warnings.push("Stockpile artwork is unavailable at this hour: the designs or their names were not read.".to_owned());
        return Ok(reply);
    };
    let icon_hour = match IconHour::read(frame, &icons) {
        Ok(hour) => hour,
        Err(error) => {
            reply.warnings.push(format!("Stockpile artwork is unavailable at this hour: {error}"));
            return Ok(reply);
        }
    };
    let tags = Tags::new(countries, links, frame.complete::<tables::CountryAppearance>().ok());
    for (key, id) in requested {
        dashboard_text::metadata::check_cancel(cancelled)?;
        let Ok(at) = definitions.binary_search_by_key(&id, |row| row.id) else {
            reply.warnings.push(format!("Stockpile artwork design {id} has no definition"));
            reply.entries.push(StockpileArt { key, variant: id.to_string(), name: None, icon: None, reason: Some("The design has no definition at this hour.".to_owned()) });
            continue;
        };
        let definition = &definitions[at];
        let raw = raw_variant(definition, frame.hour, &tags, lexical);
        let name = variant_names(&raw, Some(catalogue.as_ref() as &dyn dashboard_text::variant_names::Texts)).0;
        let choice = equipment_icon::icon_choice(&icons, &icon_hour, definition, &raw.token);
        let (icon, reason) = match picture(catalogue, state, choice, id) {
            Ok(Some(icon)) => (Some(icon), None),
            Ok(None) => (None, Some("The native icon selector found no picture in the recorded content".to_owned())),
            Err(why) => (None, Some(why)),
        };
        if let Some(why) = &reason {
            reply.warnings.push(format!("Stockpile artwork {key}, design {id}: {why}"));
        }
        reply.entries.push(StockpileArt { key, variant: id.to_string(), name, icon, reason });
    }
    Ok(reply)
}

pub struct StockpileArtwork;

impl AtHour for StockpileArtwork {
    const KEY: &'static str = "stockpile-artwork";
    const TITLE: &'static str = "Stockpile artwork";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    type Params = StockpileArtworkParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, variants];
    type Reply = StockpileArtworkReply;
    type Context = ArtContext;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![StockpileArtworkParams::decl(), StockpileArt::decl(), StockpileArtworkReply::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid branch")?;
        validate_country(&params.country)?;
        if params.variants.iter().any(|id| id.parse::<u64>().is_err()) {
            return Err("Invalid stockpile artwork design");
        }
        Ok(())
    }

    fn owner(params: &Self::Params) -> Option<&Country> {
        Some(&params.country)
    }

    fn tables(_params: &Self::Params) -> Vec<TableId> {
        TABLES.to_vec()
    }

    fn reads_in(params: &Self::Params, owner: Option<Owner>, stage: usize, frame: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        let Some(owner) = owner else { return Vec::new(); };
        let (Ok(_), Ok(units), Some(_)) = (&at.context.content.catalogue, &at.context.content.units, &at.context.held) else {
            return Vec::new();
        };
        let best = units.lost.is_none();
        match stage {
            0 => names::WHOLE.iter().copied().map(Read::all).collect(),
            1 => {
                let Ok(links) = frame.complete::<tables::CountryLinks>() else { return Vec::new(); };
                let mut reads = vec![names::capital_state_read(frame)];
                if best {
                    let mut ranges = Vec::new();
                    let mut first = 0u128;
                    for link in links {
                        let base = u128::from(link.id) << 34;
                        if first < base {
                            ranges.push((first, base - 1));
                        }
                        if link.canonical_slot == owner.slot {
                            let chosen = base | (2u128 << 32);
                            ranges.push((chosen, chosen | u128::from(u32::MAX)));
                        }
                        first = base + (1u128 << 34);
                    }
                    if first < (1u128 << 66) {
                        ranges.push((first, (1u128 << 66) - 1));
                    }
                    reads.push(Read { table: TableId::AvailableEquipmentVariants, keys: Keys::Ranges(ranges) });
                }
                reads
            }
            2 => {
                let Ok(links) = frame.complete::<tables::CountryLinks>() else { return Vec::new(); };
                let candidates: Vec<u64> = if best {
                    frame
                        .complete::<tables::AvailableEquipmentVariants>()
                        .into_iter()
                        .flatten()
                        .filter(|row| row.roster_kind == 2 && link_slot(links, row.country) == Some(owner.slot))
                        .map(|row| row.variant)
                        .collect()
                } else {
                    Vec::new()
                };
                let variants = candidates.iter().copied().chain(params.variants.iter().filter_map(|id| id.parse::<u64>().ok()));
                let mut reads = vec![Read::these(TableId::VariantDefinitions, variants.map(u128::from))];
                if best {
                    let upgrades: BTreeSet<(u128, u128)> = candidates
                        .iter()
                        .map(|&id| {
                            let lo = u128::from(id) << 32;
                            (lo, lo | u128::from(u32::MAX))
                        })
                        .collect();
                    reads.push(Read { table: TableId::VariantUpgrades, keys: Keys::Ranges(upgrades.into_iter().collect()) });
                    reads.push(Read::all(TableId::SimulationFeatures));
                }
                reads
            }
            3 => names::spelling_reads(frame, names::variant_tokens(frame).collect(), true),
            _ => Vec::new(),
        }
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        match table {
            TableId::Factions => Some(Vec::new()),
            TableId::VariantUpgrades => Some(field_columns(table, &["level"])),
            TableId::SimulationFeatures => Some(field_columns(table, &["dlc_mask"])),
            _ => names::naming_columns(table),
        }
    }

    fn plan_changes(stage: usize, edits: &Edits, _: &DecodedFrame, _: &ReadsAt<'_, Self::Context>) -> bool {
        let changed = |table| edits.fields(table, u64::MAX);
        match stage {
            1 => changed(TableId::CountryLinks) || changed(TableId::Politics),
            2 => changed(TableId::CountryLinks) || changed(TableId::AvailableEquipmentVariants),
            3 => changed(TableId::VariantDefinitions) || changed(TableId::States),
            _ => false,
        }
    }

    fn valid_through(frame: &DecodedFrame, _: &Self::Params, at: &At<'_, Self::Context>) -> i32 {
        before(at.holders.next_change(frame.hour))
    }

    fn valid_reply_through(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>, reply: &Self::Reply, _: bool) -> i32 {
        Self::valid_through(frame, params, at).min(warnings_through(frame.hour, reply.warnings.iter().map(String::as_str)))
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        let content = Content::load(source, load)?;
        let held = content.catalogue.as_ref().ok().map(|catalogue| held(source, catalogue));
        Ok(ArtContext { content, held })
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        let owner = at.owner.ok_or_else(|| dashboard_core::refused("Stockpile artwork country is unavailable at this hour"))?;
        reply(frame, params, at.holders.name(&params.country, at.hour), owner, at.context, at.cancelled)
    }
}

#[cfg(test)]
#[test]
fn cached_stockpile_pictures_keep_their_overlay_notes() {
    use dashboard_base::test_fixtures::visual_mods::{BLUE, Machine, RED, first_pixel, png, select};

    let machine = Machine::new("stockpile_picture_notes");
    machine.write("interface/fixture.gfx", b"spriteTypes={spriteType={name=GFX_fixture_stockpile textureFile=gfx/fixture_stockpile.png}}");
    machine.write("gfx/fixture_stockpile.png", &png(RED));
    machine.write_mod("fixture_art", "gfx/fixture_stockpile.png", &png(BLUE));
    let (source, mut options) = machine.open();
    select(&source, &mut options, &["fixture_art"]);
    let catalogue = catalogue::catalogue(&source, &options).unwrap();
    let state = held(&source, &catalogue);
    let choice: IconChoice = serde_json::from_value(serde_json::json!({
        "branches":[{"assignment":[], "probes":[{"Sprite":"GFX_fixture_stockpile"}], "otherwise":{"Ok":null}}], "reasons":[],
    })).unwrap();
    let read = || {
        let (image, noted) = catalogue::capture(|| picture(&catalogue, &state, choice.clone(), 17).unwrap().unwrap());
        let mut warnings = Vec::new();
        noted.diagnostics().append_to(&mut warnings);
        (first_pixel(&image), warnings)
    };
    let first = read();
    assert_eq!(first.0, BLUE);
    assert!(first.1.iter().any(|line| line.starts_with("Artwork from visual mods") && line.contains("fixture_art")), "{first:?}");
    assert_eq!(read(), first);
}
