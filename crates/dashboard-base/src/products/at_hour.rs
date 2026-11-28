
use super::{Finish, LoadContext, PanelReader, Registered, ReplyWarnings, read_ahead::Preparation};
use crate::Source;
use dashboard_follow::{
    live::{Cells, Group, GroupState, Live},
    panel::{Declared, OnPrepared, Panel, PanelData, PendingPreparation, ProductRead, ProductReader, Wake},
};
use dashboard_query::{
    Query,
    identity::{Country, Holder, Holders},
};
use replay_data::rows::TableId;
use replay_frame::frame::{DecodedFrame, Metrics, not_selected};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    ops::RangeInclusive,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
#[cfg(any(test, feature = "test-support"))]
use std::marker::PhantomData;
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

mod incremental;
mod placed;
pub use placed::with_strip_pool;
#[cfg(any(test, feature = "test-support"))]
pub use placed::interactive_pool_snapshot;
mod validity;
pub(crate) use validity::reading_dates;
pub use validity::{DateSelection, ReadDates, ScriptedInputs, reads_through, row_change_hours};
pub use incremental::{DatedStep, Edits, Held};

pub use replay_frame::frame::Keys;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub table: TableId,
    pub keys: Keys,
}

impl Read {
    pub fn all(table: TableId) -> Self {
        Self { table, keys: Keys::All }
    }

    pub fn these(table: TableId, keys: impl IntoIterator<Item = u128>) -> Self {
        let mut keys: Vec<u128> = keys.into_iter().collect();
        keys.sort_unstable();
        keys.dedup();
        Self { table, keys: Keys::These(keys) }
    }
}

pub fn field_mask(table: TableId, fields: &[&str]) -> u64 {
    fields.iter().map(|field| 1u64 << dashboard_declare::field(table, field).expect("a declared product field")).fold(0, |a, b| a | b)
}

pub fn field_columns(table: TableId, fields: &[&str]) -> Vec<usize> {
    fields.iter().map(|field| dashboard_declare::field(table, field).expect("a declared product field")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub slot: u32,
    pub id: u32,
}

pub struct At<'a, C> {
    pub owner: Option<Owner>,
    pub holders: &'a Holders,
    pub hour: i32,
    pub context: &'a C,
    pub cancelled: &'a AtomicBool,
    pub live: &'a Cells,
}

pub struct ReadsAt<'a, C> {
    pub holders: &'a Holders,
    pub context: &'a C,
    pub run: u32,
    pub hour: i32,
    pub since: Option<i32>,
}

pub type PrepareHour<P> = fn(&Arc<Source>, &P, &LoadContext, i32) -> io::Result<Option<Arc<Preparation>>>;

