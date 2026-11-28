use content_reader_key_names::key_name_products::KeyNamesExt as _;

use content_reader_layers::ResolvedLayers;
use content_reader_script::{key_names::KeyNames, modifiers, native::{self, Field, Shape}};
use content_reader_parser::parser::{self, Item};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::Arc,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriggeredIcon {
    pub text: String,
    pub trigger: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub id: Vec<u8>,
    pub name: Vec<u8>,
    pub category: Vec<u8>,
    pub cost: Vec<u8>,
    pub selectable_mission: bool,
    pub targeted: bool,
    pub state_target: bool,
    pub icons: Vec<TriggeredIcon>,
    pub icon_refusal: Option<String>,
    pub source: String,
    pub seed_component: i32,
    pub mission: bool,
    pub fixed_random_seed: bool,
    pub has_state_targets: bool,
    pub highlight_conditions: Vec<Item>,
    pub target_root_trigger: Vec<Item>,
    pub target_trigger: Vec<Item>,
    pub target_arrays: Vec<Vec<u8>>,
    pub target_states: Vec<Vec<u8>>,
    pub complete_effect: Vec<Item>,
    pub remove_effect: Vec<Item>,
    pub cancel_effect: Vec<Item>,
    pub timeout_effect: Vec<Item>,
    pub days_remove: Vec<u8>,
    pub modifier: Vec<Item>,
    pub targeted_modifier: Vec<Vec<Item>>,
    pub power_balance: bool,
    pub refused: Option<String>,
}

impl Decision {
    pub fn name_selects_state(
        &self,
        has_children: impl FnOnce(&[Item]) -> Result<bool, String>,
    ) -> Result<bool, String> {
        if self.has_state_targets {
            Ok(true)
        } else if self.highlight_conditions.is_empty() {
            Ok(false)
        } else {
            has_children(&self.highlight_conditions)
        }
    }

    pub fn name_key(
        &self,
        tag: &str,
        cosmetic: &str,
        ideology: &str,
        mut defines: impl FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let base = std::str::from_utf8(&self.name)
            .map_err(|why| format!("{} decision name: {why}", self.source))?;
        for prefix in [cosmetic, tag] {
            if prefix.is_empty() {
                continue;
            }
            let own = format!("{prefix}_{base}");
            let ruling = format!("{own}_{ideology}");
            if defines(&ruling) {
                return Ok(ruling);
            }
            if defines(&own) {
                return Ok(own);
            }
        }
        let ruling = format!("{base}_{ideology}");
        Ok(if defines(&ruling) {
            ruling
        } else {
            base.to_owned()
        })
    }

