
use crate::{
    document::Document,
    playhead_host::views::{AtHead, Need, Pace, Views},
};
use replay_data::{BranchId, Hour, rows::TableId};
use replay_frame::frame::DecodedFrame;
use viewer_counters::counters::Shown;


#[cfg(test)]
mod tests;

pub(crate) type Head<'a> = Option<&'a Views>;

const PACE: Pace = Pace::Step;

const CLICKS: &[TableId] = &[
    TableId::Provinces,
    TableId::States,
    TableId::StateFlags,
    TableId::Countries,
    TableId::CountryLinks,
    TableId::CountryAppearance,
    TableId::ReservedCountryNaming,
    TableId::CountryGameScoreInputs,
    TableId::Divisions,
    TableId::LandCombats,
    TableId::TaskForces,
    TableId::Fleets,
    TableId::Ships,
    TableId::NavalRepairQueue,
    TableId::AirRegionalMissions,
    TableId::Raids,
    TableId::RaidTargets,
    TableId::RaidFacilities,
];

const NAVAL_MISSION_CLICKS: &[TableId] = &[TableId::NavalMissionLedgers];

const AIR_CLICKS: &[TableId] = &[TableId::AirBases, TableId::AirBaseRegistrations, TableId::AirReferenceWitnesses, TableId::AirWings];

const DIVISION_CLICKS: &[TableId] = &[TableId::ArmyIconInputs, TableId::DivisionTemplateIcons];

const RAID_CLICKS: &[TableId] = &[TableId::UnitLeaders];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Clickable {
    pub shown: Shown,
    pub air: bool,
    pub divisions: bool,
    pub raids: bool,
}

impl Clickable {
    pub(crate) const GROUND: Self = Self { shown: Shown { units: false, bases: false, land_combats: false, naval_missions: false }, air: false, divisions: false, raids: false };
}

pub(crate) fn click_tables(clickable: Clickable, learned: &[TableId]) -> Vec<TableId> {
    let family = |held: bool, tables: &'static [TableId]| held.then_some(tables).unwrap_or_default();
    let mut tables: Vec<TableId> = CLICKS
        .iter()
        .chain(family(clickable.shown.naval_missions, NAVAL_MISSION_CLICKS))
        .chain(family(clickable.air, AIR_CLICKS))
        .chain(family(clickable.divisions, DIVISION_CLICKS))
        .chain(family(clickable.raids, RAID_CLICKS))
        .chain(learned)
        .copied()
        .collect();
    tables.sort_unstable_by_key(|table| table.index());
    tables.dedup();
    tables
}

pub(crate) fn hidden_families(shown: Shown) -> Vec<TableId> {
    use viewer_playback::overlay::{overlay_tables, tables_shown};
    let drawn = tables_shown(shown);
    overlay_tables().into_iter().filter(|table| !drawn.contains(table)).collect()
}

pub(crate) fn head(side: Option<&viewer_playback::side::SideThread>) -> Head<'_> {
    side.map(viewer_playback::side::SideThread::views)
}

pub(crate) fn branch(side: Option<&viewer_playback::side::SideThread>, document: &Document) -> replay_data::BranchId {
    side.and_then(viewer_playback::side::SideThread::branch).unwrap_or_else(|| document.playback.branch())
}

#[derive(Debug)]
pub(crate) struct HeadRows {
    units: AtHead,
    arrows: AtHead,
    air: AtHead,
    fleets: AtHead,
    clicks: AtHead,
    clickable: Clickable,
    learned: Vec<TableId>,
    click_at: Option<(BranchId, Hour)>,
    walk: AtHead,
    map: AtHead,
    find: AtHead,
    cursor: AtHead,
    order: AtHead,
    order_read: Option<(BranchId, Hour, super::selection_input::ClientOrder)>,
    four: Option<bool>,
}

impl Default for HeadRows {
    fn default() -> Self {
        Self {
            units: AtHead::with("selection units", &[Need::keys(TableId::Divisions), Need::keys(TableId::RailwayGunCounterInputs), Need::keys(TableId::TaskForces)], PACE).keeps_its_answer(),
            arrows: AtHead::new("selection arrows", &[], PACE).keeps_its_answer(),
            air: AtHead::with("selection air", &[Need::keys(TableId::AirWings), Need::keys(TableId::AirBaseRegistrations)], PACE).keeps_its_answer(),
            fleets: AtHead::with("selection fleets", &[Need::keys(TableId::Fleets)], PACE).keeps_its_answer(),
            clicks: AtHead::new("overlay clicks", &click_tables(Clickable::GROUND, &[]), PACE),
            clickable: Clickable::GROUND,
            learned: Vec::new(),
            click_at: None,
            walk: AtHead::new("walk figure", &[], PACE).keeps_its_answer(),
            map: AtHead::new("clock and picker", &[TableId::Clock, TableId::SimulationFeatures], PACE).keeps_its_answer(),
            find: AtHead::new("find view", &[], PACE).keeps_its_answer(),
            cursor: AtHead::new("order cursor", &[], PACE).keeps_its_answer(),
            order: AtHead::new("unit order", &[TableId::LandUnitOrder, TableId::LocalPlayer, TableId::CountryLinks], PACE).keeps_its_answer(),
            order_read: None,
            four: None,
        }
    }
}