pub trait AtHour: 'static {
    const KEY: &'static str;
    const TITLE: &'static str;
    const CONTENT_DEPENDENT: bool;
    const INTERACTIVE: bool = true;
    const PREPARE: Option<PrepareHour<Self::Params>> = None;
    const RESET_READERS: Option<fn(&Self::Context)> = None;
    const RELEASE_ON_GAP: Option<fn(&Source)> = None;
    const FIELDS: super::Fields;
    type Params: DeserializeOwned + Send + 'static;
    type Reply: Serialize + ReplyWarnings + PartialEq + Send + 'static;
    type Context: Send + 'static;

    fn validate(_params: &Self::Params) -> Result<(), &'static str> {
        Ok(())
    }

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String>
    where
        Self::Params: TS,
        Self::Reply: TS,
    {
        vec![Self::Params::decl(), Self::Reply::decl()]
    }

    fn owner(_params: &Self::Params) -> Option<&Country> {
        None
    }

    fn tables(params: &Self::Params) -> Vec<TableId>;

    fn reads(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let _ = (owner, held);
        if step == 0 { Self::tables(params).into_iter().map(Read::all).collect() } else { Vec::new() }
    }

    fn reads_in(params: &Self::Params, owner: Option<Owner>, step: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> Vec<Read> {
        let _ = at;
        Self::reads(params, owner, step, held)
    }

    fn retained_reads(params: &Self::Params, owner: Option<Owner>, stage: usize, held: &DecodedFrame, at: &ReadsAt<'_, Self::Context>) -> (Vec<Read>, i32) {
        (Self::reads_in(params, owner, stage, held, at), i32::MAX)
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        let _ = table;
        None
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> { Self::columns(table) }

    fn plan_changes(_stage: usize, edits: &Edits, _held: &DecodedFrame, _at: &ReadsAt<'_, Self::Context>) -> bool { !edits.tables.is_empty() || !edits.replaced.is_empty() }

    fn compares_previous(_table: TableId) -> bool { false }

    const DATE_HELD_REPLY: Option<fn(&mut Value, i32)> = None;

    fn valid_through(_frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>) -> i32 {
        i32::MAX
    }

    fn valid_reply_through(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>, _reply: &Self::Reply, partial: bool) -> i32 {
        if partial { frame.hour } else { Self::valid_through(frame, params, at) }
    }

    fn holds_refusal(_frame: &DecodedFrame) -> bool { false }

    const UPDATES_REVISION: bool = false;

    const RESEED_ON_SEEK: bool = false;

    fn context_revision(_context: &Self::Context) -> u64 { 0 }

    fn advance(_frame: &DecodedFrame, _params: &Self::Params, _at: &At<'_, Self::Context>, _edits: &Edits, _live: &Live, _data: &mut Held<'_>) -> io::Result<bool> {
        Ok(false)
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context>;

    fn context_for(source: &Source, load: &LoadContext, params: &Self::Params) -> io::Result<Self::Context> {
        let _ = params;
        Self::context(source, load)
    }

    fn playback_reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        Self::reply(frame, params, at)
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply>;
}

fn holder<P: AtHour>(holders: &Holders, params: &P::Params, hour: i32) -> io::Result<Option<Owner>> {
    let Some(country) = P::owner(params) else { return Ok(None) };
    match holders.at(country, hour).map(|run| run.holder) {
        Some(Holder::Found { slot, id }) => Ok(Some(Owner { slot, id })),
        Some(Holder::Unresolved) | None => Err(dashboard_core::refused(holders.not_known(country, hour).unwrap_or_else(|| format!("{} country is unavailable at this hour", P::TITLE)))),
    }
}

fn empty(branch: u64, hour: i32) -> DecodedFrame {
    DecodedFrame { branch, hour, tables: std::array::from_fn(|_| not_selected()), metrics: Metrics::default() }
}

fn converted<'a>(reads: &'a [Read], fields: &'a [Option<Vec<usize>>]) -> Vec<replay_lanes::at::Read<'a>> {
    reads
        .iter()
        .zip(fields)
        .map(|(read, fields)| replay_lanes::at::Read {
            table: read.table,
            columns: fields.as_deref(),
            keys: match &read.keys {
                Keys::All => replay_lanes::at::Keys::All,
                Keys::These(keys) => replay_lanes::at::Keys::These(keys),
                Keys::Range(lo, hi) => replay_lanes::at::Keys::Range(*lo, *hi),
                Keys::Ranges(ranges) => replay_lanes::at::Keys::Ranges(ranges),
            },
        })
        .collect()
}

thread_local! { static READ_ROWS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) }; }

pub fn counted<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let before = READ_ROWS.replace(Some(0));
    let value = work();
    let rows = READ_ROWS.replace(before).unwrap();
    (value, rows)
}

pub fn count_rows(rows: impl FnOnce() -> u64) {
    if let Some(n) = READ_ROWS.get() { READ_ROWS.set(Some(n + rows())); }
}

fn count_frame(frame: &DecodedFrame, reads: &[Read]) {
    count_rows(|| reads.iter().map(|r| match frame.tables[r.table.index()].held() {
        replay_frame::frame::TableView::Complete(rows) | replay_frame::frame::TableView::Truncated { rows, .. } => rows.len() as u64,
        _ => 0,
    }).sum());
}

fn frame_at(query: &Query<'_>, run: u32, hour: i32, reads: &[Read], columns: fn(TableId) -> Option<Vec<usize>>) -> io::Result<DecodedFrame> {
    let fields: Vec<Option<Vec<usize>>> = reads.iter().map(|read| columns(read.table)).collect();
    replay_lanes::at::frame(query.replay.recording(), run, hour, &converted(reads, &fields)).inspect(|frame| count_frame(frame, reads))
}

pub struct Frames<'q> {
    query: &'q Query<'q>,
    seeker: replay_lanes::at::Seeker<'q>,
}

impl<'q> Frames<'q> {
    pub fn new(query: &'q Query<'q>) -> io::Result<Self> {
        Ok(Self { query, seeker: replay_lanes::at::Seeker::new(query.replay.recording())? })
    }

    pub fn frame(&mut self, hour: i32, reads: &[Read], title: &str) -> io::Result<DecodedFrame> {
        self.frame_with(hour, reads, |_| None, title)
    }

    pub fn frame_with(&mut self, hour: i32, reads: &[Read], columns: impl Fn(TableId) -> Option<Vec<usize>>, title: &str) -> io::Result<DecodedFrame> {
        let run = self.query.timeline.at(hour).ok_or_else(|| dashboard_core::refused(format!("{title} hour is not recorded")))?.run;
        let fields: Vec<_> = reads.iter().map(|read| columns(read.table)).collect();
        dashboard_query::cost::storage(|| self.seeker.frame(run, hour, &converted(reads, &fields))).inspect(|frame| count_frame(frame, reads))
    }
}