    pub fn select_icon(
        &self,
        mut evaluate: impl FnMut(&[Item]) -> Result<bool, String>,
    ) -> Result<Option<&str>, String> {
        if let Some(reason) = &self.icon_refusal {
            return Err(reason.clone());
        }
        match self.icons.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(&one.text)),
            icons => {
                for icon in icons {
                    if evaluate(&icon.trigger)? {
                        return Ok(Some(&icon.text));
                    }
                }
                Ok(Some(&icons[0].text))
            }
        }
    }

    pub fn sprite_candidates(&self, selected: Option<&str>) -> Result<Vec<String>, String> {
        let token = std::str::from_utf8(&self.id)
            .map_err(|why| format!("{} decision identifier: {why}", self.source))?;
        let mut names = vec![format!("GFX_decision_{token}")];
        if let Some(selected) = selected.filter(|text| !text.is_empty()) {
            names.push(format!("GFX_decision_{selected}"));
            names.push(selected.to_owned());
        }
        names.push("GFX_decision_unknown".to_owned());
        Ok(names)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Database {
    pub entries: BTreeMap<Vec<u8>, Decision>,
    pub categories: BTreeMap<Vec<u8>, Vec<u8>>,
    power_balance_categories: BTreeSet<Vec<u8>>,
    pub notes: Vec<String>,
    unread: Option<String>,
}

impl Database {
    pub fn roster(&self) -> Result<impl Iterator<Item = &Decision>, String> {
        if let Some(why) = &self.unread {
            return Err(why.clone());
        }
        Ok(self.entries.values())
    }

    pub fn category(&self, token: &[u8]) -> Result<Option<&[u8]>, String> {
        if let Some(why) = &self.unread {
            return Err(why.clone());
        }
        Ok(self
            .categories
            .get(&parser::c_string(token).to_ascii_lowercase())
            .map(Vec::as_slice))
    }
    pub fn decision(&self, token: &[u8]) -> Result<Option<&Decision>, String> {
        if let Some(why) = &self.unread {
            return Err(why.clone());
        }
        Ok(self
            .entries
            .get(&parser::c_string(token).to_ascii_lowercase()))
    }
}

pub fn seed_component(id: &[u8]) -> i32 {
    parser::c_string(id)
        .iter()
        .fold(0x811C9DC5_u32, |hash, byte| {
            (hash ^ u32::from(byte.to_ascii_lowercase())).wrapping_mul(0x01000193)
        }) as i32
}

#[derive(Clone, Copy)]
enum Node {
    File,
    Category,
    Decision,
    Icon,
    Highlight,
    Targets,
    Mtth,
    Modifier(modifiers::Node),
}

fn targets(field: &Field<'_>) -> Option<Shape<Node>> {
    Some(match field.keyword().as_deref()? {
        b"targets" => Shape::List,
        b"target_array"
        | b"state"
        | b"country"
        | b"targets_dynamic"
        | b"target_non_existing"
        | b"state_target" => Shape::One,
        b"target_trigger" | b"target_root_trigger" => Shape::Trigger,
        _ => return None,
    })
}

fn append(
    database: &mut Database,
    bytes: &[u8],
    source: &str,
    categories: &BTreeSet<Vec<u8>>,
    names: &KeyNames,
) {
    let mut positions = Vec::new();
    let mut undecided = Vec::new();
    let registry = &|name: &[u8]| modifiers::registered(name, Some(names));
    let read = native::read_in_effect_scope(
        bytes,
        native::Mode::KeepMark,
        native::Top::File(Node::File),
        Some(names),
        content_reader_script::scope::ScopeKind::Country.bit(),
        &mut |node, field| match node {
            Node::File => Shape::Object(Node::Category),
            Node::Category => {
                positions.push(field.key.line);
                Shape::Object(Node::Decision)
            }
            Node::Decision => {
                if let Some(shape) = targets(field) {
                    return shape;
                }
                match field.keyword().as_deref() {
                    Some(
                        b"allowed"
                        | b"available"
                        | b"visible"
                        | b"activation"
                        | b"remove_trigger"
                        | b"cancel_trigger"
                        | b"custom_cost_trigger",
                    ) => Shape::Trigger,
                    Some(
                        b"remove_effect" | b"complete_effect" | b"timeout_effect"
                        | b"cancel_effect",
                    ) => Shape::Effect,
                    Some(b"modifier") => Shape::Object(Node::Modifier(modifiers::Node::Modifier)),
                    Some(b"targeted_modifier") => {
                        Shape::Object(Node::Modifier(modifiers::Node::Targeted))
                    }
                    Some(b"ai_will_do") => Shape::Object(Node::Mtth),
                    Some(b"highlight_states") => Shape::Object(Node::Highlight),
                    Some(b"icon") => Shape::ObjectIfOpen(Node::Icon),
                    Some(
                        b"war_with_on_remove" | b"war_with_on_complete" | b"war_with_on_timeout",
                    ) => Shape::One,
                    Some(
                        b"name"
                        | b"priority"
                        | b"cost"
                        | b"alert_days"
                        | b"fire_only_once"
                        | b"desc"
                        | b"cosmetic_tag"
                        | b"days_remove"
                        | b"days_re_enable"
                        | b"days_mission_timeout"
                        | b"days_decision_timeout"
                        | b"is_good"
                        | b"fixed_random_seed"
                        | b"selectable_mission"
                        | b"custom_cost_text"
                        | b"war_with_target_on_remove"
                        | b"war_with_target_on_complete"
                        | b"war_with_target_on_timeout"
                        | b"cancel_if_not_visible"
                        | b"on_map_mode"
                        | b"power_balance"
                        | b"cosmetic_ideology",
                    ) => Shape::One,
                    _ => Shape::Skip,
                }
            }
            Node::Highlight => match field.keyword().as_deref() {
                Some(b"highlight_state_targets") => Shape::Object(Node::Targets),
                Some(b"highlight_states_trigger") => Shape::Trigger,
                Some(b"highlight_provinces") => Shape::RawList,
                Some(
                    b"highlight_only_provinces"
                    | b"highlight_color_before_active"
                    | b"highlight_color_while_active",
                ) => Shape::One,
                _ => Shape::One,
            },
            Node::Icon => match field.keyword().as_deref() {
                Some(b"text" | b"key") => Shape::One,
                Some(b"trigger") => Shape::Trigger,
                _ => Shape::Skip,
            },
            Node::Targets => targets(field).unwrap_or(Shape::Skip),
            Node::Mtth => native::mean_time_to_happen(field),
            Node::Modifier(node) => {
                modifiers::shape(node, field, registry, &mut undecided).map(Node::Modifier)
            }
        },
    );
    database
        .notes
        .extend(read.misreads.iter().map(|note| format!("{source}: {note}")));
    database
        .notes
        .extend(undecided.iter().map(|note| format!("{source}: {note}")));
    if let Some(why) = undecided.first() {
        database.unread = Some(format!("{source}: unread decisions: {why}"));
    }
    if let Some(why) = &read.unsettled {
        database.unread = Some(format!("{source}: unread decisions: {why}"));
    }
    let mut ordinal = 0;
    for category in &read.items {
        let category_key =
            parser::c_string(category.key.as_deref().unwrap_or_default()).to_ascii_lowercase();
        for item in category.value.items().unwrap_or_default() {
            let id = parser::c_string(item.key.as_deref().unwrap_or_default()).to_vec();
            let source = format!("{source}:{}", positions.get(ordinal).copied().unwrap_or(1));
            ordinal += 1;
            if !categories.contains(&category_key) {
                database.notes.push(format!(
                    "{source}: decision {} ignored: category {} is not registered",
                    String::from_utf8_lossy(&id),
                    String::from_utf8_lossy(&category_key)
                ));
                continue;
            }
            let mut decision = Decision {
                name: id.clone(),
                category: category_key.clone(),
                seed_component: seed_component(&id),
                id: id.clone(),
                source,
                ..Default::default()
            };
            let fields = item.value.items().unwrap_or_default();
            decision.power_balance = database.power_balance_categories.contains(&category_key);
            decision.targeted = fields.iter().any(|field| {
                field.keyword().is_some_and(|keyword| {
                    [
                        "target_trigger",
                        "target_root_trigger",
                        "target_array",
                        "targets",
                        "country",
                        "state",
                        "targets_dynamic",
                        "target_non_existing",
                        "state_target",
                    ]
                    .iter()
                    .any(|name| keyword.is(name))
                })
            });
            decision.state_target = parser::read_bool_field(fields, "state_target", false);
            decision.selectable_mission =
                parser::read_bool_field(fields, "selectable_mission", false);
            decision.fixed_random_seed =
                parser::read_bool_field(fields, "fixed_random_seed", false);
            for field in fields {
                if field.keyword_is("cost") {
                    decision.cost = field.value.scalar().unwrap_or_default().to_vec();
                }
                if field.keyword_is("icon") {
                    match triggered_icon(&field.value) {
                        Ok(icon) => decision.icons.push(icon),
                        Err(reason) => {
                            let reason = format!("{}: decision icon: {reason}", decision.source);
                            database.notes.push(reason.clone());
                            decision.icon_refusal.get_or_insert(reason);
                        }
                    }
                }
                if field.keyword_is("complete_effect") {
                    decision
                        .complete_effect
                        .extend_from_slice(field.value.items().unwrap_or_default());
                } else if field.keyword_is("remove_effect") {
                    decision
                        .remove_effect
                        .extend_from_slice(field.value.items().unwrap_or_default());
                } else if field.keyword_is("cancel_effect") {
                    decision
                        .cancel_effect
                        .extend_from_slice(field.value.items().unwrap_or_default());
                } else if field.keyword_is("timeout_effect") {
                    decision
                        .timeout_effect
                        .extend_from_slice(field.value.items().unwrap_or_default());
                } else if field.keyword_is("modifier") {
                    decision
                        .modifier
                        .extend_from_slice(field.value.items().unwrap_or_default());
                } else if field.keyword_is("targeted_modifier") {
                    decision
                        .targeted_modifier
                        .push(field.value.items().unwrap_or_default().to_vec());
                } else if field.keyword_is("days_remove") {
                    decision.days_remove = field.value.scalar().unwrap_or_default().to_vec();
                }
                if field.keyword_is("name") {
                    let name = field.value.read_string().unwrap_or_default();
                    decision.name = if name.is_empty() {
                        id.clone()
                    } else {
                        name.to_vec()
                    };
                }
                if field.keyword_is("days_mission_timeout")
                    || field.keyword_is("days_decision_timeout")
                {
                    decision.mission = true;
                }
            }
            let highlights = fields
                .iter()
                .filter(|field| field.keyword_is("highlight_states"))
                .flat_map(|field| field.value.items().unwrap_or_default());
            for field in highlights {
                if field.keyword_is("highlight_states_trigger") {
                    decision
                        .highlight_conditions
                        .extend_from_slice(field.value.items().unwrap_or_default());
                }
                if field.keyword_is("highlight_state_targets") {
                    decision.has_state_targets = true;
                    for target in field.value.items().unwrap_or_default() {
                        match target.keyword().map(|key| key.folded()).as_deref() {
                            Some(b"state") => decision
                                .target_states
                                .push(target.value.scalar().unwrap_or_default().to_vec()),
                            Some(b"targets") => {
                                decision.target_states.extend(
                                    target.value.items().unwrap_or_default().iter().map(|item| {
                                        item.value.scalar().unwrap_or_default().to_vec()
                                    }),
                                )
                            }
                            Some(b"target_array") => decision
                                .target_arrays
                                .push(target.value.scalar().unwrap_or_default().to_vec()),
                            Some(b"target_trigger") => decision
                                .target_trigger
                                .extend_from_slice(target.value.items().unwrap_or_default()),
                            Some(b"target_root_trigger") => decision
                                .target_root_trigger
                                .extend_from_slice(target.value.items().unwrap_or_default()),
                            Some(
                                b"country"
                                | b"targets_dynamic"
                                | b"target_non_existing"
                                | b"state_target",
                            ) => {
                                let why = decision.refused.get_or_insert_with(String::new);
                                if !why.is_empty() {
                                    why.push_str("; ");
                                }
                                why.push_str(&format!(
                                    "{}: highlight script-target {} is not ported",
                                    decision.source,
                                    String::from_utf8_lossy(
                                        target.key.as_deref().unwrap_or_default()
                                    )
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
            let key = id.to_ascii_lowercase();
            if let std::collections::btree_map::Entry::Vacant(entry) = database.entries.entry(key) {
                entry.insert(decision);
            } else {
                database.notes.push(format!(
                    "{}: duplicate decision {} ignored",
                    decision.source,
                    String::from_utf8_lossy(&id)
                ));
            }
        }
    }
}

fn triggered_icon(value: &parser::Value) -> Result<TriggeredIcon, String> {
    let mut icon = TriggeredIcon::default();
    if let Some(body) = value.items() {
        for item in body {
            if item.keyword_is("text") || item.keyword_is("key") {
                icon.text = item
                    .value
                    .read_str()
                    .map_err(|why| why.to_string())?
                    .to_owned();
            } else if item.keyword_is("trigger") {
                icon.trigger
                    .extend_from_slice(item.value.items().unwrap_or_default());
            }
        }
    } else {
        icon.text = std::str::from_utf8(parser::c_string(value.scalar().unwrap_or_default()))
            .map_err(|why| why.to_string())?
            .to_owned();
    }
    Ok(icon)
}

pub fn read_shared(layers: &ResolvedLayers) -> io::Result<Arc<Database>> {
    layers.retained_product("decision database", 1, &[], read)
}

pub fn read(layers: &ResolvedLayers) -> io::Result<Database> {
    let names = layers.key_names()?;
    let mut categories = BTreeSet::new();
    let mut database = Database::default();
    for path in layers.directory_files("common/decisions/categories", ".txt")? {
        let read = native::read(
            &path.read(layers)?,
            native::Mode::KeepMark,
            native::Top::File(false),
            Some(&names),
            &mut |category, field| {
                if !category {
                    Shape::Object(true)
                } else if matches!(
                    field.keyword().as_deref(),
                    Some(b"allowed" | b"visible" | b"available")
                ) {
                    Shape::Trigger
                } else {
                    Shape::SkipIfOpen
                }
            },
        );
        database
            .notes
            .extend(read.misreads.iter().map(|note| format!("{path}: {note}")));
        if let Some(why) = read.unsettled {
            database.unread = Some(format!("{path}: unread decision categories: {why}"));
        }
        categories.extend(read.items.iter().map(|item| {
            parser::c_string(item.key.as_deref().unwrap_or_default()).to_ascii_lowercase()
        }));
        for item in &read.items {
            let id = parser::c_string(item.key.as_deref().unwrap_or_default());
            database
                .categories
                .entry(id.to_ascii_lowercase())
                .or_insert_with(|| id.to_vec());
        }
    }
    let mut balances = BTreeMap::new();
    for path in layers.directory_files("common/bop", ".txt")? {
        let read = native::read(
            &path.read_uncovered(layers)?,
            native::Mode::KeepMark,
            native::Top::File(false),
            Some(&names),
            &mut |balance, field| {
                if !balance {
                    Shape::Object(true)
                } else if field.keyword_is("decision_category") {
                    Shape::Current
                } else {
                    Shape::SkipIfOpen
                }
            },
        );
        database
            .notes
            .extend(read.misreads.iter().map(|note| format!("{path}: {note}")));
        if let Some(why) = read.unsettled {
            database.unread = Some(format!("{path}: unread balance decision category: {why}"));
        }
        for item in read.items {
            let token =
                parser::c_string(item.key.as_deref().unwrap_or_default()).to_ascii_lowercase();
            let category = item
                .value
                .items()
                .unwrap_or_default()
                .iter()
                .rfind(|item| item.keyword_is("decision_category"))
                .and_then(|item| item.value.scalar())
                .unwrap_or_default()
                .to_ascii_lowercase();
            balances.insert(token, category);
        }
    }
    database.power_balance_categories.extend(
        balances
            .into_values()
            .filter(|category| !category.is_empty()),
    );
    for path in layers.directory_files("common/decisions", ".txt")? {
        append(
            &mut database,
            &path.read(layers)?,
            &path.text(),
            &categories,
            &names,
        );
    }
    Ok(database)
}

#[cfg(test)]
mod tests;