fn read_at<T>(at: &mut AtHead, head: Head<'_>, today: Option<&DecodedFrame>, mut read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
    match head {
        Some(views) => {
            let rows = at.frame(views)?;
            at.discover(|| read(&rows))
        }
        None => today.map(read),
    }
}

impl HeadRows {
    pub(crate) fn open_clicks(&mut self, views: &Views) {
        self.clicks.keep(views);
    }

    pub(crate) fn units<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.units, head, today, read)
    }

    pub(crate) fn arrows<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.arrows, head, today, read)
    }

    pub(crate) fn air<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.air, head, today, read)
    }

    pub(crate) fn fleets<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.fleets, head, today, read)
    }

    pub(crate) fn walk<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.walk, head, today, read)
    }

    pub(crate) fn walk_hour(&mut self, head: Head<'_>) -> Option<Hour> {
        self.walk.frame(head?).map(|rows| rows.hour)
    }

    pub(crate) fn find<T>(&mut self, head: Head<'_>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.find, Some(head?), None, read)
    }

    pub(crate) fn cursor<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.cursor, head, today, read)
    }

    pub(crate) fn unit_order(&mut self, head: Head<'_>) -> Option<super::selection_input::ClientOrder> {
        let rows = self.order.frame(head?)?;
        if let Some((branch, hour, read)) = &self.order_read
            && (*branch, *hour) == (rows.branch, rows.hour)
        {
            return Some(read.clone());
        }
        let read = self.order.discover(|| super::selection_input::recorded_unit_order(&rows))?;
        self.order_read = Some((rows.branch, rows.hour, read.clone()));
        Some(read)
    }

    pub(crate) fn map<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        read_at(&mut self.map, head, today, read)
    }

    pub(crate) fn four_buttons(&mut self, head: Head<'_>, document: &Document) -> bool {
        if head.is_none() {
            return super::picker::four_buttons(document);
        }
        if let Some(four) = self.map(head, None, super::picker::four_buttons_at) {
            self.four = Some(four);
        }
        self.four.unwrap_or(true)
    }


    pub(crate) fn click_at(&mut self, at: Option<(BranchId, Hour)>) {
        self.click_at = at;
    }

    pub(crate) fn clicked_at(&self) -> Option<(BranchId, Hour)> {
        self.click_at
    }

    pub(crate) fn clickable(&mut self, clickable: Clickable) {
        if clickable != self.clickable {
            self.clickable = clickable;
            let hidden = hidden_families(clickable.shown);
            self.learned.retain(|table| !hidden.contains(table));
            self.declare_clicks();
        }
    }

    fn declare_clicks(&mut self) {
        let needs: Vec<Need> = click_tables(self.clickable, &self.learned).into_iter().map(Need::every).collect();
        self.clicks.declare(&needs);
    }

    pub(crate) fn clicks<T>(&mut self, head: Head<'_>, today: Option<&DecodedFrame>, mut read: impl FnMut(&DecodedFrame) -> T) -> Option<T> {
        let Some(views) = head else { return today.map(read) };
        let (branch, hour) = self.click_at?;
        let rows = self.clicks.frame_of(views, hour).filter(|rows| rows.branch == branch)?;
        let (value, missed) = replay_frame::frame::discovering_fields(|| read(&rows));
        if missed.is_empty() {
            return Some(value);
        }
        for (table, _) in missed {
            if !self.learned.contains(&table) {
                self.learned.push(table);
            }
        }
        self.declare_clicks();
        None
    }

    pub(crate) fn end_frame(&mut self, head: Head<'_>) {
        if let Some(views) = head {
            self.open_clicks(views);
        }
        for at in [&mut self.units, &mut self.arrows, &mut self.air, &mut self.fleets, &mut self.walk, &mut self.map, &mut self.cursor, &mut self.order, &mut self.clicks, &mut self.find] {
            at.end_frame();
        }
    }
}