pub fn frame(query: &Query<'_>, hour: i32, reads: &[Read], title: &str) -> io::Result<DecodedFrame> {
    frame_with(query, hour, reads, |_| None, title)
}

pub fn frame_with(query: &Query<'_>, hour: i32, reads: &[Read], columns: fn(TableId) -> Option<Vec<usize>>, title: &str) -> io::Result<DecodedFrame> {
    let run = query.timeline.at(hour).ok_or_else(|| dashboard_core::refused(format!("{title} hour is not recorded")))?.run;
    dashboard_query::cost::storage(|| frame_at(query, run, hour, reads, columns))
}

fn covers(read: &Read, group: &Group) -> bool {
    match &read.keys {
        Keys::All => true,
        Keys::These(keys) => group.keys.iter().all(|key| keys.binary_search(key).is_ok()),
        Keys::Range(low, high) => group.keys.iter().all(|key| low <= key && key <= high),
        Keys::Ranges(ranges) => group.keys.iter().all(|key| ranges.iter().any(|(low, high)| low <= key && key <= high)),
    }
}

fn answered<P: AtHour, C: std::borrow::Borrow<P::Context>>(query: &Query<'_>, pool: &Arc<placed::Pool>, params: &P::Params, hour: i32, context: impl FnOnce() -> io::Result<C>, cancelled: &AtomicBool) -> io::Result<(P::Reply, Option<Value>)> {
    let run = query.timeline.at(hour).ok_or_else(|| dashboard_core::refused(format!("{} hour is not recorded", P::TITLE)))?.run;
    let owner = holder::<P>(&query.holders, params, hour)?;
    let held = context()?;
    let context = held.borrow();
    let read_run = pool.pages.run_of(run).ok_or_else(|| dashboard_core::refused(format!("{} hour is in a run the recording cannot read", P::TITLE)))?;
    let mut count = 0;
    let mut input = incremental::Input { pool, run: read_run, hour, mode: incremental::capture_mode(&pool.pages)?, count: &mut count, step: None, placed: None, gap: false, exact: true };
    let mut answer = AnswerFrame::new(query.timeline.id, hour);
    let mut read_frame = |reads: &[Read]| dashboard_query::cost::storage(|| input.frame(reads, P::columns, query.timeline.id));
    for step in 0.. {
        crate::metadata::check_cancel(cancelled)?;
        let reads = P::reads_in(params, owner, step, &answer.frame, &ReadsAt { holders: &query.holders, context, run, hour, since: None });
        if reads.is_empty() {
            break;
        }
        answer.read(reads, &mut read_frame)?;
    }
    let cells = Cells::default();
    let reply = validity::without_read_dates(|| {
        P::reply(&answer.frame, params, &At { owner, holders: &query.holders, hour, context, cancelled, live: &cells })
    })?;
    let live = answer.live(&cells.finish(), &mut read_frame)?;
    Ok((reply, live))
}

struct AnswerFrame {
    frame: DecodedFrame,
    held: HashMap<TableId, Read>,
}

impl AnswerFrame {
    #[inline(never)]
    fn new(branch: u64, hour: i32) -> Self {
        Self { frame: empty(branch, hour), held: HashMap::new() }
    }

    #[inline(never)]
    fn read(&mut self, reads: Vec<Read>, read_frame: &mut impl FnMut(&[Read]) -> io::Result<DecodedFrame>) -> io::Result<()> {
        let read = read_frame(&reads)?;
        for asked in reads {
            self.frame.tables[asked.table.index()] = read.tables[asked.table.index()].clone();
            self.held.insert(asked.table, asked);
        }
        Ok(())
    }

    #[inline(never)]
    fn live(&mut self, live: &Live, read_frame: &mut impl FnMut(&[Read]) -> io::Result<DecodedFrame>) -> io::Result<Option<Value>> {
        if live.is_empty() {
            return Ok(None);
        }
        let reads = supplemental(live, |group| self.held.get(&group.table).is_some_and(|read| covers(read, group)));
        if !reads.is_empty() {
            self.read(reads, read_frame)?;
        }
        let states: Vec<GroupState> = live.values(&self.frame).into_iter().map(GroupState::Values).collect();
        Ok(Some(live.json(&states)))
    }
}

