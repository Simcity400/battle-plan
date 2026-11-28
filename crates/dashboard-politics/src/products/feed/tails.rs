mod page;

use page::{FeedPage, ProjectedLine, StagedPage};

use super::{
    EventFeedCategory, EventFeedKind, EventFeedRow, EventFeedUnavailable, Names, Notices, Presenter, branch, content_from, content_refusals, families, invalid,
    cards::{List, ScriptEdge, ScriptOccurrence, edge},
    fold::{self, PlannedLine},
    index::{self, Feed, Item, Locator, ScriptStatus, Scanned, TitleKeys, kind_code},
    present_all, present_rows,
    row::FeedEvent,
    text::{KeptNaming, kept_naming},
};
use dashboard_base::{
    Source,
    catalogue::{self, Catalogue},
    products::{
        LoadContext, Product,
        occurrences::{self, OccurrenceCount, OccurrenceReply, OccurrenceSort},
    },
};
use dashboard_follow::panel::Panel;
use dashboard_query::{
    Query,
    identity::{Country, Holders},
    log::occurrence::{Answer, Ask, Chip, DatedCursor, Index, IndexedStateRecords, Order, PlacedGap, Selection, Shown},
    series::ranked::GroupSelection,
    timeline::Timeline,
};
use dashboard_world::products::place_name_runs::{self, PlacePoint, RowNames};
use replay::campaign::Replay;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventFeedTailParams {
    pub branch_id: String,
    #[doc = " The observation cutoff: nothing observed later exists. The branch's last hour when none."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub hour: Option<i32>,
    #[doc = " The first and last occurrence hour listed."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub to: Option<i32>,
    #[doc = " Rows naming one of these countries at the row's date; every row when neither this nor `group`."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub countries: Option<Vec<Country>>,
    #[doc = " Rows naming a member of the group at the row's date, membership resolved in Rust."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub search: Option<String>,
    #[doc = " One row by its key, with the detail text list rows leave out; no row when it is not in the log at the hour."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub row: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub sort: Option<OccurrenceSort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub offset: Option<String>,
    pub limit: u32,
    pub categories: Vec<EventFeedCategory>,
    #[doc = " Kinds left out of the categories asked, by name; an unknown name is refused."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub exclude: Option<Vec<String>>,
    #[doc = " The folds, by key, whose newest 32 events are listed as lines after them; the others list none."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub expanded: Option<Vec<String>>,
    #[doc = " The expanded folds, by key, that list every event, paged with the other lines."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub full: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub place_names: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventFeedActivityParams {
    pub branch_id: String,
    #[doc = " The observation cutoff and the last hour binned."]
    pub hour: i32,
    #[doc = " The first hour binned; rows dated before it are counted in `before`."]
    pub from: i32,
    #[cfg_attr(any(test, feature = "export-types"), ts(type = "number"))]
    pub bin_hours: i64,
    pub categories: Vec<EventFeedCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub exclude: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub countries: Option<Vec<Country>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedActivityLine {
    pub category: EventFeedCategory,
    pub before: String,
    #[doc = " Per bin with rows: its index from `firstHour` and its count."]
    #[cfg_attr(any(test, feature = "export-types"), ts(type = "Array<[number, number]>"))]
    pub bins: Vec<(u32, u64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub latest_retirement: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedActivityReply {
    pub branch_id: String,
    #[doc = " The first hour of bin 0."]
    pub first_hour: i32,
    #[cfg_attr(any(test, feature = "export-types"), ts(type = "number"))]
    pub bin_hours: i64,
    pub lines: Vec<EventFeedActivityLine>,
    #[doc = " Hours of the span the feed does not hold, cut at the hour asked."]
    pub missing: Vec<dashboard_query::series::MissingSpan>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(EventFeedActivityReply);

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedKindInfo {
    pub kind: EventFeedKind,
    pub category: EventFeedCategory,
    pub first_observed: Option<i32>,
    pub unavailable: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedKindsReply {
    pub kinds: Vec<EventFeedKindInfo>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(EventFeedKindsReply);

fn feed_kinds(categories: &[EventFeedCategory], exclude: Option<&[String]>) -> io::Result<Vec<u16>> {
    let mut left_out = Vec::new();
    for name in exclude.unwrap_or_default() {
        let kind: EventFeedKind = serde_json::from_value(serde_json::Value::String(name.clone())).map_err(|_| invalid(format!("The event feed has no kind {name}")))?;
        left_out.push(kind);
    }
    Ok(EventFeedKind::ALL.iter().filter(|kind| categories.contains(&kind.category()) && !left_out.contains(kind)).map(|&kind| kind_code(kind)).collect())
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptTailParams {
    pub branch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub hour: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub to: Option<i32>,
    #[doc = " Rows naming one of these countries at the row's date; every row when neither this nor `group`."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub countries: Option<Vec<Country>>,
    #[doc = " Rows naming a member of the group at the row's date, membership resolved in Rust."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub search: Option<String>,
    #[doc = " One row by its key, with the detail text list rows leave out; no row when it is not in the log at the hour."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub row: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub sort: Option<OccurrenceSort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub offset: Option<String>,
    pub limit: u32,
    #[doc = " The events whose choices are listed; every one when none. Decisions take none."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub events: Option<Vec<ScriptEventKind>>,
    #[doc = " The statuses listed, each at the hour asked; every one when none."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub statuses: Option<Vec<ScriptStatus>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ScriptEventKind {
    Country,
    News,
    Unknown,
}

impl ScriptEventKind {
    fn kind(self) -> EventFeedKind {
        match self {
            Self::Country => EventFeedKind::ScriptEventFired,
            Self::News => EventFeedKind::NewsEventFired,
            Self::Unknown => EventFeedKind::EventOptionSelected,
        }
    }
}

const FEED_FIELDS: dashboard_base::products::Fields =
    dashboard_base::fields![branchId, hour: Read, from: Start, to: End, countries, "countries[]", group, search, sort, offset, limit, categories, exclude, "exclude[]", expanded, "expanded[]", full, "full[]", row, placeNames];

const CARD_FIELDS: dashboard_base::products::Fields =
    dashboard_base::fields![branchId, hour: Read, from: Start, to: End, countries, "countries[]", group, search, sort, offset, limit, events, statuses, row];

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedPageRow {
    #[doc = " The occurrence's stable id: the row key and what a click hands to the detail."]
    pub key: String,
    #[doc = " The occurrence hour, by which rows are ranged and ordered."]
    pub hour: i32,
    #[doc = " The hour the recording first witnessed it."]
    pub observed: i32,
    pub live: bool,
    pub row: Option<EventFeedRow>,
    #[doc = " A fired event's chosen option, where one was observed by the hour asked."]
    pub choice: Option<ScriptEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub status: Option<ScriptStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub not_recorded_reason: Option<String>,
    #[doc = " The fold this event was expanded under; absent for standalone events."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub place_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub place_missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedFoldRow {
    pub key: String,
    pub hour: i32,
    pub observed: i32,
    pub count: String,
    pub kind: EventFeedKind,
    pub country: String,
    pub first_row: Option<EventFeedRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedMoreRow {
    pub key: String,
    pub parent: String,
    pub hour: i32,
    pub total: String,
    pub remaining: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(tag = "line", rename_all = "camelCase")]
pub enum EventFeedLine {
    Event(Arc<EventFeedPageRow>),
    Fold(Arc<EventFeedFoldRow>),
    More(EventFeedMoreRow),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EventFeedTailReply {
    pub branch_id: String,
    pub total: String,
    pub offset: String,
    pub live: String,
    pub counts: Vec<OccurrenceCount>,
    pub rows: Vec<EventFeedLine>,
    pub missing: Vec<PlacedGap>,
    pub next_change: Option<i32>,
    pub unavailable: Vec<EventFeedUnavailable>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(EventFeedTailReply);

impl OccurrenceReply for EventFeedTailReply {
    fn next_change(&self) -> Option<i32> {
        self.next_change
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ScriptPageRow {
    pub key: String,
    pub hour: i32,
    pub observed: i32,
    pub live: bool,
    #[doc = " The card's status at the hour asked."]
    pub status: Option<ScriptStatus>,
    pub occurrence: Option<ScriptOccurrence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub not_recorded_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub not_recorded_hour: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ScriptOccurrenceTailReply {
    pub branch_id: String,
    pub total: String,
    pub offset: String,
    pub live: String,
    pub counts: Vec<OccurrenceCount>,
    pub rows: Vec<ScriptPageRow>,
    pub missing: Vec<PlacedGap>,
    pub next_change: Option<i32>,
    pub unavailable: Vec<EventFeedUnavailable>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(ScriptOccurrenceTailReply);

impl OccurrenceReply for ScriptOccurrenceTailReply {
    fn next_change(&self) -> Option<i32> {
        self.next_change
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }
}

struct Owned {
    replay: Arc<Replay>,
    timeline: Arc<Timeline>,
    holders: Arc<Holders>,
}

impl Owned {
    fn of(query: &Query<'_>, replay: Arc<Replay>) -> Self {
        Self { replay, timeline: Arc::clone(&query.timeline), holders: Arc::clone(&query.holders) }
    }

    fn query(&self) -> Query<'_> {
        Query { replay: &self.replay, timeline: Arc::clone(&self.timeline), holders: Arc::clone(&self.holders) }
    }
}

struct Prepared {
    selection: Option<Selection>,
    index: Arc<Index<Item>>,
    feed: Option<Arc<Feed>>,
    scanned: Arc<Scanned>,
    losses: Vec<(i32, i32)>,
    content: Arc<Result<Arc<Catalogue>, String>>,
    kept: Arc<Mutex<KeptNaming>>,
    places: Mutex<RowNames>,
    assets: Option<Arc<super::assets::Warming>>,
}

fn branch_id(text: &str) -> io::Result<u64> {
    text.parse().map_err(|_| invalid("Invalid branch"))
}

fn title_keys(search: Option<&str>, sort: Option<&OccurrenceSort>) -> Option<TitleKeys> {
    if search.is_some_and(|text| !text.trim().is_empty()) { return Some(TitleKeys::Search); }
    match sort.map(|sort| sort.column.as_str()) {
        Some("item") => Some(TitleKeys::Item),
        Some("choice") => Some(TitleKeys::Choice),
        _ => None,
    }
}

macro_rules! asked {
    ($params:expr) => {
        ($params.countries.as_deref(), $params.group.as_ref(), title_keys($params.search.as_deref(), $params.sort.as_ref()))
    };
}

fn prepare(source: &Source, query: &Query<'_>, branch: u64, context: &LoadContext, list: Option<List>, asked: (Option<&[Country]>, Option<&GroupSelection>, Option<TitleKeys>), cancelled: &AtomicBool) -> io::Result<Prepared> {
    let selection = dashboard_world::products::group_members::dated(source, query, context, asked.0, asked.1, cancelled)?;
    let content = Arc::new(super::resolve_content(source, context));
    let scanned = index::scanned_for(source, query, branch, content.as_ref().as_ref().ok(), cancelled)?;
    let assets = if list.is_none() && let Ok(catalogue) = content.as_ref() {
        super::assets::launch(source, branch, context, catalogue, &scanned)
    } else { None };
    let (index, feed) = match list {
        Some(List::Decisions) => (index::decisions_for(source, query, branch, content.as_ref().as_ref().ok(), cancelled)?, None),
        Some(List::EventChoices) => (index::choices_for(source, query, branch, content.as_ref().as_ref().ok(), cancelled)?, None),
        None => {
            let feed = index::feed_for(source, query, branch, context, content.as_ref().as_ref().ok(), cancelled)?;
            (Arc::clone(&feed.index), Some(feed))
        }
    };
    let kept = kept_naming(source);
    let index = match asked.2 {
        Some(keys) => {
            let presenter = Presenter::new(query, content.as_ref().as_ref().ok(), Arc::clone(&kept));
            dashboard_base::catalogue::capture_isolated(|| scanned.title_index(query, &index, list, keys, presenter, cancelled)).0?
        }
        None => index,
    };
    let losses = source.replay_ref().event_losses(branch).collect();
    Ok(Prepared { selection, index, feed, scanned, losses, content, kept, places: Mutex::default(), assets })
}

struct Window<'p> {
    from: Option<i32>,
    to: Option<i32>,
    selection: Option<&'p Selection>,
    search: Option<&'p str>,
    sort: Option<&'p OccurrenceSort>,
    offset: Option<&'p str>,
    limit: u32,
    sortable: &'p [&'p str],
}

fn answer(index: &Index<Item>, query: &Query<'_>, window: &Window<'_>, hour: i32, kinds: Option<&[u16]>, states: Option<&[u8]>, live_first: bool, chips: &[Chip]) -> io::Result<Answer> {
    let (order, newest) = occurrences::order(window.sort, window.sortable)?;
    let (offset, limit) = occurrences::window(window.offset, window.limit)?;
    index.ask(
        &query.holders,
        &Ask { hour, from: window.from, to: window.to, gap_end: None, selection: window.selection, subject: None, kinds, states, search: window.search, order, live_first: live_first && newest, offset, limit, chips },
    )
}

fn locators(index: &Index<Item>, answer: &Answer, hour: i32) -> Vec<Locator> {
    let mut out = Vec::new();
    for shown in &answer.rows {
        if let Item::Log { head, edges, .. } = &index.get(shown.at).payload {
            out.push(*head);
            out.extend(edges.iter().copied().filter(|edge| edge.hour <= hour));
        }
    }
    out
}

fn planned_locators(index: &Index<Item>, planned: &[PlannedLine], hour: i32, indexed: Option<&Scanned>) -> Vec<Locator> {
    let mut out = Vec::new();
    for line in planned {
        match line {
            PlannedLine::Event { at, .. } => if let Item::Log { head, edges, .. } = &index.get(*at).payload {
                out.push(*head);
                if let Some(scanned) = indexed {
                    out.extend(scanned.visible_choice(edges, hour));
                } else {
                    out.extend(edges.iter().copied().filter(|edge| edge.hour <= hour));
                }
            },
            PlannedLine::Fold { first_at, .. } => if let Item::Log { head, .. } = &index.get(*first_at).payload { out.push(*head); },
            PlannedLine::More { .. } => {}
        }
    }
    out
}

fn next_change(prepared: &Prepared, answer_next: Option<i32>, windows: bool, hour: i32) -> Option<i32> {
    let incomplete = &prepared.scanned.incomplete;
    let read = incomplete.get(incomplete.partition_point(|(at, ..)| *at <= hour)).map(|(at, ..)| *at);
    let reason = windows.then(|| prepared.scanned.windows_unrecorded.iter().map(|(first, ..)| *first).filter(|first| *first > hour).min()).flatten();
    [answer_next, read, reason].into_iter().flatten().min()
}

fn feed_next_change(feed: &Feed, categories: &[EventFeedCategory], (from, to): (i32, i32), hour: i32) -> Option<i32> {
    let warning = feed.warnings.iter().map(|(at, _)| *at).filter(|at| *at > hour).min();
    let left_out = categories.contains(&EventFeedCategory::WarsPeace).then(|| feed.unattributed.get(feed.unattributed.partition_point(|&at| at <= hour || at < from)).copied().filter(|&at| at <= to)).flatten();
    let unrecorded = feed
        .unrecorded
        .iter()
        .filter(|family| categories.contains(&family.category))
        .flat_map(|family| family.spans.iter().copied())
        .chain(feed.notices.iter().filter(|(cats, ..)| cats.iter().any(|c| categories.contains(c))).filter_map(|(.., span)| *span))
        .filter(|&(first, last)| first <= to && last >= from)
        .map(|(first, last)| (first.max(from), last.min(to)))
        .filter_map(|(first, last)| if first <= hour && hour < last { Some(hour + 1) } else { (first > hour).then_some(first) })
        .min();
    [warning, left_out, unrecorded].into_iter().flatten().min()
}

fn notices_for(prepared: &Prepared, unattributed: u64, windows: bool, from: i32, hour: i32, notices: &mut Notices) {
    families::incomplete_notices(&prepared.scanned.incomplete, &prepared.losses, from, hour, notices);
    if windows {
        families::window_notices(&prepared.scanned.windows_unrecorded, from, hour, notices);
    }
    if unattributed > 0 {
        notices.unavailable(
            "unattributed",
            "Country not identified",
            "These events name no country the recording identifies at their date, so the country selection cannot place them.",
            Some(u32::try_from(unattributed).unwrap_or(u32::MAX)),
        );
    }
}

struct FollowedFeed {
    dated: Option<(DatedCursor<Item>, fold::Pages)>,
    page: FeedPage,
    records: IndexedStateRecords,
}

impl FollowedFeed {
    fn new(source: &Source) -> Self {
        Self {
            dated: None,
            page: FeedPage::default(),
            records: IndexedStateRecords::new(source.replay(), replay_data::rows::TableId::FeedEvents),
        }
    }
}

fn place_point(row: &EventFeedRow) -> Option<PlacePoint> {
    let state = match row.kind {
        EventFeedKind::StateTransferred => row.cause.as_ref()?.state?,
        EventFeedKind::NuclearStrike => row.strike.as_ref()?.state?,
        _ => return None,
    };
    Some(PlacePoint::State(state, row.hour))
}

fn attach_place_names(source: &Source, query: &Query<'_>, load: &LoadContext, prepared: &Prepared, rows: &mut [EventFeedLine], hour: i32, cancelled: &AtomicBool) -> io::Result<()> {
    let point = |row: &EventFeedPageRow| {
        let event = row.row.as_ref().filter(|event| event.hour <= hour)?;
        place_point(event)
    };
    let points: BTreeSet<_> = rows.iter().filter_map(|line| match line {
        EventFeedLine::Event(row) => point(row),
        _ => None,
    }).collect();
    let mut held = prepared.places.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.update(source, query, load, points, cancelled)?;
    for line in rows {
        let EventFeedLine::Event(row) = line else { continue };
        if let Some(name) = point(row).and_then(|point| held.get(&point))
            && (row.place_name != name.name || row.place_missing != name.missing)
        {
            let row = Arc::make_mut(row);
            row.place_name.clone_from(&name.name);
            row.place_missing.clone_from(&name.missing);
        }
    }
    held.trim();
    Ok(())
}

fn load_feed_at(prepared: &Prepared, query: &Query<'_>, (source, load): (&Source, &LoadContext), params: &EventFeedTailParams, hour: i32, mut followed: Option<&mut FollowedFeed>, cancelled: &AtomicBool) -> io::Result<EventFeedTailReply> {
    dashboard_text::metadata::check_cancel(cancelled)?;
    let feed = prepared.feed.as_ref().ok_or_else(|| invalid("The event feed's index is not prepared"))?;
    let index = prepared.index.as_ref();
    let kinds = feed_kinds(&params.categories, params.exclude.as_deref())?;
    let (order, _newest) = occurrences::order(params.sort.as_ref(), &["country", "item"])?;
    let (offset, limit) = occurrences::window(params.offset.as_deref(), params.limit)?;
    let opened = params.row.is_some();
    let indexed_records = followed.is_some() && prepared.scanned.unique_record_keys;
    let (total, reply_offset, live_total, unattributed, gaps, base_next, planned) = if let Some(row_key) = params.row.as_deref() {
        let located = prepared.scanned.locate(row_key).and_then(|key| index.find(key)).or_else(|| index::family_at(index, row_key));
        match located {
            Some(at) => {
                let occ = index.get(at);
                if occ.observed > hour {
                    (0, 0, 0, 0, Vec::new(), Some(occ.observed), Vec::new())
                } else if index.retired_at(at).is_some_and(|end| hour >= end) {
                    (0, 0, 0, 0, Vec::new(), None, Vec::new())
                } else {
                    let live = u64::from(occ.state_at(hour).is_some_and(|state| index::LIVE.contains(&state)));
                    let next = [occ.states.get(occ.states.partition_point(|&(h, _)| h <= hour)).map(|step| step.0), index.retired_at(at)].into_iter().flatten().min();
                    (1, 0, live, 0, Vec::new(), next, vec![PlannedLine::Event { at, parent: None }])
                }
            }
            None => (0, 0, 0, 0, Vec::new(), None, Vec::new()),
        }
    } else {
        let ask = Ask {
            hour,
            from: params.from,
            to: params.to,
            gap_end: None,
            selection: prepared.selection.as_ref(),
            subject: None,
            kinds: Some(&kinds),
            states: None,
            search: params.search.as_deref(),
            order,
            live_first: false,
            offset,
            limit,
            chips: &[],
        };
        let (folded, live, unattributed, mut next) = if let Order::Date(direction) = order {
            if let Some(held) = followed.as_mut().map(|held| &mut held.dated) {
                if held.is_none() {
                    *held = Some((prepared.index.dated_cursor(&query.holders, &ask)?, fold::Pages::new(feed)));
                }
                let (cursor, pages) = held.as_mut().expect("the dated feed cursor was prepared");
                let update = cursor.advance(hour);
                let window = fold::Window { direction, offset, limit, expanded: params.expanded.as_deref(), full: params.full.as_deref() };
                (pages.advance(feed, &update, &window), update.live, update.unattributed, update.until)
            } else {
                index.with_dated(&query.holders, &ask, |dated| {
                    let (live, unattributed, until) = (dated.live, dated.unattributed, dated.until);
                    let folded = fold::page(feed, dated, direction, offset, limit, params.expanded.as_deref(), params.full.as_deref());
                    (folded, live, unattributed, until)
                })?
            }
        } else {
            let answer = index.ask(&query.holders, &ask)?;
            let lines = answer.rows.iter().map(|shown| PlannedLine::Event { at: shown.at, parent: None }).collect();
            (fold::FoldedPage { total: answer.total, offset: answer.offset, lines, gaps: answer.gaps }, answer.live, answer.unattributed, answer.next_change)
        };
        for line in &folded.lines {
            if let PlannedLine::Event { at, .. } = line {
                let entry = index.get(*at);
                let after = entry.states.partition_point(|&(h, _)| h <= hour);
                next = next.into_iter().chain(entry.states.get(after).map(|step| step.0)).min();
            }
        }
        (folded.total, folded.offset, live, unattributed, folded.gaps, next, folded.lines)
    };
    let assets = followed.is_some().then_some(prepared.assets.as_deref()).flatten();
    let (cache, records) = followed.map_or((None, None), |held| {
        ((!opened && indexed_records).then_some(&mut held.page), indexed_records.then_some(&held.records))
    });
    let (mut staged, planned) = StagedPage::new(cache, prepared, hour, planned);
    let locators = planned_locators(index, &planned, hour, records.map(|_| prepared.scanned.as_ref()));
    let read = match (locators.is_empty(), records) {
        (true, _) => std::collections::HashMap::new(),
        (false, None) => index::read_events(query, &locators)?,
        (false, Some(retained)) => match index::read_indexed_events(query, &locators, retained)? {
            Some(read) => read,
            None => index::read_events(query, &planned_locators(index, &planned, hour, None))?,
        },
    };
    let names = Names::new(&query.holders);
    let mut notices = Notices::default();
    let presenter = Presenter::new(query, content_from(&prepared.content, &mut notices), Arc::clone(&prepared.kept));
    let presenter = if opened { presenter } else { presenter.without_effects().with_assets(assets) };
    let mut events: Vec<&FeedEvent> = Vec::new();
    let mut seen: std::collections::HashMap<u128, usize> = std::collections::HashMap::new();
    let mut shapes: Vec<Option<(usize, Option<usize>)>> = Vec::with_capacity(planned.len());
    for line in &planned {
        let (at, with_choice) = match line {
            PlannedLine::Event { at, .. } => (*at, true),
            PlannedLine::Fold { first_at, .. } => (*first_at, false),
            PlannedLine::More { .. } => {
                shapes.push(None);
                continue;
            }
        };
        let Item::Log { head, edges, .. } = &index.get(at).payload else {
            shapes.push(None);
            continue;
        };
        let head_at = match seen.get(&head.key) {
            Some(&pos) => pos,
            None => {
                let pos = events.len();
                events.push(read.get(&head.key).ok_or_else(|| invalid("An event in the window was not read back"))?);
                seen.insert(head.key, pos);
                pos
            }
        };
        let choice = with_choice
            .then(|| {
                edges.iter().filter(|edge| edge.hour <= hour).filter_map(|edge| read.get(&edge.key)).rfind(|event| event.option.is_none_or(|option| option >= 0)).map(|event| {
                    events.push(event);
                    events.len() - 1
                })
            })
            .flatten();
        shapes.push(Some((head_at, choice)));
    }
    let (mut presented, mut evidence): (Vec<_>, Vec<_>) = present_rows(query, &prepared.scanned, &events, &names, &presenter, cancelled)?
        .rows.into_iter().zip(events.iter()).map(|(held, event)| {
            let (row, source) = held.map_or((None, None), |(row, source)| (row, Some((event.hour, source))));
            (row, source)
        }).unzip();
    if !opened {
        listed(&mut presented, false, true);
    }
    let projected = planned
        .into_iter()
        .zip(shapes)
        .map(|(line, shape)| {
            let mut projected = ProjectedLine::new(shape.and_then(|(head, _)| evidence[head].take()),
                shape.and_then(|(_, choice)| choice).and_then(|at| evidence[at].take()));
            projected.row = Some(match line {
                PlannedLine::Event { at, parent } => {
                    let occurrence = index.get(at);
                    let (key, mut row, mut choice) = match (&occurrence.payload, shape) {
                        (Item::Family(family), _) => (family.row.id.clone(), Some(projected.family(family)), None),
                        (Item::Log { .. }, Some((head_at, choice))) => {
                            let row = presented[head_at].take();
                            let choice = choice.and_then(|at| presented[at].take().map(|row| edge(events[at], row, prepared.scanned.partial_read(events[at].read_hour))));
                            (events[head_at].id(), row, choice)
                        }
                        _ => unreachable!(),
                    };
                    if !opened {
                        let chosen = choice.as_ref().and_then(|choice| choice.option).or_else(|| row.as_ref().and_then(|row| row.cause.as_ref()).and_then(|cause| cause.selected_option));
                        if let Some(item) = row.as_mut().and_then(|row| row.item.as_mut()) {
                            chosen_names(item, chosen);
                        }
                        let listed_options = row.as_ref().and_then(|row| row.item.as_ref()).is_some_and(|item| item.options.as_ref().is_some_and(|options| !options.is_empty()));
                        if let Some(choice) = choice.as_mut() {
                            choice.row.cause = None;
                            choice.row.window = None;
                            if listed_options {
                                choice.row.item = None;
                            }
                        }
                    }
                    let state = occurrence.state_at(hour);
                    let live = state.is_some_and(|state| index::LIVE.contains(&state));
                    let status = state.and_then(ScriptStatus::of_code);
                    let not_recorded_reason = (status == Some(ScriptStatus::NotRecorded))
                        .then(|| prepared.scanned.choice_break(occurrence.observed, hour).map(|(_, reason)| reason.to_owned()))
                        .flatten();
                    EventFeedLine::Event(Arc::new(EventFeedPageRow {
                        key,
                        hour: occurrence.date,
                        observed: occurrence.observed,
                        live,
                        row,
                        choice,
                        status,
                        not_recorded_reason,
                        parent,
                        place_name: None,
                        place_missing: None,
                    }))
                }
                PlannedLine::Fold { key, first_at, observed, count, kind, country } => {
                    let occurrence = index.get(first_at);
                    let mut first_row = match (&occurrence.payload, shape) {
                        (Item::Family(family), _) => Some(projected.family(family)),
                        (Item::Log { .. }, Some((head_at, _))) => presented[head_at].clone(),
                        _ => unreachable!(),
                    };
                    if !opened
                        && let Some(item) = first_row.as_mut().and_then(|row| row.item.as_mut())
                    {
                        chosen_names(item, None);
                    }
                    EventFeedLine::Fold(Arc::new(EventFeedFoldRow {
                        key,
                        hour: occurrence.date,
                        observed,
                        count: count.to_string(),
                        kind,
                        country,
                        first_row,
                    }))
                }
                PlannedLine::More { key, parent, hour, total, remaining } => EventFeedLine::More(EventFeedMoreRow {
                    key,
                    parent,
                    hour,
                    total: total.to_string(),
                    remaining: remaining.to_string(),
                }),
            });
            projected
        });
    let projected = staged.finish(projected);
    let art_warnings = projected.credit(&mut notices);
    let mut rows = projected.rows();
    content_refusals(&presenter, &mut notices)?;
    let from = params.from.unwrap_or(query.timeline.first);
    let to = params.to.map_or(hour, |to| to.min(hour));
    notices_for(prepared, unattributed, true, from, hour, &mut notices);
    families::unread(&params.categories, &mut notices);
    for (categories, kind, label, reason, count, span) in &feed.notices {
        if categories.iter().any(|category| params.categories.contains(category)) && span.is_none_or(|(a, b)| a <= to && b >= from) {
            notices.unavailable(kind, label, reason.clone(), *count);
        }
    }
    if params.categories.contains(&EventFeedCategory::WarsPeace) {
        let left_out = feed.unattributed.partition_point(|&at| at <= hour.min(params.to.unwrap_or(hour))).saturating_sub(feed.unattributed.partition_point(|&at| at < from));
        if left_out > 0 {
            notices.unavailable(
                "warUnattributed",
                "War starts and capitulations",
                "These war starts or capitulations name a country no link resolved at their date, so they are left out.",
                Some(u32::try_from(left_out).unwrap_or(u32::MAX)),
            );
        }
    }
    for family in feed.unrecorded.iter().filter(|family| params.categories.contains(&family.category)) {
        let hours: i64 = family.spans.iter().map(|&(first, last)| (i64::from(last.min(to)) - i64::from(first.max(from)) + 1).max(0)).sum();
        if hours > 0 {
            notices.unavailable(family.kind, family.label, format!("{} {hours} hours of this span {}.", family.listed, family.why), None);
        }
    }
    if params.place_names == Some(true) {
        attach_place_names(source, query, load, prepared, &mut rows, hour, cancelled)?;
    }
    let reply = EventFeedTailReply {
        branch_id: params.branch_id.clone(),
        total: total.to_string(),
        offset: reply_offset.to_string(),
        live: live_total.to_string(),
        counts: Vec::new(),
        rows,
        next_change: [next_change(prepared, base_next, true, hour), feed_next_change(feed, &params.categories, (from, params.to.unwrap_or(query.timeline.last).min(query.timeline.last)), hour), feed.unsettled().then(|| hour.saturating_add(1))].into_iter().flatten().min(),
        missing: gaps,
        unavailable: notices.into_list(),
        warnings: feed.warnings.iter().filter(|(at, _)| *at <= hour).map(|(_, text)| text.clone())
            .chain(art_warnings.into_iter().map(|(reason, rows)| format!("{reason} (count: {})", rows.len())))
            .collect(),
    };
    dashboard_base::products::check_reply_budget(&reply)?;
    projected.keep(&reply.rows);
    Ok(reply)
}

const STATUSES_LISTED: [ScriptStatus; 9] = [
    ScriptStatus::Active,
    ScriptStatus::Taken,
    ScriptStatus::Completed,
    ScriptStatus::Failed,
    ScriptStatus::Cancelled,
    ScriptStatus::TimedOut,
    ScriptStatus::Removed,
    ScriptStatus::Unsupported,
    ScriptStatus::NotRecorded,
];

fn chips(list: List) -> (Vec<Chip>, &'static [&'static str]) {
    match list {
        List::Decisions => (vec![Chip { kinds: None, states: Some(vec![ScriptStatus::Active.code()]) }], &["running"]),
        List::EventChoices => (vec![Chip { kinds: None, states: Some(vec![ScriptStatus::Awaiting.code()]) }], &["awaiting"]),
    }
}

fn only_row(scanned: &index::Scanned, index: &Index<Item>, row: Option<&str>, answer: &mut Answer, hour: i32) -> bool {
    let Some(row) = row else { return false };
    let found = scanned.locate(row).and_then(|key| index.find(key)).or_else(|| index::family_at(index, row)).filter(|&at| index.get(at).observed <= hour);
    answer.rows = found
        .map(|at| {
            let state = index.get(at).state_at(hour);
            Shown { at, ordinal: 0, live: state.is_some_and(|state| index::LIVE.contains(&state)), state }
        })
        .into_iter()
        .collect();
    true
}

pub(super) fn listed_choice(edges: &[ScriptEdge]) -> Option<i32> {
    edges.iter().rfind(|edge| edge.row.kind == EventFeedKind::EventOptionSelected && edge.option.is_none_or(|option| option >= 0)).and_then(|edge| edge.option)
}

fn chosen_names(item: &mut super::EventFeedItem, chosen: Option<i32>) {
    for option in item.options.iter_mut().flatten() {
        if chosen.is_none_or(|chosen| i64::from(chosen) != i64::from(option.ordinal)) {
            option.name = None;
        }
    }
    item.gaps.retain(|gap| {
        gap.field != super::EventFeedGapField::Option
            || chosen.is_some_and(|chosen| gap.ordinal.is_some_and(|ordinal| i64::from(chosen) == i64::from(ordinal)))
    });
}

pub(super) fn listed(rows: &mut [Option<EventFeedRow>], description: bool, window: bool) {
    for row in rows.iter_mut().flatten() {
        if let Some(cause) = row.cause.as_mut() {
            cause.notes.clear();
        }
        if row.cause.as_ref().is_some_and(|cause| {
            cause.how.is_none() && cause.within.is_none() && cause.conference.is_none() && cause.state.is_none() && cause.ally.is_none() && cause.reason.is_none() && cause.selected_option.is_none() && cause.target.is_none()
        }) {
            row.cause = None;
        }
        if !description && let Some(item) = row.item.as_mut() {
            item.description = None;
            item.gaps.retain(|gap| gap.field != super::EventFeedGapField::Description);
        }
        if !window {
            row.window = None;
        } else if let Some(presented) = row.window.as_mut().and_then(|window| window.presented.as_mut()) {
            presented.description = None;
            presented.options = None;
            presented.not_offered = None;
        }
        if matches!(row.decision_state, Some(super::DecisionLifecycleState::NotApplicable { .. })) {
            row.decision_state = None;
        }
    }
}

struct FollowedCards {
    events: Mutex<index::CardEvents>,
    reader: Option<IndexedStateRecords>,
}

fn load_cards_at(
    list: List, prepared: &Prepared, query: &Query<'_>, params: &ScriptTailParams, hour: i32,
    followed: Option<&FollowedCards>, cancelled: &AtomicBool,
) -> io::Result<ScriptOccurrenceTailReply> {
    dashboard_text::metadata::check_cancel(cancelled)?;
    if list == List::Decisions && params.events.is_some() {
        return Err(invalid("Decisions are not filtered by event kind"));
    }
    let kinds: Option<Vec<u16>> = params.events.as_ref().map(|events| events.iter().map(|event| kind_code(event.kind())).collect());
    let states: Option<Vec<u8>> = match (&params.statuses, list) {
        (Some(statuses), _) => Some(statuses.iter().map(|status| status.code()).collect()),
        (None, List::Decisions) => Some(STATUSES_LISTED.iter().map(|status| status.code()).collect()),
        (None, List::EventChoices) => None,
    };
    let (chips, keys) = chips(list);
    let window = Window { from: params.from, to: params.to, selection: prepared.selection.as_ref(), search: params.search.as_deref(), sort: params.sort.as_ref(), offset: params.offset.as_deref(), limit: params.limit, sortable: if list == List::Decisions { &["country", "item", "cost", "status"] } else { &["country", "item", "choice"] } };
    let mut answer = answer(&prepared.index, query, &window, hour, kinds.as_deref(), states.as_deref(), true, &chips)?;
    let opened_row = only_row(&prepared.scanned, &prepared.index, params.row.as_deref(), &mut answer, hour);
    let locators = locators(&prepared.index, &answer, hour);
    let mut kept = followed.map(|held| std::mem::take(&mut *held.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner)));
    let exact;
    let read = match kept.as_mut() {
        Some(kept) => kept.read(query, &locators, followed.and_then(|held| held.reader.as_ref()))?,
        None => {
            exact = index::read_events(query, &locators)?;
            &exact
        }
    };
    let names = Names::new(&query.holders);
    let mut notices = Notices::default();
    let presenter = Presenter::new(query, content_from(&prepared.content, &mut notices), Arc::clone(&prepared.kept));
    let presenter = if opened_row { presenter } else { presenter.without_effects() };
    let mut events: Vec<&FeedEvent> = Vec::new();
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    for shown in &answer.rows {
        let occurrence = prepared.index.get(shown.at);
        let Item::Log { head, edges, .. } = &occurrence.payload else { return Err(invalid("A card holds no feed rows")) };
        let first = events.len();
        for at in edges.iter().filter(|edge| edge.hour <= hour) {
            events.push(read.get(&at.key).ok_or_else(|| invalid("A card's edge was not read back"))?);
        }
        let opened = edges.first().is_some_and(|first| first == head) && events.get(first).is_some_and(|event| EventFeedKind::of_code(event.code).is_some_and(|kind| list.openers().contains(&kind)) && event.opener.is_none());
        spans.push((first, events.len(), opened));
    }
    let presented = present_all(query, &prepared.scanned, &events, &names, &presenter, &mut notices, cancelled)?;
    let mut rows = Vec::with_capacity(answer.rows.len());
    for (shown, (first, end, opened)) in answer.rows.iter().zip(spans) {
        let occurrence = prepared.index.get(shown.at);
        let mut card: Vec<Option<EventFeedRow>> = presented[first..end].to_vec();
        if list == List::Decisions {
            super::decision_state::enforce_same_incarnation(&events[first..end], &mut card);
        }
        if !opened_row {
            listed(&mut card, list == List::Decisions, list == List::Decisions);
        }
        let mut edges: Vec<ScriptEdge> = events[first..end].iter().zip(card).filter_map(|(event, row)| row.map(|row| edge(event, row, prepared.scanned.partial_read(event.read_hour)))).collect();
        if !opened_row {
            let chosen = listed_choice(&edges);
            let mut named = false;
            for edge in &mut edges {
                if named {
                    edge.row.item = None;
                }
                named |= edge.row.item.is_some();
                if let Some(item) = edge.row.item.as_mut() {
                    chosen_names(item, chosen);
                }
            }
        }
        let Item::Log { head, .. } = &occurrence.payload else { return Err(invalid("A card holds no feed rows")) };
        let id = read.get(&head.key).map(FeedEvent::id).ok_or_else(|| invalid("A card's head was not read back"))?;
        let status = shown.state.and_then(ScriptStatus::of_code);
        let not_recorded = (list == List::EventChoices && status == Some(ScriptStatus::NotRecorded))
            .then(|| prepared.scanned.choice_break(occurrence.observed, hour))
            .flatten();
        rows.push(ScriptPageRow {
            key: id.clone(),
            hour: occurrence.date,
            observed: occurrence.observed,
            live: shown.live,
            status,
            occurrence: (!edges.is_empty()).then(|| ScriptOccurrence { id, opened, edges }),
            not_recorded_reason: not_recorded.map(|(_, reason)| reason.to_owned()),
            not_recorded_hour: not_recorded.map(|(at, _)| at),
        });
    }
    content_refusals(&presenter, &mut notices)?;
    notices_for(prepared, answer.unattributed, list == List::EventChoices, params.from.unwrap_or(query.timeline.first), hour, &mut notices);
    let reply = ScriptOccurrenceTailReply {
        branch_id: params.branch_id.clone(),
        total: answer.total.to_string(),
        offset: answer.offset.to_string(),
        live: answer.live.to_string(),
        counts: occurrences::counts(&answer, keys),
        rows,
        next_change: next_change(prepared, answer.next_change, list == List::EventChoices, hour),
        missing: answer.gaps,
        unavailable: notices.into_list(),
        warnings: Vec::new(),
    };
    dashboard_base::products::check_reply_budget(&reply)?;
    if let Some((held, kept)) = followed.zip(kept) {
        *held.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = kept;
    }
    Ok(reply)
}

pub struct EventFeedTail;

impl Product for EventFeedTail {
    const KEY: &'static str = "event-feed-tail";
    const CONTENT_DEPENDENT: bool = true;
    const REPLAYS: bool = true;
    const PREPARE: Option<dashboard_base::products::read_ahead::Prepare<Self::Params>> = Some(|source, params, load| {
        let places = if params.place_names == Some(true) { place_name_runs::prepare(source, load)? } else { None };
        if params.row.is_none() {
            super::assets::reserve(source, branch_id(&params.branch_id)?, params.hour, load);
        }
        Ok(places)
    });
    type Params = EventFeedTailParams;
    const FIELDS: dashboard_base::products::Fields = FEED_FIELDS;
    type Reply = EventFeedTailReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        use dashboard_query::{log::occurrence::Direction, series::ranked::{GroupKind, GroupMembership}};
        vec![
            Country::decl(),
            GroupKind::decl(),
            GroupMembership::decl(),
            GroupSelection::decl(),
            Direction::decl(),
            OccurrenceSort::decl(),
            OccurrenceCount::decl(),
            PlacedGap::decl(),
            ScriptStatus::decl(),
            EventFeedTailParams::decl(),
            super::EventFeedCategory::decl(),
            EventFeedKind::decl(),
            super::EventFeedOption::decl(),
            super::EventFeedGapField::decl(),
            super::EventFeedGap::decl(),
            super::EventFeedItem::decl(),
            super::EventFeedScriptTarget::decl(),
            super::EventFeedCause::decl(),
            super::DecisionLifecycleState::decl(),
            super::EventWindowDecision::decl(),
            super::EventWindowKind::decl(),
            super::EventWindowOption::decl(),
            super::EventWindowPresented::decl(),
            super::EventFeedWindow::decl(),
            EventFeedRow::decl(),
            super::EventFeedStrike::decl(),
            EventFeedUnavailable::decl(),
            EventFeedPageRow::decl(),
            EventFeedFoldRow::decl(),
            EventFeedMoreRow::decl(),
            EventFeedLine::decl(),
            EventFeedTailReply::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        branch(&params.branch_id)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let branch = branch_id(&params.branch_id)?;
        let query = dashboard_base::queries::query(source, branch)?;
        let hour = occurrences::cutoff(&query, params.hour)?;
        let prepared = prepare(source, &query, branch, context, None, asked!(params), cancelled)?;
        load_feed_at(&prepared, &query, (source, context), params, hour, None, cancelled)
    }
}

fn parse<P: serde::de::DeserializeOwned>(name: &str, asked: &serde_json::Value) -> Result<P, String> {
    let mut request = asked.clone();
    request.as_object_mut().ok_or_else(|| format!("Invalid {name} panel"))?.remove("panel");
    dashboard_query::named::named(&request).map_err(|error| format!("Invalid {name} panel: {error}"))
}

pub fn event_feed_dated_panel(source: &Arc<Source>, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    let params: EventFeedTailParams = parse(EventFeedTail::KEY, asked)?;
    EventFeedTail::validate(&params)?;
    let ready = EventFeedTail::PREPARE.expect("event feed preparation")(source, &params, load).map_err(|error| error.to_string())?;
    let branch = branch_id(&params.branch_id).map_err(|error| error.to_string())?;
    let query = dashboard_base::queries::query(source, branch).map_err(|error| error.to_string())?;
    let prepare = move |source: &Source, query: &Query<'_>, load: &LoadContext, params: &EventFeedTailParams, cancelled: &AtomicBool| {
        let prepared = self::prepare(source, query, branch, load, None, asked!(params), cancelled)?;
        if params.place_names == Some(true) {
            let mut names = RowNames::following();
            names.hints(prepared.scanned.place_points().chain((0..prepared.index.len()).filter_map(|at| {
                match &prepared.index.get(at as u32).payload {
                    Item::Family(family) => place_point(&family.row),
                    Item::Log { .. } => None,
                }
            })));
            *prepared.places.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = names;
        }
        Ok::<_, io::Error>(prepared)
    };
    let (finish, prepared) = dashboard_base::products::Finish::open(source, load, EventFeedTail::CONTENT_DEPENDENT, || prepare(source, &query, load, &params, &AtomicBool::new(false)));
    let held = Mutex::new((finish, prepared.map_err(|error| error.to_string())?, FollowedFeed::new(source)));
    let owned = Owned::of(&query, source.replay());
    let (shared, panel_load) = (Arc::clone(source), LoadContext { content: load.content.clone() });
    place_name_runs::prepared_panel(ready, dashboard_base::products::at_hour::interval_panel(source.tables(), move |hour, cancelled| {
        let load = &panel_load;
        let query = owned.query();
        let hour = occurrences::cutoff(&query, Some(hour))?;
        let mut held = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.1.feed.as_ref().is_some_and(|feed| feed.unsettled()) {
            let (finish, prepared) = dashboard_base::products::Finish::open(&shared, load, EventFeedTail::CONTENT_DEPENDENT, || prepare(&shared, &query, load, &params, cancelled));
            *held = (finish, prepared.map_err(|error| dashboard_core::duplicate(&error))?, FollowedFeed::new(&shared));
        }
        let (finish, prepared, pages) = &mut *held;
        let reply = finish.reply(|| load_feed_at(prepared, &query, (&shared, load), &params, hour, Some(pages), cancelled))?;
        let through = match prepared.feed.as_ref().is_some_and(|feed| feed.unsettled()) {
            true => hour.saturating_sub(1),
            false => reply.next_change().map_or(i32::MAX, |next| next.saturating_sub(1)),
        };
        let rows = reply.row_count() as u64;
        Ok((serde_json::to_value(reply)?, hour..=through, rows))
    }))
}

fn cards_load(list: List, source: &Source, params: &ScriptTailParams, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<ScriptOccurrenceTailReply> {
    let branch = branch_id(&params.branch_id)?;
    let query = dashboard_base::queries::query(source, branch)?;
    let hour = occurrences::cutoff(&query, params.hour)?;
    let prepared = prepare(source, &query, branch, context, Some(list), asked!(params), cancelled)?;
    load_cards_at(list, &prepared, &query, params, hour, None, cancelled)
}

fn cards_dated_panel<P: Product>(list: List, source: &Source, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    let params: ScriptTailParams = parse(P::KEY, asked)?;
    branch(&params.branch_id)?;
    let branch = branch_id(&params.branch_id).map_err(|error| error.to_string())?;
    let query = dashboard_base::queries::query(source, branch).map_err(|error| error.to_string())?;
    let (finish, prepared) = dashboard_base::products::Finish::open(source, load, P::CONTENT_DEPENDENT, || prepare(source, &query, branch, load, Some(list), asked!(params), &AtomicBool::new(false)));
    let prepared = prepared.map_err(|error| error.to_string())?;
    let owned = Owned::of(&query, source.replay());
    let followed = FollowedCards {
        events: Mutex::default(),
        reader: (list == List::EventChoices && prepared.scanned.unique_record_keys)
            .then(|| IndexedStateRecords::new(source.replay(), replay_data::rows::TableId::FeedEvents)),
    };
    Ok(occurrences::occurrence_panel(source.tables(), finish, move |hour, cancelled| {
        let query = owned.query();
        let hour = occurrences::cutoff(&query, Some(hour))?;
        load_cards_at(list, &prepared, &query, &params, hour, Some(&followed), cancelled)
    }))
}

pub struct EventFeedActivity;

impl Product for EventFeedActivity {
    const KEY: &'static str = "event-feed-activity";
    const CONTENT_DEPENDENT: bool = true;
    type Params = EventFeedActivityParams;
    const FIELDS: dashboard_base::products::Fields =
        dashboard_base::fields![branchId, hour: Read, from: Start, binHours, categories, exclude, "exclude[]", countries, "countries[]", group];
    type Reply = EventFeedActivityReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![EventFeedActivityParams::decl(), EventFeedActivityLine::decl(), EventFeedActivityReply::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        branch(&params.branch_id)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let branch = branch_id(&params.branch_id)?;
        let query = dashboard_base::queries::query(source, branch)?;
        let hour = occurrences::cutoff(&query, Some(params.hour))?;
        if params.from > hour || params.bin_hours < 1 {
            return Err(invalid("Invalid activity span"));
        }
        let prepared = Self::prepare(source, &query, context, params, cancelled)?;
        Self::read(&prepared, &query, params, params.from, hour, cancelled)
    }
}

impl EventFeedActivity {
    fn prepare(source: &Source, query: &Query<'_>, context: &LoadContext, params: &EventFeedActivityParams, cancelled: &AtomicBool) -> io::Result<Prepared> {
        prepare(source, query, query.timeline.id, context, None, (params.countries.as_deref(), params.group.as_ref(), None), cancelled)
    }

    fn read(prepared: &Prepared, query: &Query<'_>, params: &EventFeedActivityParams, from: i32, hour: i32, cancelled: &AtomicBool) -> io::Result<EventFeedActivityReply> {
        if from > hour || params.bin_hours < 1 {
            return Err(invalid("Invalid activity span"));
        }
        let feed = prepared.feed.as_ref().ok_or_else(|| invalid("The event feed's index is not prepared"))?;
        let whole = Ask {
            hour,
            from: None,
            to: None,
            gap_end: None,
            selection: prepared.selection.as_ref(),
            subject: None,
            kinds: None,
            states: None,
            search: None,
            order: dashboard_query::log::occurrence::Order::Date(dashboard_query::log::occurrence::Direction::Descending),
            live_first: false,
            offset: 0,
            limit: 1,
            chips: &[],
        };
        let mut lines = Vec::with_capacity(params.categories.len());
        let mut missing = feed.index.coverage(&query.holders, &whole, from, hour);
        let base_missing = missing.len();
        for &category in &params.categories {
            dashboard_text::metadata::check_cancel(cancelled)?;
            let kinds = feed_kinds(&[category], params.exclude.as_deref())?;
            let line_ask = Ask { kinds: Some(&kinds), ..whole };
            let (before, bins) = feed.index.bins(&query.holders, &line_ask, from, hour, params.bin_hours)?;
            let latest_retirement = feed.index.latest_retirement(&query.holders, &line_ask, from);
            lines.push(EventFeedActivityLine { category, before: before.to_string(), bins, latest_retirement });
            let active_key = |key: &str| match key {
                "peace" => kinds.contains(&index::kind_code(EventFeedKind::Peace)),
                "factionMembership" => kinds.contains(&index::kind_code(EventFeedKind::FactionJoined)) || kinds.contains(&index::kind_code(EventFeedKind::FactionLeft)),
                "countryGroups" => kinds.contains(&index::kind_code(EventFeedKind::WarStarted)) || kinds.contains(&index::kind_code(EventFeedKind::Capitulated)),
                _ => false,
            };
            for family in feed.unrecorded.iter().filter(|f| f.category == category && active_key(f.kind)) {
                let reason = format!("{} {}.", family.label, family.why);
                for &(first, last) in &family.spans {
                    let (a, b) = (first.max(from), last.min(hour));
                    if a <= b { missing.push(dashboard_query::series::MissingSpan { first_hour: a, last_hour: b, reason: reason.clone() }); }
                }
            }
            for (_, _, _, reason, _, span) in feed.notices.iter().filter(|(cats, key, ..)| cats.contains(&category) && active_key(key)) {
                let (first, last) = span.unwrap_or((from, hour));
                let (a, b) = (first.max(from), last.min(hour));
                if a <= b { missing.push(dashboard_query::series::MissingSpan { first_hour: a, last_hour: b, reason: reason.clone() }); }
            }
        }
        if missing.len() > base_missing {
            missing.sort_by(|a, b| a.first_hour.cmp(&b.first_hour).then(a.last_hour.cmp(&b.last_hour)).then(a.reason.cmp(&b.reason)));
            let mut merged: Vec<dashboard_query::series::MissingSpan> = Vec::with_capacity(missing.len());
            for span in missing {
                match merged.last_mut() {
                    Some(held) if held.reason == span.reason && span.first_hour <= held.last_hour.saturating_add(1) => held.last_hour = held.last_hour.max(span.last_hour),
                    _ => merged.push(span),
                }
            }
            missing = merged;
        }
        Ok(EventFeedActivityReply {
            branch_id: params.branch_id.clone(),
            first_hour: (i64::from(from).div_euclid(params.bin_hours) * params.bin_hours) as i32,
            bin_hours: params.bin_hours,
            lines,
            missing,
            warnings: Vec::new(),
        })
    }
}

pub fn event_feed_activity_panel(source: &Arc<Source>, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    let mut request = asked.clone();
    let fields = request.as_object_mut().ok_or("Invalid event activity panel")?;
    if fields.insert("from".into(), 0.into()).is_some() {
        return Err("Followed event activity uses the rolling window".into());
    }
    fields.entry("hour").or_insert(0.into());
    let params: EventFeedActivityParams = parse(EventFeedActivity::KEY, &request)?;
    let branch = branch_id(&params.branch_id).map_err(|error| error.to_string())?;
    let query = dashboard_base::queries::query(source, branch).map_err(|error| error.to_string())?;
    let (finish, prepared) = dashboard_base::products::Finish::open(source, load, EventFeedActivity::CONTENT_DEPENDENT, || {
        EventFeedActivity::prepare(source, &query, load, &params, &AtomicBool::new(false))
    });
    let held = Mutex::new((finish, prepared.map_err(|error| error.to_string())?));
    let owned = Owned::of(&query, source.replay());
    let (shared, load) = (Arc::clone(source), LoadContext { content: load.content.clone() });
    Ok(dashboard_base::products::at_hour::interval_panel(source.tables(), move |hour, cancelled| {
        let query = owned.query();
        let hour = occurrences::cutoff(&query, Some(hour))?;
        let mut held = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.1.feed.as_ref().is_some_and(|feed| feed.unsettled()) {
            let (finish, prepared) = dashboard_base::products::Finish::open(&shared, &load, EventFeedActivity::CONTENT_DEPENDENT, || {
                EventFeedActivity::prepare(&shared, &query, &load, &params, cancelled)
            });
            *held = (finish, prepared?);
        }
        let from = hour.saturating_sub(60 * 24).div_euclid(24).saturating_mul(24).max(query.timeline.first);
        let reply = held.0.reply(|| EventFeedActivity::read(&held.1, &query, &params, from, hour, cancelled))?;
        let rows = reply.lines.iter().map(|line| line.bins.len() as u64).sum();
        Ok((serde_json::to_value(reply)?, hour..=hour, rows))
    }))
}

pub struct EventFeedKinds;

impl Product for EventFeedKinds {
    const KEY: &'static str = "event-feed-kinds";
    const CONTENT_DEPENDENT: bool = true;
    const REPLAYS: bool = true;
    type Params = dashboard_base::products::BranchParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId];
    type Reply = EventFeedKindsReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![EventFeedKindInfo::decl(), EventFeedKindsReply::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        branch(&params.branch_id)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let branch = branch_id(&params.branch_id)?;
        let query = dashboard_base::queries::query(source, branch)?;
        let content = super::resolve_content(source, context);
        let scanned = index::scanned_for(source, &query, branch, content.as_ref().ok(), cancelled)?;
        let feed = index::feed_for(source, &query, branch, context, content.as_ref().ok(), cancelled)?;
        let mut first = scanned.observed_kinds();
        for at in 0..feed.index.len() {
            let row = feed.index.get(at as u32);
            let hour = &mut first[usize::from(row.kind)];
            *hour = Some(hour.map_or(row.observed, |held| held.min(row.observed)));
        }
        let mut unread = Notices::default();
        families::unread(&[EventFeedCategory::WarsPeace, EventFeedCategory::Diplomacy, EventFeedCategory::Politics, EventFeedCategory::Milestones], &mut unread);
        let unread = unread.into_list();
        let kinds = EventFeedKind::ALL.into_iter().filter(|kind| !index::internal(*kind)).map(|kind| {
            use EventFeedKind::*;
            let first_observed = first[kind as usize];
            let unavailable = first_observed.is_none().then(|| {
                let key = match kind {
                    LawChanged | AdvisorHired => "lawChanged",
                    RulingPartyChanged => "rulingPartyChanged",
                    FocusCompleted => "focusCompleted",
                    NukesIncreased => "nukesIncreased",
                    Peace => "peace",
                    FactionJoined | FactionLeft => "factionMembership",
                    WarStarted | Capitulated => "countryGroups",
                    BecameSubject => "subjects",
                    ProjectFinished => "projectFinished",
                    NuclearStrike => "nuclearStrike",
                    _ => "feed-events",
                };
                if let Some(notice) = unread.iter().find(|notice| notice.kind == key) {
                    return notice.reason.clone();
                }
                if let Some((_, _, _, reason, _, _)) = feed.notices.iter().find(|(_, name, ..)| name == key) {
                    return reason.clone();
                }
                if let Some(family) = feed.unrecorded.iter().find(|family| family.kind == key && !family.spans.is_empty()) {
                    return format!("Events of this kind {}.", family.why);
                }
                let table = match key {
                    "feed-events" => Some(replay_data::rows::TableId::FeedEvents),
                    "nuclearStrike" => Some(replay_data::rows::TableId::NuclearStrikes),
                    _ => None,
                };
                if let Some(reason) = table.and_then(|table| query.replay.recording().predates(table.index())) {
                    return reason.to_owned();
                }
                if key == "feed-events" && let Some(gap) = scanned.gaps.first() {
                    return gap.reason.clone();
                }
                if let Some(gap) = query.timeline.unheld.first() {
                    return gap.reason.clone();
                }
                "No events of this kind were recorded in this branch.".to_owned()
            });
            EventFeedKindInfo { kind, category: kind.category(), first_observed, unavailable }
        }).collect();
        Ok(EventFeedKindsReply { kinds, warnings: Vec::new() })
    }
}

pub struct EventChoicesTail;

impl Product for EventChoicesTail {
    const KEY: &'static str = "event-choices-tail";
    const CONTENT_DEPENDENT: bool = true;
    const REPLAYS: bool = true;
    type Params = ScriptTailParams;
    const FIELDS: dashboard_base::products::Fields = CARD_FIELDS;
    type Reply = ScriptOccurrenceTailReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        use super::cards::{ScriptCost, ScriptCurrency};
        vec![ScriptEventKind::decl(), ScriptTailParams::decl(), ScriptCurrency::decl(), ScriptCost::decl(), ScriptEdge::decl(), ScriptOccurrence::decl(), ScriptPageRow::decl(), ScriptOccurrenceTailReply::decl()]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        branch(&params.branch_id)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        cards_load(List::EventChoices, source, params, context, cancelled)
    }
}

pub fn event_choices_dated_panel(source: &Source, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    cards_dated_panel::<EventChoicesTail>(List::EventChoices, source, load, asked)
}

pub struct DecisionsTail;

impl Product for DecisionsTail {
    const KEY: &'static str = "decisions-tail";
    const CONTENT_DEPENDENT: bool = true;
    const REPLAYS: bool = true;
    type Params = ScriptTailParams;
    const FIELDS: dashboard_base::products::Fields = CARD_FIELDS;
    type Reply = ScriptOccurrenceTailReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        EventChoicesTail::types()
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        branch(&params.branch_id)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        cards_load(List::Decisions, source, params, context, cancelled)
    }
}

pub fn decisions_dated_panel(source: &Source, load: &LoadContext, asked: &serde_json::Value) -> Result<Box<dyn Panel>, String> {
    cards_dated_panel::<DecisionsTail>(List::Decisions, source, load, asked)
}
