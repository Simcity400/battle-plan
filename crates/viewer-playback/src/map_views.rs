use std::sync::Mutex;
use std::{collections::BTreeSet, sync::Arc};
use viewer_unit_reads::counter_move::SharedMovement;
use viewer_native_gui::icon_windows::Pools;
use viewer_gpu::province_schedule::Shared;
use viewer_runtime::unit_handle::Unit;
use viewer_counter_kit::CounterSettings;
use viewer_counters_naval::naval_windows;
use viewer_native_gui::icon_windows::{PoolView, Retry};

fn counter_owners(
    frame: &replay_frame::frame::DecodedFrame,
    filter: &dashboard_core::protocol::CounterFilter,
) -> Option<Arc<BTreeSet<u32>>> {
    use dashboard_core::protocol::CounterFilter;
    use replay_data::rows::tables;
    match filter {
        CounterFilter::All => None,
        CounterFilter::Players => {
            let links = frame.complete::<tables::CountryLinks>().ok()?;
            let humans = frame.complete::<tables::HumanControllers>().ok()?;
            player_slots(
                links.iter().map(|link| (link.id, link.canonical_slot)),
                humans
                    .iter()
                    .filter(|row| row.count > 0)
                    .map(|row| row.country),
            )
            .map(Arc::new)
        }
        CounterFilter::Countries { tags } => {
            let countries = frame.complete::<tables::Countries>().ok()?;
            Some(Arc::new(chosen_slots(
                tags,
                countries
                    .iter()
                    .map(|country| (country.tag.as_slice(), country.canonical_slot)),
            )))
        }
    }
}

fn player_slots(
    links: impl IntoIterator<Item = (u32, u32)>,
    humans: impl IntoIterator<Item = u32>,
) -> Option<BTreeSet<u32>> {
    let slots: std::collections::BTreeMap<u32, u32> = links.into_iter().collect();
    humans
        .into_iter()
        .map(|country| slots.get(&country).copied())
        .collect()
}

fn chosen_slots<'a>(
    tags: &[String],
    countries: impl IntoIterator<Item = (&'a [u8], u32)>,
) -> BTreeSet<u32> {
    let tags: BTreeSet<&[u8]> = tags.iter().map(|tag| tag.as_bytes()).collect();
    countries
        .into_iter()
        .filter(|(tag, _)| tags.contains(tag))
        .map(|(_, slot)| slot)
        .collect()
}

#[derive(Debug, Default)]
pub struct ResolvedSettings {
    read: Option<(u64, String, Arc<dashboard_core::protocol::ResolvedMapSettings>)>,
}

impl ResolvedSettings {
    pub fn get(
        &mut self,
        generation: u64,
        scope: String,
        resolve: impl FnOnce(&str) -> dashboard_core::protocol::ResolvedMapSettings,
    ) -> Arc<dashboard_core::protocol::ResolvedMapSettings> {
        match &self.read {
            Some((at, key, settings)) if *at == generation && *key == scope => Arc::clone(settings),
            _ => {
                let settings = Arc::new(resolve(&scope));
                self.read = Some((generation, scope, Arc::clone(&settings)));
                settings
            }
        }
    }
}

const OWNER_TABLES: [replay_data::rows::TableId; 3] = [
    replay_data::rows::TableId::CountryLinks,
    replay_data::rows::TableId::HumanControllers,
    replay_data::rows::TableId::Countries,
];

#[derive(Debug, Default)]
pub struct CounterOwners {
    resolved: Option<ResolvedOwners>,
    head: Option<viewer_layer_core::playhead_host::views::AtHead>,
}

type ResolvedOwners = (
    [replay_frame::frame::ViewIdentity; OWNER_TABLES.len()],
    dashboard_core::protocol::CounterFilter,
    Option<Arc<BTreeSet<u32>>>,
);