fn supplemental(live: &Live, covered: impl Fn(&Group) -> bool) -> Vec<Read> {
    let mut unread: BTreeMap<TableId, BTreeSet<u128>> = BTreeMap::new();
    for group in live.groups.iter().filter(|group| !covered(group)) {
        unread.entry(group.table).or_default();
    }
    for group in &live.groups {
        if let Some(keys) = unread.get_mut(&group.table) {
            keys.extend(&group.keys);
        }
    }
    unread.into_iter().map(|(table, keys)| Read::these(table, keys)).collect()
}

#[derive(Serialize, PartialEq)]
struct WithLive<R> {
    #[serde(flatten)]
    reply: R,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<Value>,
}

impl<R: ReplyWarnings> ReplyWarnings for WithLive<R> {
    fn warnings_mut(&mut self) -> &mut Vec<String> {
        self.reply.warnings_mut()
    }
}

fn request_hour(request: &mut Value) -> Option<i32> {
    let hour = request.as_object_mut()?.remove("hour")?;
    i32::try_from(hour.as_i64()?).ok()
}

fn load<P: AtHour>(source: &Source, lease_id: &str, mut request: Value, load: &LoadContext, cancelled: &AtomicBool) -> Result<String, dashboard_core::QueryError> {
    let hour = request_hour(&mut request).ok_or_else(|| format!("Invalid {} request: it names no hour", P::KEY))?;
    let params = dashboard_query::named::named::<P::Params>(&request).map_err(|error| format!("Invalid {} request: {error}", P::KEY))?;
    P::validate(&params).map_err(str::to_owned)?;
    super::reply_json(source, lease_id, P::KEY, P::CONTENT_DEPENDENT, load, cancelled, || {
        let query = crate::queries::query(source, dashboard_query::branch(&request)?)?;
        if let Some(release) = P::RELEASE_ON_GAP && query.timeline.at(hour).is_none() { release(source); }
        let pool = placed::Pool::of(source)?;
        answered::<P, _>(&query, &pool, &params, hour, || P::context_for(source, load, &params), cancelled).map(|(reply, live)| WithLive { reply, live })
    })
}

pub fn exact_panel<P: AtHour>(source: &Source, load: &LoadContext, asked: &Value) -> Result<Box<dyn Panel>, String>
where P::Params: Sync, P::Context: Sync {
    exact_panel_with::<P>(source, load, asked, None)
}

fn exact_panel_with<P: AtHour>(source: &Source, load: &LoadContext, asked: &Value, owned: Option<&Arc<Source>>) -> Result<Box<dyn Panel>, String>
where P::Params: Sync, P::Context: Sync {
    let mut request = asked.clone();
    if let Some(fields) = request.as_object_mut() { fields.remove("panel"); }
    let params = dashboard_query::named::named_fields::<P::Params>(&request).map_err(|reason| format!("Invalid {} panel: {reason}", P::KEY))?;
    P::validate(&params)?;
    let branch = dashboard_query::branch(asked).map_err(|e| e.to_string())?;
    let query = crate::queries::query(source, branch).map_err(|e| e.to_string())?;
    let (finish, context) = Finish::open(source, load, P::CONTENT_DEPENDENT, || P::context_for(source, load, &params));
    let context = Arc::new(context.map_err(|e| e.to_string())?);
    let params = Arc::new(params);
    let pool = placed::Pool::of(source).map_err(|e| e.to_string())?;
    let advance = shared_reader(source, load, P::KEY, asked, || incremental::reader::<P>(source, pool.clone(), params.clone(), context.clone(), Query { replay: query.replay, holders: query.holders.clone(), timeline: query.timeline.clone() }, finish.clone()));
    let (advance, waiting) = if let Some((prepare, source)) = P::PREPARE.zip(owned) {
        let (source, params, load) = (Arc::clone(source), Arc::clone(&params), LoadContext { content: load.content.clone() });
        let waiting = Arc::new(Awaiting::default());
        let reader = prepare_reader(advance, waiting.clone(), move |hour| source.in_scope(|| prepare(&source, &params, &load, hour)));
        (reader, Some(waiting))
    } else { (advance, None) };
    let (replay, holders, timeline, executable) = (source.replay(), query.holders, query.timeline, source.tables());
    let cold: dashboard_follow::panel::ExactReader = Arc::new(move |hour, cancelled| {
        if let Some(reset) = P::RESET_READERS && timeline.at(hour).is_none() { reset(&context); }
        let query = Query { replay: &replay, timeline: timeline.clone(), holders: holders.clone() };
        let reply = engine_executable::executable_tables::scope(executable, || finish.reply(||
            answered::<P, _>(&query, &pool, &params, hour, || Ok(context.as_ref()), cancelled).map(|(reply, live)| WithLive { reply, live }))).map_err(|e| e.to_string())?;
        serde_json::to_value(reply).map_err(|e| e.to_string())
    });
    Ok(with_pending_preparation(product_panel(advance, cold), waiting))
}

#[derive(Default)]
struct Awaiting(Mutex<Option<(i32, Arc<Preparation>, bool)>>);

impl Awaiting {
    fn on_prepared(&self, hour: i32) -> Option<OnPrepared> {
        let waiting = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_, build, _) = waiting.as_ref().filter(|(asked, ..)| *asked == hour)?;
        let build = Arc::clone(build);
        Some(Box::new(move |wake| build.on_ready(Box::new(move |_| wake()))))
    }

    fn check(&self, hour: i32, wake: Option<&Wake>, prepare: impl FnOnce() -> io::Result<Option<Arc<Preparation>>>) -> io::Result<()> {
        let mut waiting = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if waiting.as_ref().is_none_or(|(asked, ..)| *asked != hour) {
            let previous = waiting.take();
            *waiting = prepare()?.map(|build| {
                let subscribed = previous.is_some_and(|(_, old, subscribed)| subscribed && Arc::ptr_eq(&old, &build));
                (hour, build, subscribed)
            });
        }
        let Some((_, build, subscribed)) = waiting.as_mut() else { return Ok(()) };
        let Some(result) = build.result() else {
            if !*subscribed && let Some(wake) = wake.cloned() {
                build.on_ready(Box::new(move |_| wake()));
                *subscribed = true;
            }
            return Err(io::ErrorKind::WouldBlock.into());
        };
        *waiting = None;
        result
    }
}

fn prepare_reader(read: ProductReader, waiting: Arc<Awaiting>, prepare: impl Fn(i32) -> io::Result<Option<Arc<Preparation>>> + Send + Sync + 'static) -> ProductReader {
    Arc::new(move |hour, seed, cancelled, step| {
        match waiting.check(hour, step.and_then(|step| step.wake), || prepare(hour)) {
            Ok(()) => read(hour, seed, cancelled, step),
            Err(error) => {
                let pending = error.kind() == io::ErrorKind::WouldBlock;
                let value = if pending { Ok(Value::Null) } else { Err(error.to_string()) };
                ProductRead { value: Arc::new(value), rows: 0, fresh: !pending, pending, live: None, lanes: Vec::new(), from: hour }
            }
        }
    })
}

struct PreparedPanel {
    panel: Box<dyn Panel>,
    prepare: PendingPreparation,
}

impl Panel for PreparedPanel {
    fn exact(&self) -> Option<&dashboard_follow::panel::AtHour> { self.panel.exact() }
    fn pending_preparation(&self) -> Option<&PendingPreparation> { Some(&self.prepare) }
    fn tables(&self) -> &[TableId] { self.panel.tables() }
    fn data(&mut self, frame: &DecodedFrame) -> PanelData { self.panel.data(frame) }
}

fn with_pending_preparation(panel: Box<dyn Panel>, waiting: Option<Arc<Awaiting>>) -> Box<dyn Panel> {
    match waiting {
        Some(waiting) => Box::new(PreparedPanel { panel, prepare: Arc::new(move |hour| waiting.on_prepared(hour)) }),
        None => panel,
    }
}

pub fn interval_panel(
    executable: Option<engine_executable::executable_tables::Tables>,
    read: impl Fn(i32, &AtomicBool) -> io::Result<(Value, RangeInclusive<i32>, u64)> + Send + Sync + 'static,
) -> Box<dyn Panel> {
    dated_interval(executable, false, move |hour, cancelled, _| read(hour, cancelled), |_, _| {})
}