impl CounterOwners {
    pub fn get(
        &mut self,
        today: Option<&replay_frame::frame::DecodedFrame>,
        head: Option<&viewer_layer_core::playhead_host::views::Views>,
        filter: &dashboard_core::protocol::CounterFilter,
    ) -> Option<Arc<BTreeSet<u32>>> {
        use viewer_layer_core::playhead_host::views::{AtHead, Pace};
        if matches!(filter, dashboard_core::protocol::CounterFilter::All) {
            self.head = None;
            self.resolved = None;
            return None;
        }
        let Some(views) = head else {
            self.head = None;
            return today.and_then(|frame| self.read(frame, filter));
        };
        let at = self.head.get_or_insert_with(|| AtHead::new("counter owners", &OWNER_TABLES, Pace::Step).keeps_its_answer());
        match at.frame(views) {
            Some(rows) => self.read(&rows, filter),
            None => self.resolved.as_ref().and_then(|(_, _, owners)| owners.clone()),
        }
    }

    fn read(
        &mut self,
        frame: &replay_frame::frame::DecodedFrame,
        filter: &dashboard_core::protocol::CounterFilter,
    ) -> Option<Arc<BTreeSet<u32>>> {
        let tables = OWNER_TABLES.map(|table| frame.tables[table.index()].identity());
        self.get_with(tables, filter, |filter| counter_owners(frame, filter))
    }