fn dated_interval(
    executable: Option<engine_executable::executable_tables::Tables>,
    seed_pending: bool,
    read: impl Fn(i32, &AtomicBool, Option<DatedStep<'_>>) -> io::Result<(Value, RangeInclusive<i32>, u64)> + Send + Sync + 'static,
    date: impl Fn(&mut Arc<PanelData>, i32) + Send + Sync + 'static,
) -> Box<dyn Panel> {
    use dashboard_follow::panel::{ExactReader, ProductRead, ProductReader};
    let read = Arc::new(move |hour, cancelled: &AtomicBool, step: Option<DatedStep<'_>>| {
        engine_executable::executable_tables::scope(executable, || read(hour, cancelled, step))
    });
    let cold_read = Arc::clone(&read);
    let cold: ExactReader = Arc::new(move |hour, cancelled| cold_read(hour, cancelled, None).map(|(data, _, _)| data).map_err(|error| error.to_string()));
    let held = Mutex::new((None::<(RangeInclusive<i32>, Arc<PanelData>)>, incremental::StepFrame::default()));
    let advance: ProductReader = Arc::new(move |hour, seed, cancelled, step| {
        let (held, frame) = &mut *held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if seed || step.is_some_and(|input| input.step.reset() || input.step.gap()) { *frame = incremental::StepFrame::default(); }
        if !seed && let Some((interval, value)) = &mut *held && interval.contains(&hour) {
            let undated = value.clone();
            date(value, hour);
            if !Arc::ptr_eq(&undated, value) { *interval = hour..=*interval.end(); }
            return ProductRead { value: value.clone(), rows: 0, fresh: false, pending: false, live: None, lanes: Vec::new(), from: *interval.start() };
        }
        let (value, rows, from) = match read(hour, cancelled, Some(DatedStep { step, frame })) {
            Ok((reply, proven, rows)) => {
                let value = Arc::new(Ok(reply));
                let holds = *proven.end() >= hour;
                let from = if holds { (*proven.start()).min(hour) } else { hour };
                *held = holds.then(|| (from..=*proven.end(), value.clone()));
                (value, rows, from)
            }
            Err(error) if (seed_pending || step.is_some()) && error.kind() == io::ErrorKind::WouldBlock => return ProductRead {
                value: Arc::new(Ok(Value::Null)), rows: 0, fresh: false, pending: true, live: None, lanes: frame.lanes(), from: hour,
            },
            Err(error) => {
                *held = None;
                *frame = incremental::StepFrame::default();
                (Arc::new(Err(error.to_string())), 0, hour)
            }
        };
        ProductRead { value, rows, fresh: true, pending: false, live: None, lanes: frame.lanes(), from }
    });
    product_panel(advance, cold)
}

pub fn at_hour(params: &mut Value, hour: i32) {
    params["hour"] = hour.into();
}

pub fn warnings_through<'a>(hour: i32, reasons: impl IntoIterator<Item = &'a str>) -> i32 {
    let stamp = format!("hour {hour} ");
    if reasons.into_iter().any(|reason| reason.contains(&stamp)) { hour } else { i32::MAX }
}

pub fn before(next: Option<i32>) -> i32 {
    next.map_or(i32::MAX, |n| n.saturating_sub(1))
}

pub fn day_panel<P: super::Product>(
    source: &Arc<Source>, load: &LoadContext, asked: &Value,
    start: &'static str, end: &'static str, through: fn(&P::Reply) -> i32,
) -> Result<Box<dyn Panel>, String> {
    dated_day_panel::<P>(source, load, asked, start, end, move |_, reply| through(reply), |_, _| {})
}

pub fn dated_day_panel<P: super::Product>(
    source: &Arc<Source>, load: &LoadContext, asked: &Value,
    start: &'static str, end: &'static str,
    through: impl Fn(&P::Params, &P::Reply) -> i32 + Send + Sync + 'static,
    date_held: impl Fn(&mut Arc<PanelData>, i32) + Send + Sync + 'static,
) -> Result<Box<dyn Panel>, String> {
    stepped_day_panel::<P>(source, load, asked, (start, end), through, date_held, |source, params, load, cancelled, _| P::load(source, params, load, cancelled))
}

pub fn stepped_day_panel<P: super::Product>(
    source: &Arc<Source>, load: &LoadContext, asked: &Value, (start, end): (&'static str, &'static str),
    through: impl Fn(&P::Params, &P::Reply) -> i32 + Send + Sync + 'static,
    date_held: impl Fn(&mut Arc<PanelData>, i32) + Send + Sync + 'static,
    load_step: impl Fn(&Source, &P::Params, &LoadContext, &AtomicBool, DatedStep<'_>) -> io::Result<P::Reply> + Send + Sync + 'static,
) -> Result<Box<dyn Panel>, String> {
    let first = dashboard_query::branch(asked).and_then(|branch| crate::queries::query(source, branch))
        .map_err(|e| e.to_string())?.timeline.first;
    let day_start = |hour: i32| hour.div_euclid(24).saturating_mul(24);
    let Some(tile_first) = asked[start].as_i64() else {
        return dated_product::<P>(source, load, asked, move |request, hour| {
            request[start] = first.max(day_start(hour)).into();
            request[end] = hour.into();
        }, through, date_held, load_step);
    };
    let tile_first = i32::try_from(tile_first).map_err(|_| "Invalid history start")?;
    let tile_last = asked[end].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or("Invalid history end")?;
    let mut params = asked.clone();
    params.as_object_mut().ok_or("Expected history parameters")?.remove("panel");
    let source = Arc::clone(source);
    let load = LoadContext { content: load.content.clone() };
    Ok(interval_panel(source.tables(), move |hour, cancelled| {
        let today = day_start(hour);
        if today <= tile_first {
            return Ok((Value::Null, hour..=day_start(tile_first).saturating_add(23), 0));
        }
        let mut request = params.clone();
        let last = tile_last.min(today.saturating_sub(1));
        request[end] = last.into();
        let (reply, rows) = counted(|| {
            crate::metadata::check_cancel(cancelled)?;
            super::register::<P>().answer(&source, request, &load, cancelled)
        });
        Ok((reply?, hour..=if last == tile_last { i32::MAX } else { today.saturating_add(23) }, rows))
    }))
}

pub fn product_interval_panel<P: super::Product>(
    source: &Arc<Source>, load: &LoadContext, asked: &Value,
    date: impl Fn(&mut Value, i32) + Send + Sync + 'static,
    valid_through: fn(&P::Reply) -> i32,
) -> Result<Box<dyn Panel>, String> {
    dated_product::<P>(source, load, asked, date, move |_, reply| valid_through(reply), |_, _| {}, |source, params, load, cancelled, _| P::load(source, params, load, cancelled))
}

pub fn dated_product<P: super::Product>(
    source: &Arc<Source>, load: &LoadContext, asked: &Value,
    date: impl Fn(&mut Value, i32) + Send + Sync + 'static,
    valid_through: impl Fn(&P::Params, &P::Reply) -> i32 + Send + Sync + 'static,
    date_held: impl Fn(&mut Arc<PanelData>, i32) + Send + Sync + 'static,
    load_step: impl Fn(&Source, &P::Params, &LoadContext, &AtomicBool, DatedStep<'_>) -> io::Result<P::Reply> + Send + Sync + 'static,
) -> Result<Box<dyn Panel>, String> {
    let mut params = asked.clone();
    params.as_object_mut().ok_or("Expected panel parameters")?.remove("panel");
    let source = Arc::clone(source);
    let load = LoadContext { content: load.content.clone() };
    let finish = Finish::new(&source, &load, P::CONTENT_DEPENDENT);
    let waiting = P::PREPARE.map(|_| Arc::new(Awaiting::default()));
    let pending = waiting.clone();
    let panel = dated_interval(source.tables(), pending.is_some(), move |hour, cancelled, step| {
        let mut request = params.clone();
        date(&mut request, hour);
        let params = dashboard_query::named::named::<P::Params>(&request)?;
        P::validate(&params).map_err(dashboard_core::invalid)?;
        #[cfg(any(test, feature = "test-support"))]
        crate::test_fixtures::product_called();
        let (reply, rows) = counted(|| finish.reply(|| {
            let Some(step) = step else { return P::load(&source, &params, &load, cancelled) };
            if let Some((prepare, waiting)) = P::PREPARE.zip(pending.as_ref()) {
                waiting.check(hour, step.step.and_then(|step| step.wake), || prepare(&source, &params, &load))?;
            }
            load_step(&source, &params, &load, cancelled, step)
        }));
        let reply = reply?;
        let through = valid_through(&params, &reply);
        Ok((serde_json::to_value(reply)?, hour..=through, rows))
    }, date_held);
    Ok(with_pending_preparation(panel, waiting))
}

pub fn product_panel(advance: dashboard_follow::panel::ProductReader, cold: dashboard_follow::panel::ExactReader) -> Box<dyn Panel> {
    let current = advance.clone();
    let fallback = cold.clone();
    let read = Arc::new(move |hour, cancelled: &AtomicBool| {
        let result = current(hour, true, cancelled, None);
        if result.pending { fallback(hour, cancelled) } else { result.value.as_ref().clone() }
    });
    Box::new(dashboard_follow::panel::AtHour { read, cold, advance })
}

pub fn shared_reader(source: &Source, load: &LoadContext, product: &str, params: &Value, make: impl FnOnce() -> dashboard_follow::panel::ProductReader) -> dashboard_follow::panel::ProductReader {
    type WeakReader = std::sync::Weak<dyn Fn(i32, bool, &AtomicBool, Option<dashboard_follow::panel::ProductStep<'_>>) -> dashboard_follow::panel::ProductRead + Send + Sync>;
    #[derive(Default)]
    struct Shared(HashMap<String, WeakReader>);
    let mut args = params.clone();
    if let Some(obj) = args.as_object_mut() { obj.remove("panel"); }
    let key = format!("{product} {args} {:?}", load.content.as_ref().map(|c| (&c.options.game_dir, &c.options.user_dir, &c.options.cache_dir, c.allow_unverified, Arc::as_ptr(c.presentation.binding()), c.presentation.identity())));
    let shared = source.state::<Shared>();
    let mut cache = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.0.retain(|_, r| r.strong_count() > 0);
    if let Some(reader) = cache.0.get(&key).and_then(WeakReader::upgrade) { return reader; }
    let reader = make();
    cache.0.insert(key, Arc::downgrade(&reader));
    reader
}

#[cfg(any(test, feature = "test-support"))]
struct Followed<P: AtHour> {
    params: P::Params,
    tables: Vec<TableId>,
    holders: Arc<Holders>,
    context: P::Context,
    finish: Finish,
    live: Option<Live>,
    executable: Option<engine_executable::executable_tables::Tables>,
    product: PhantomData<fn() -> P>,
}

#[cfg(any(test, feature = "test-support"))]
impl<P: AtHour> Panel for Followed<P> {
    fn tables(&self) -> &[TableId] {
        &self.tables
    }

    fn data(&mut self, frame: &DecodedFrame) -> PanelData {
        let cancelled = AtomicBool::new(false);
        let cells = Cells::default();
        self.live = None;
        let made = engine_executable::executable_tables::scope(self.executable, || {
            self.finish.reply(|| {
                let owner = holder::<P>(&self.holders, &self.params, frame.hour)?;
                P::reply(frame, &self.params, &At { owner, holders: &self.holders, hour: frame.hour, context: &self.context, cancelled: &cancelled, live: &cells })
            })
        });
        let reply = made.map_err(|error| error.to_string())?;
        self.live = Some(cells.finish()).filter(|live| !live.is_empty());
        serde_json::to_value(&reply).map_err(|error| error.to_string())
    }

    fn live(&self) -> Option<&Live> {
        self.live.as_ref()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn frame_panel<P: AtHour>(source: &Source, load: &LoadContext, asked: &Value) -> Result<Box<dyn Panel>, String> {
    let mut asked = asked.clone();
    if let Some(fields) = asked.as_object_mut() {
        fields.remove("panel");
    }
    let params = dashboard_query::named::named::<P::Params>(&asked).map_err(|error| format!("Invalid {} panel: {error}", P::KEY))?;
    P::validate(&params).map_err(str::to_owned)?;
    let branch = dashboard_query::branch(&asked).map_err(|error| error.to_string())?;
    let holders = Arc::clone(&crate::queries::query(source, branch).map_err(|error| error.to_string())?.holders);
    let (finish, context) = Finish::open(source, load, P::CONTENT_DEPENDENT, || P::context_for(source, load, &params));
    let context = context.map_err(|error| error.to_string())?;
    Ok(Box::new(Followed::<P> { tables: P::tables(&params), params, holders, context, finish, live: None, executable: source.tables(), product: PhantomData }))
}

#[cfg(any(test, feature = "export-types"))]
fn types<P: AtHour>() -> Vec<String>
where
    P::Params: TS,
    P::Reply: TS,
{
    let mut out = P::types();
    out.push(format!("type {0}AtHour = {0} & {{ hour: number }};", <P::Params as TS>::name()));
    out
}

pub const fn exact<P: AtHour>() -> PanelReader
where P::Params: Sync, P::Context: Sync {
    PanelReader { open: |source, load, asked| exact_panel_with::<P>(source, load, asked, Some(source)) }
}

fn prepare_exact<P: AtHour>(source: &Arc<Source>, request: &Value, load: &LoadContext) -> io::Result<Option<Arc<Preparation>>> {
    let mut request = request.clone();
    let hour = request_hour(&mut request).ok_or_else(|| dashboard_core::invalid(format!("Invalid {} request: it names no hour", P::KEY)))?;
    let params = dashboard_query::named::named::<P::Params>(&request)
        .map_err(|error| dashboard_core::invalid(format!("Invalid {} request: {error}", P::KEY)))?;
    P::validate(&params).map_err(dashboard_core::invalid)?;
    P::PREPARE.expect("registered preparation")(source, &params, load, hour)
}

pub const fn entry<P: AtHour>() -> Registered {
    Registered {
        panel: None,
        prepare: if P::PREPARE.is_some() { Some(prepare_exact::<P>) } else { None },
        key: P::KEY,
        content_dependent: P::CONTENT_DEPENDENT,
        interactive: P::INTERACTIVE,
        replays: false,
        load: load::<P>,
        fields: P::FIELDS,
        read_hour: true,
        #[cfg(any(test, feature = "export-types"))]
        types: None,
        #[cfg(any(test, feature = "export-types"))]
        params: dashboard_query::fields::walk::<P::Params>,
    }
}

#[cfg(any(test, feature = "export-types"))]
pub const fn entry_for_export<P: AtHour>() -> Registered
where P::Params: TS, P::Reply: TS {
    Registered { types: Some(types::<P>), ..entry::<P>() }
}

#[cfg(test)]
mod tests;