    fn get_with(
        &mut self,
        tables: [replay_frame::frame::ViewIdentity; OWNER_TABLES.len()],
        filter: &dashboard_core::protocol::CounterFilter,
        read: impl FnOnce(&dashboard_core::protocol::CounterFilter) -> Option<Arc<BTreeSet<u32>>>,
    ) -> Option<Arc<BTreeSet<u32>>> {
        match &self.resolved {
            Some((at, cached, owners)) if *at == tables && cached == filter => owners.clone(),
            _ => {
                let owners = read(filter);
                self.resolved = Some((tables, filter.clone(), owners.clone()));
                owners
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Views {
    pub naval: naval_windows::Windows,
    pub labels: viewer_map_icons::labels::LabelView,
    pub pings: PoolView,
    pub raids: PoolView,
    pub intel: viewer_raids::map::intel_icons::IntelView,
    pub bases: viewer_counters_naval::base_windows::View,
    pub land_combats: viewer_counter_kit::land_windows::View,
    pub resources: viewer_map_icons::resource_icons::ResourceIconView,
    pub occupation: viewer_map_icons::occupation_icons::OccupationIconView,
    pub adjacency_rules: PoolView,
}

impl Views {
    pub fn retries(&self) -> Vec<Retry> {
        let mut retries = self.naval.retries();
        retries.extend(self.labels.retries());
        retries.extend(self.pings.retry());
        retries.extend(self.raids.retry());
        retries.extend(self.bases.retries());
        retries.extend(self.land_combats.retry());
        retries.extend(self.resources.retry());
        retries.extend(self.occupation.retry());
        retries.extend(self.adjacency_rules.retry());
        retries
    }

    pub fn set_quiet(&mut self, quiet: bool) {
        self.naval.set_quiet(quiet);
        self.labels.set_quiet(quiet);
        self.pings.set_quiet(quiet);
        self.raids.set_quiet(quiet);
        self.bases.set_quiet(quiet);
        self.land_combats.set_quiet(quiet);
        self.adjacency_rules.set_quiet(quiet);
    }

    pub fn running_visibly(&self) -> bool {
        self.naval.running_visibly()
            || self.labels.running_visibly()
            || self.pings.running_visibly()
            || self.raids.running_visibly()
            || self.intel.running()
            || self.bases.running_visibly()
            || self.land_combats.running_visibly()
            || self.resources.running_visibly()
            || self.occupation.running()
            || self.adjacency_rules.running_visibly()
    }

    pub fn take_running(&mut self) -> Pools {
        let running = Pools {
            naval: self.naval.take_active(),
            labels: self.labels.take_running(),
            pings: self.pings.take_running(),
            raids: self.raids.take_running(),
            intel: self.intel.take_running(),
            bases: self.bases.take_running(),
            land_combats: self.land_combats.take_running(),
            resources: self.resources.take_running(),
            occupation: self.occupation.take_running(),
            adjacency_rules: self.adjacency_rules.take_running(),
        };
        for (running, name) in running.named() {
            viewer_runtime::redraws::note(name, u64::from(running));
        }
        running
    }
}

#[derive(Debug, Default)]
pub struct Presentation {
    pub windows: Mutex<Views>,
    pub counters: Mutex<viewer_counters::counters::preparation::Preparation>,
    schedule: Shared,
    movement: SharedMovement,
}

impl Presentation {
    pub fn schedule(&self) -> &Shared {
        &self.schedule
    }

    pub fn movement(&self) -> &SharedMovement {
        &self.movement
    }

    pub fn sharing(&self) -> Self {
        Self {
            windows: Default::default(),
            counters: Default::default(),
            schedule: Arc::clone(&self.schedule),
            movement: Arc::clone(&self.movement),
        }
    }

    pub fn snapshot(&self) -> Self {
        let schedule = self.schedule.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        Self {
            windows: Default::default(),
            counters: Default::default(),
            schedule: Arc::new(Mutex::new(schedule)),
            movement: Default::default(),
        }
    }

    pub fn take_memory(&self, laid: &Presentation) {
        let mut ours = self.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut theirs = laid.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::swap(&mut *ours, &mut *theirs);
    }

    pub fn lend_layered(&self, laid: &Presentation) {
        let ours = self.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        laid.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).adopt_layered(&ours);
    }

    pub fn take_running(&self) -> Running {
        let (windows, visible) = {
            let mut views = self.windows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let visible = views.running_visibly();
            (views.take_running(), visible)
        };
        let moving = self
            .movement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running();
        viewer_runtime::redraws::note("running.movement", u64::from(moving));
        Running { any: windows.any() || moving, visible, windows }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Running {
    pub any: bool,
    pub visible: bool,
    pub windows: Pools,
}

#[derive(Clone, Debug, Default)]
pub struct CounterView {
    pub settings: CounterSettings,
    pub selected: Arc<BTreeSet<Unit>>,
    pub air_selected: Arc<Vec<u64>>,
    pub air_bases: Arc<Vec<u64>>,
    pub fleets_selected: Arc<Vec<u64>>,
    pub hovered: Option<Unit>,
    pub pressed: Option<Unit>,
    pub plate_hovered: Option<Unit>,
    pub operatives: bool,
    pub occupation: Option<viewer_map_icons::occupation_icons::Mode>,
    pub presentation: Arc<Presentation>,
    pub visibility: viewer_foundation::mode_visibility::Visibility,
    pub raid_details: Option<(u32, Arc<viewer_raids::map::raids::tooltips::Details>)>,
    pub raid_hovered: Option<u32>,
}

impl CounterView {
    pub fn set_mode(&mut self, mode: &viewer_map_modes::map_mode::MapMode) {
        (self.visibility, self.operatives, self.occupation) = Self::of_mode(mode);
    }

    pub fn matches_mode(&self, mode: &viewer_map_modes::map_mode::MapMode) -> bool {
        (self.visibility, self.operatives, self.occupation) == Self::of_mode(mode)
    }

    fn of_mode(mode: &viewer_map_modes::map_mode::MapMode) -> (viewer_foundation::mode_visibility::Visibility, bool, Option<viewer_map_icons::occupation_icons::Mode>) {
        use viewer_map_modes::map_mode::{MapMode, Stock};
        use viewer_map_icons::occupation_icons::Mode;
        (
            viewer_map_modes::map_mode::visibility::of(mode),
            matches!(mode, MapMode::Stock(Stock::Operatives)),
            match mode {
                MapMode::Stock(Stock::Resistance) => Some(Mode::Resistance),
                MapMode::Stock(Stock::Compliance) => Some(Mode::Compliance),
                _ => None,
            },
        )
    }

    pub fn new(settings: CounterSettings) -> Self {
        Self {
            settings,
            selected: Arc::default(),
            air_selected: Arc::default(),
            air_bases: Arc::default(),
            fleets_selected: Arc::default(),
            hovered: None,
            pressed: None,
            plate_hovered: None,
            operatives: false,
            occupation: None,
            presentation: Arc::default(),
            visibility: Default::default(),
            raid_details: None,
            raid_hovered: None,
        }
    }
}

impl PartialEq for CounterView {
    fn eq(&self, other: &Self) -> bool {
        self.settings == other.settings
            && self.selected == other.selected
            && self.air_selected == other.air_selected
            && self.air_bases == other.air_bases
            && self.fleets_selected == other.fleets_selected
            && self.hovered == other.hovered
            && self.operatives == other.operatives
            && self.occupation == other.occupation
            && Arc::ptr_eq(&self.presentation, &other.presentation)
            && self.visibility == other.visibility
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashboard_core::protocol::CounterFilter;

    #[test]
    fn only_a_pool_with_a_pass_to_run_asks_for_the_pools_to_be_presented() {
        let view = Presentation::default();
        let idle = Running { any: false, visible: false, windows: Pools::default() };
        assert_eq!(view.take_running(), idle, "nothing asks for anything");
        view.windows.lock().unwrap().pings.keep_running();
        let running = view.take_running();
        assert_eq!(
            running,
            Running {
                any: true,
                visible: false,
                windows: Pools { pings: true, ..Pools::default() },
            }
        );
        assert_eq!(view.take_running(), idle, "taken, so the frame after it asks for nothing");
    }

    #[test]
    fn a_changed_air_selection_invalidates_the_prepared_view() {
        let first = CounterView::default();
        let mut selected = first.clone();
        selected.air_selected = Arc::new(vec![7]);
        assert_ne!(first, selected);
        assert_eq!(selected, selected.clone());
        selected = first.clone();
        selected.air_bases = Arc::new(vec![9]);
        assert_ne!(first, selected);
        selected = first.clone();
        selected.fleets_selected = Arc::new(vec![11]);
        assert_ne!(first, selected);
    }

    #[test]
    fn players_are_placed_through_the_links_and_an_unplaced_one_draws_everyone() {
        let links = [(10, 1), (20, 2), (30, 3)];
        assert_eq!(player_slots(links, [30, 10]), Some(BTreeSet::from([1, 3])));
        assert_eq!(player_slots(links, [10, 40]), None);
        assert_eq!(player_slots(links, []), Some(BTreeSet::new()));
    }

    #[test]
    fn settings_are_resolved_again_only_for_another_generation_or_scope() {
        let mut settings = ResolvedSettings::default();
        let mut resolved = Vec::new();
        let mut resolve = |scope: &str| {
            resolved.push(scope.to_owned());
            dashboard_core::protocol::ResolvedMapSettings::default()
        };
        let first = settings.get(1, "c".into(), &mut resolve);
        let again = settings.get(1, "c".into(), &mut resolve);
        assert!(Arc::ptr_eq(&first, &again));
        settings.get(2, "c".into(), &mut resolve);
        settings.get(2, "c/widget/a".into(), &mut resolve);
        settings.get(2, "c/widget/a".into(), &mut resolve);
        assert_eq!(resolved, ["c", "c", "c/widget/a"]);
    }

    #[test]
    fn owners_are_read_again_only_for_other_tables_or_another_filter() {
        use replay_frame::{frame::SharedView, frame::TableView};
        let view = || -> SharedView {
            TableView::Unavailable {
                reason: "synthetic".into(),
            }
            .into()
        };
        let first: [SharedView; 3] = std::array::from_fn(|_| view());
        let other: [SharedView; 3] = std::array::from_fn(|_| view());
        assert_eq!(first, other);
        let identities = |tables: &[SharedView; 3]| tables.each_ref().map(SharedView::identity);
        let mut owners = CounterOwners::default();
        let mut reads = 0;
        let mut read = |filter: &CounterFilter| {
            reads += 1;
            (*filter != CounterFilter::All).then(|| Arc::new(BTreeSet::from([reads])))
        };
        let shown = owners.get_with(identities(&first), &CounterFilter::Players, &mut read);
        let again = owners.get_with(identities(&first), &CounterFilter::Players, &mut read);
        assert!(Arc::ptr_eq(
            shown.as_ref().unwrap(),
            again.as_ref().unwrap()
        ));
        let switched = owners.get_with(identities(&other), &CounterFilter::Players, &mut read);
        assert_eq!(switched, Some(Arc::new(BTreeSet::from([2]))));
        let mut changed = other;
        changed[1] = view();
        owners.get_with(identities(&changed), &CounterFilter::Players, &mut read);
        owners.get_with(identities(&changed), &CounterFilter::All, &mut read);
        assert_eq!(
            owners.get_with(identities(&changed), &CounterFilter::All, &mut read),
            None
        );
        assert_eq!(reads, 4);
    }
}
