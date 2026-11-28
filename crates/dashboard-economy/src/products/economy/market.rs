
pub mod ledger;

use dashboard_base::products::Country;
use dashboard_base::{Source, catalogue::{self, OwnRefusals}, products::{LoadContext, check_reply_budget, at_hour::{At, AtHour, Owner, Read}}};
use dashboard_world::products::equipment_names::{EquipmentIcon, VariantName, self as names, Named};
use dashboard_follow::live::{Cells, Source as Cell};
use dashboard_query::{
    identity::{Holder, Holders, NOT_HELD_AT_HOUR, NOT_IN_HISTORY, TAG_UNLINKED},
    series::ranked::GroupSelection,
};
use dashboard_world::products::group_members::GroupsAt;
use dashboard_text::{
    metadata::link_slot,
    variant_names::{Texts, localise_for},
};
use replay_data::rows::{CountryLinkRow, LendLeaseAgreementRow, LexicalTokenRow, MarketContractTermsRow, MarketPool, MarketPriceLevel, MarketSubsidy, TableId, tables};
use replay_frame::frame::{DecodedFrame, TableView, ViewIdentity};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    io,
    sync::{Arc, Mutex, PoisonError},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum MarketSection {
    Offers,
    Contracts,
    Requests,
    Agreements,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketLendLeaseParams {
    pub branch_id: String,
    #[doc = " Rows these countries take part in (as seller or buyer, giver or receiver); every row when"]
    #[doc = " empty."]
    pub countries: Vec<Country>,
    #[doc = " The one section read (`contracts` and `requests` are the two kinds of terms); every section when absent."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub only: Option<MarketSection>,
    #[doc = " Lend-lease rows the chosen countries give (`sent`) or receive (`received`); either side when absent."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub lease_side: Option<LeaseSide>,
    #[doc = " The group whose members at this hour take part in the rows; never with `countries`."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub group: Option<GroupSelection>,
    #[doc = " The one row read, by its recorded key in the `only` section: an offer's seller raw id, a"]
    #[doc = " contract's `key`. With it, `countries` is empty."]
    #[serde(default)]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub row: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum LeaseSide {
    Sent,
    Received,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketItem {
    pub variant: String,
    pub name: Option<String>,
    pub amount_raw: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketLevel {
    pub variant: String,
    pub name: Option<String>,
    #[doc = " 0 low, 1 normal, 2 high."]
    pub level: i32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketGrant {
    pub cic_raw: String,
    pub archetype: Option<String>,
    pub targets: Vec<String>,
    pub trigger: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketOffer {
    #[doc = " The seller's raw country id."]
    pub country: u32,
    pub tag: Option<String>,
    pub inventory: Vec<MarketItem>,
    pub prices: Vec<MarketLevel>,
    pub subsidies: Vec<MarketGrant>,
    pub subsidy_total_raw: String,
    pub auto_send_market_access: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketTerms {
    pub key: String,
    #[doc = " The handle, hexadecimal."]
    pub id: String,
    pub seller: Option<String>,
    pub buyer: Option<String>,
    pub request: bool,
    pub speed: i32,
    pub equipment: Vec<MarketItem>,
    #[doc = " Agreed prices, CIC."]
    pub prices: Vec<MarketItem>,
    pub levels: Vec<MarketLevel>,
    pub subsidies: Vec<MarketGrant>,
    #[doc = " Whether a purchase contract stands for it: its progress is in the live cells."]
    pub purchase: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct LeaseTerms {
    pub key: String,
    #[doc = " The handle, hexadecimal."]
    pub id: String,
    pub giver: Option<String>,
    #[doc = " Why `giver` is null at this hour."]
    pub giver_missing: Option<String>,
    pub receiver: Option<String>,
    #[doc = " Why `receiver` is null at this hour."]
    pub receiver_missing: Option<String>,
    pub revision: String,
    pub active_since: i32,
    pub last_delivery: i32,
    pub monthly: Vec<MarketItem>,
    #[doc = " Production shares, /1000 percent."]
    pub percentage: Vec<MarketItem>,
    pub once: Vec<MarketItem>,
    pub delivered: Vec<MarketItem>,
    pub sunk: Vec<MarketItem>,
    pub fuel_daily_raw: String,
    pub fuel_percentage_raw: String,
    pub last_fuel_delivered_raw: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MarketLendLeaseReply {
    pub branch_id: String,
    #[doc = " Null where the section was not asked or not recorded at the hour (`warnings` says which)."]
    pub offers: Option<Vec<MarketOffer>>,
    pub contracts: Option<Vec<MarketTerms>>,
    pub agreements: Option<Vec<LeaseTerms>>,
    #[doc = " Why `agreements` may lack rows at this hour: some were not read, so an empty list is not proof of none."]
    pub agreements_missing: Option<String>,
    #[doc = " Why purchase progress is not known at the hour, when contracts are shown and it is not."]
    pub purchases_missing: Option<String>,
    #[doc = " Rows left out because a country on the chosen side has no country at the hour: a chosen country may be theirs."]
    pub unattributed: u32,
    #[doc = " Why the asked group's members are not known at this hour; every section is then null."]
    pub refused: Option<String>,
    pub variants: Arc<[VariantName]>,
    pub icons: Arc<[EquipmentIcon]>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(MarketLendLeaseReply);

pub(crate) struct Parties<'a> {
    links: Option<&'a [CountryLinkRow]>,
    holders: &'a Holders,
    hour: i32,
}

impl<'a> Parties<'a> {
    pub(crate) fn new(links: Option<&'a [CountryLinkRow]>, holders: &'a Holders, hour: i32) -> Self {
        Self { links, holders, hour }
    }

    fn slot(&self, raw: u32) -> Option<u32> {
        link_slot(self.links?, raw)
    }

    pub(crate) fn tag(&self, raw: u32) -> Option<String> {
        self.holders.slot_tag(self.slot(raw)?, self.hour).map(str::to_owned)
    }

    pub(crate) fn named(&self, raw: u32) -> (Option<String>, Option<String>) {
        let Some(links) = self.links else { return (None, Some("Country links are not recorded at this hour".to_owned())) };
        let Some(slot) = link_slot(links, raw) else { return (None, Some(format!("Country reference {raw} has no country link at this hour"))) };
        match self.holders.slot_tag(slot, self.hour) {
            Some(tag) => (Some(tag.to_owned()), None),
            None => (None, Some(self.holders.unnamed(&Country::Slot(slot), self.hour))),
        }
    }
}

pub(crate) fn pool_variants(pool: &MarketPool) -> impl Iterator<Item = u64> + '_ {
    pool.items.iter().map(|item| item.variant).filter(|&variant| variant != 0)
}

pub(crate) fn lease_variants(row: &LendLeaseAgreementRow) -> impl Iterator<Item = u64> + '_ {
    [&row.monthly, &row.percentage, &row.once, &row.delivered, &row.sunk].into_iter().flat_map(|pool| pool_variants(pool))
}

fn pool(named: &MarketDesigns, pool: &MarketPool) -> Vec<MarketItem> {
    pool.items.iter().map(|item| MarketItem { variant: item.variant.to_string(), name: named.name(item.variant), amount_raw: item.amount_raw.to_string() }).collect()
}

fn levels<'r>(named: &MarketDesigns, rows: impl IntoIterator<Item = &'r MarketPriceLevel>) -> Vec<MarketLevel> {
    rows.into_iter().map(|row| MarketLevel { variant: row.variant.to_string(), name: named.name(row.variant), level: row.level }).collect()
}

fn grants<'r>(rows: impl IntoIterator<Item = &'r MarketSubsidy>, parties: &Parties<'_>, lexical: Option<&[LexicalTokenRow]>, texts: Option<&dyn Texts>) -> Vec<MarketGrant> {
    rows.into_iter()
        .map(|row| MarketGrant {
            cic_raw: row.cic_raw.to_string(),
            archetype: lexical
                .and_then(|lexical| lexical.binary_search_by_key(&row.archetype, |token| token.token).ok().map(|at| &lexical[at]))
                .and_then(|token| std::str::from_utf8(&token.literal).ok())
                .and_then(|key| texts.and_then(|texts| localise_for(texts, None, key))),
            targets: row.targets.iter().map(|&raw| parties.tag(raw).unwrap_or_else(|| format!("Country {raw}: name unavailable"))).collect(),
            trigger: (row.kind == 1).then(|| String::from_utf8_lossy(&row.trigger).into_owned()),
        })
        .collect()
}

fn lease_terms(row: &LendLeaseAgreementRow, party: impl Fn(u32) -> (Option<String>, Option<String>), named: &MarketDesigns) -> LeaseTerms {
    let ((giver, giver_missing), (receiver, receiver_missing)) = (party(row.country), party(row.receiver));
    LeaseTerms {
        key: lease_key(row).to_string(),
        id: format!("{:016x}", row.id),
        giver,
        giver_missing,
        receiver,
        receiver_missing,
        revision: row.revision.to_string(),
        active_since: row.active_since,
        last_delivery: row.last_delivery,
        monthly: pool(named, &row.monthly),
        percentage: pool(named, &row.percentage),
        once: pool(named, &row.once),
        delivered: pool(named, &row.delivered),
        sunk: pool(named, &row.sunk),
        fuel_daily_raw: row.fuel_daily_raw.to_string(),
        fuel_percentage_raw: row.fuel_percentage_raw.to_string(),
        last_fuel_delivered_raw: row.last_fuel_delivered_raw.to_string(),
    }
}

fn terms_key(row: &MarketContractTermsRow) -> u128 {
    u128::from(row.country) << 72 | u128::from(row.id) << 8 | u128::from(row.kind)
}

pub(crate) fn lease_key(row: &LendLeaseAgreementRow) -> u128 {
    u128::from(row.country) << 64 | u128::from(row.id)
}

fn row_key(params: &MarketLendLeaseParams) -> Option<u128> {
    params.row.as_deref().and_then(|row| row.parse().ok())
}

fn read_section(params: &MarketLendLeaseParams, table: TableId) -> Read {
    match row_key(params) {
        Some(key) => Read::these(table, [key]),
        None => Read::all(table),
    }
}

fn sections(params: &MarketLendLeaseParams) -> [bool; 3] {
    match params.only {
        None => [true; 3],
        Some(MarketSection::Offers) => [true, false, false],
        Some(MarketSection::Contracts | MarketSection::Requests) => [false, true, false],
        Some(MarketSection::Agreements) => [false, false, true],
    }
}

fn terms_kind(params: &MarketLendLeaseParams, row: &MarketContractTermsRow) -> bool {
    match (params.only, &params.row) {
        (Some(MarketSection::Contracts), None) => row.kind != 0,
        (Some(MarketSection::Requests), None) => row.kind == 0,
        _ => true,
    }
}

pub struct MarketContext {
    pub(crate) names: names::Context,
    pub(crate) groups: GroupsAt,
    designs: Mutex<Option<KeptDesigns>>,
}

struct MarketDesigns {
    ids: Vec<u64>,
    variants: Arc<[VariantName]>,
    icons: Arc<[EquipmentIcon]>,
    warnings: BTreeSet<String>,
}

impl MarketDesigns {
    fn read(frame: &DecodedFrame, ids: &[u64], context: &names::Context) -> Self {
        let mut named = Named::default();
        names::name(frame, ids, &[], context, &mut named);
        let (ids, variants): (Vec<_>, Vec<_>) = named.variants.into_iter()
            .map(|(id, variant)| (id, VariantName { hour: None, ..variant })).unzip();
        let icons = named.icons.into_iter().map(|(key, image)| EquipmentIcon { key, image }).collect();
        Self { ids, variants: variants.into(), icons, warnings: named.warnings }
    }

    fn name(&self, id: u64) -> Option<String> {
        self.ids.binary_search(&id).ok().and_then(|index| self.variants[index].name.clone())
    }
}

struct KeptDesigns {
    inputs: Vec<ViewIdentity>,
    value: Arc<MarketDesigns>,
    noted: OwnRefusals,
}

fn can_keep_designs(frame: &DecodedFrame) -> bool {
    [TableId::Countries, TableId::CountryLinks, TableId::LexicalTokens, TableId::VariantDefinitions].into_iter()
        .all(|table| matches!(frame.tables[table.index()].held(), TableView::Complete(rows) if rows.table() == table))
        && names::TABLES.iter().all(|table| !matches!(
            frame.tables[table.index()].held(), TableView::Unavailable { reason } if reason.contains("hour ")
        ))
}

impl MarketContext {
    pub(super) fn new(names: names::Context, groups: GroupsAt) -> Self {
        Self { names, groups, designs: Mutex::default() }
    }

    fn designs(&self, frame: &DecodedFrame, ids: &[u64], reuse: bool) -> Arc<MarketDesigns> {
        let read = || Arc::new(MarketDesigns::read(frame, ids, &self.names));
        if !reuse { return read(); }
        let mut kept = self.designs.lock().unwrap_or_else(PoisonError::into_inner);
        if ids.is_empty() || !can_keep_designs(frame) {
            *kept = None;
            return read();
        }
        let views = || names::TABLES.iter().map(|table| &frame.tables[table.index()]);
        if let Some(held) = kept.as_ref().filter(|held| held.value.ids == ids && held.inputs.iter().zip(views()).all(|(input, view)| input.is(view))) {
            catalogue::credit(&held.noted);
            return Arc::clone(&held.value);
        }
        let (value, noted) = catalogue::capture(read);
        *kept = Some(KeptDesigns { inputs: views().map(|view| view.identity()).collect(), value: Arc::clone(&value), noted });
        value
    }
}

fn section<'f, R>(shown: bool, rows: io::Result<&'f [R]>, label: &str, warnings: &mut Vec<String>) -> Option<&'f [R]> {
    if !shown {
        return None;
    }
    rows.map_err(|error| warnings.push(format!("{label} are not recorded at this hour: {error}"))).ok()
}

fn chosen(holders: &Holders, tags: &[Country], hour: i32, warnings: &mut Vec<String>) -> Result<Option<BTreeSet<u32>>, String> {
    if tags.is_empty() {
        return Ok(None);
    }
    slots(holders, tags, hour, warnings).map(Some)
}

fn slots(holders: &Holders, tags: &[Country], hour: i32, warnings: &mut Vec<String>) -> Result<BTreeSet<u32>, String> {
    let mut slots = BTreeSet::new();
    for tag in tags {
        match holders.at(tag, hour).map(|run| run.holder) {
            Some(Holder::Found { slot, .. }) => {
                slots.insert(slot);
            }
            Some(Holder::Unresolved) => {
                let reason = holders.unnamed(tag, hour);
                return Err(if reason == NOT_HELD_AT_HOUR || reason == NOT_IN_HISTORY { format!("Who holds {} is not known: {TAG_UNLINKED}", tag.key()) } else { reason });
            }
            None => {
                let reason = holders.unnamed(tag, hour);
                if reason != NOT_HELD_AT_HOUR && reason != NOT_IN_HISTORY {
                    return Err(reason);
                }
                warnings.push(holders.say(tag, hour, |name| format!("{name} is not held by any country at this hour")));
            }
        }
    }
    Ok(slots)
}

pub struct MarketLendLease;

impl AtHour for MarketLendLease {
    const KEY: &'static str = "market-lendlease";
    const TITLE: &'static str = "Market and lend-lease";
    const PREPARE: Option<dashboard_base::products::at_hour::PrepareHour<Self::Params>> =
        Some(|source, _, load, _| dashboard_base::products::artwork::prepare_artwork(source, load));
    const CONTENT_DEPENDENT: bool = true;
    type Params = MarketLendLeaseParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, countries, only, leaseSide, group, row, "countries[]"];
    type Reply = MarketLendLeaseReply;
    type Context = MarketContext;

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid branch")?;
        if let Some(row) = &params.row {
            row.parse::<u128>().map_err(|_| "Invalid market row")?;
            if !params.countries.is_empty() || params.group.is_some() || !matches!(params.only, Some(MarketSection::Offers | MarketSection::Contracts)) {
                return Err("A market row is read alone, from the offers or the contracts");
            }
        }
        if params.group.is_some() && !params.countries.is_empty() {
            return Err("The market asks a group or countries, never both");
        }
        params.countries.iter().try_for_each(dashboard_base::products::validate_country)
    }

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            MarketLendLeaseParams::decl(),
            MarketLendLeaseReply::decl(),
            MarketSection::decl(),
            LeaseSide::decl(),
            MarketItem::decl(),
            MarketLevel::decl(),
            MarketGrant::decl(),
            MarketOffer::decl(),
            MarketTerms::decl(),
            LeaseTerms::decl(),
        ]
    }

    fn tables(params: &Self::Params) -> Vec<TableId> {
        let [offers, contracts, agreements] = sections(params);
        let mut out = names::TABLES.to_vec();
        out.extend(offers.then_some(TableId::CountryMarket));
        out.extend(contracts.then_some(TableId::MarketContractTerms));
        out.extend(contracts.then_some(TableId::PurchaseContracts));
        out.extend(agreements.then_some(TableId::LendLeaseAgreements));
        out
    }

    fn columns(table: TableId) -> Option<Vec<usize>> {
        names::naming_columns(table)
    }

    fn projection(_params: &Self::Params, table: TableId, _plan: &[(Vec<Read>, i32)], _context: &Self::Context) -> Option<Vec<usize>> {
        match table {
            TableId::LendLeaseAgreements => Some((0..table.field_count()).filter(|&column| !matches!(table.field_name(column), Some("fuel_sent_raw" | "fuel_sunk_raw"))).collect()),
            _ => Self::columns(table),
        }
    }

    fn reads(params: &Self::Params, _owner: Option<Owner>, step: usize, held: &DecodedFrame) -> Vec<Read> {
        let [offers, contracts, agreements] = sections(params);
        match step {
            0 => {
                let mut reads: Vec<Read> = names::WHOLE.iter().map(|&table| Read::all(table)).collect();
                reads.extend(offers.then(|| read_section(params, TableId::CountryMarket)));
                reads.extend(contracts.then(|| read_section(params, TableId::MarketContractTerms)));
                reads.extend(agreements.then(|| Read::all(TableId::LendLeaseAgreements)));
                reads
            }
            1 => {
                let mut variants: BTreeSet<u64> = BTreeSet::new();
                let mut purchases: BTreeSet<u64> = BTreeSet::new();
                if let Ok(rows) = held.complete::<tables::CountryMarket>() {
                    for row in rows {
                        variants.extend(pool_variants(&row.inventory));
                        variants.extend(row.prices.iter().map(|level| level.variant));
                    }
                }
                if let Ok(rows) = held.complete::<tables::MarketContractTerms>() {
                    for row in rows {
                        variants.extend(pool_variants(&row.equipment).chain(pool_variants(&row.prices)));
                        variants.extend(row.price_levels.iter().map(|level| level.variant));
                        if row.kind == 1 {
                            purchases.insert(row.id);
                        }
                    }
                }
                for row in dashboard_query::coverage::witnessed_rows::<tables::LendLeaseAgreements>(held).rows {
                    variants.extend(lease_variants(row));
                }
                let mut reads = vec![
                    Read::these(TableId::VariantDefinitions, variants.into_iter().map(u128::from)),
                    names::capital_state_read(held),
                ];
                if contracts {
                    reads.push(Read::these(TableId::PurchaseContracts, purchases.into_iter().map(u128::from)));
                }
                reads
            }
            2 => {
                let mut tokens: BTreeSet<u32> = names::variant_tokens(held).collect();
                if let Ok(rows) = held.complete::<tables::CountryMarket>() {
                    tokens.extend(rows.iter().flat_map(|row| row.subsidies.iter().map(|grant| grant.archetype)));
                }
                if let Ok(rows) = held.complete::<tables::MarketContractTerms>() {
                    tokens.extend(rows.iter().flat_map(|row| row.subsidies.iter().map(|grant| grant.archetype)));
                }
                names::spelling_reads(held, tokens, true)
            }
            _ => Vec::new(),
        }
    }

    fn context(source: &Source, load: &LoadContext) -> io::Result<Self::Context> {
        Ok(MarketContext::new(names::context(source, load)?, GroupsAt::default()))
    }

    fn context_for(source: &Source, load: &LoadContext, params: &Self::Params) -> io::Result<Self::Context> {
        Ok(MarketContext::new(names::context(source, load)?, GroupsAt::load(source, load, &params.branch_id, params.group.as_ref())?))
    }

    fn valid_through(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> i32 {
        let holders = at.holders.next_change(frame.hour).map_or(i32::MAX, |change| change - 1);
        holders.min(at.context.groups.valid_through(params.group.as_ref(), at.hour))
    }

    fn reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        market_reply(frame, params, at, false)
    }

    fn playback_reply(frame: &DecodedFrame, params: &Self::Params, at: &At<'_, Self::Context>) -> io::Result<Self::Reply> {
        market_reply(frame, params, at, matches!(params.only, Some(MarketSection::Offers | MarketSection::Contracts)))
    }
}

fn market_reply(frame: &DecodedFrame, params: &MarketLendLeaseParams, at: &At<'_, MarketContext>, reuse_designs: bool) -> io::Result<MarketLendLeaseReply> {
    let members = params.group.as_ref().map(|group| at.context.groups.members(group, frame.hour)).transpose()?;
    let reply = match members {
        Some(members) if members.refused.is_some() => MarketLendLeaseReply { refused: members.refused, ..empty_reply(params) },
        members => {
            let asked: Option<Vec<Country>> = members.map(|members| members.tags.into_iter().map(Country::from).collect());
            market_at(frame, params, asked.as_deref(), at.holders, at.context, at.live, reuse_designs)?
        }
    };
    if let Err(error) = check_reply_budget(&reply) {
        if reuse_designs {
            *at.context.designs.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        return Err(error);
    }
    Ok(reply)
}

fn empty_reply(params: &MarketLendLeaseParams) -> MarketLendLeaseReply {
    MarketLendLeaseReply {
        branch_id: params.branch_id.clone(),
        offers: None,
        contracts: None,
        agreements: None,
        agreements_missing: None,
        purchases_missing: None,
        unattributed: 0,
        refused: None,
        variants: Arc::default(),
        icons: Arc::default(),
        warnings: Vec::new(),
    }
}

fn market_at(frame: &DecodedFrame, params: &MarketLendLeaseParams, members: Option<&[Country]>, holders: &Holders, context: &MarketContext, live: &Cells, reuse_designs: bool) -> io::Result<MarketLendLeaseReply> {
    let [show_offers, show_contracts, show_agreements] = sections(params);
    let hour = frame.hour;
    let mut reply = empty_reply(params);
    let links = frame
        .complete::<tables::CountryLinks>()
        .map_err(|error| reply.warnings.push(format!("Countries are not recorded at this hour, so no market row can be named or matched to a chosen country: {error}")))
        .ok();
    let parties = Parties::new(links, holders, hour);
    let wanted = match members {
        Some(members) => slots(holders, members, hour, &mut reply.warnings).map(Some),
        None => chosen(holders, &params.countries, hour, &mut reply.warnings),
    };
    let wanted = match wanted {
        Ok(wanted) => wanted,
        Err(reason) => return Ok(MarketLendLeaseReply { refused: Some(reason), warnings: reply.warnings, ..empty_reply(params) }),
    };
    let mut unattributed = 0_u32;
    let mut keep = |sides: &[u32]| -> bool {
        let Some(slots) = wanted.as_ref() else { return true };
        if slots.is_empty() {
            return false;
        }
        let held: Vec<Option<u32>> = sides.iter().map(|&raw| parties.slot(raw).filter(|_| parties.tag(raw).is_some())).collect();
        if held.iter().flatten().any(|slot| slots.contains(slot)) {
            return true;
        }
        if held.iter().any(Option::is_none) {
            unattributed += 1;
        }
        false
    };
    let offers = section(show_offers, frame.complete::<tables::CountryMarket>(), "Market offers", &mut reply.warnings);
    let terms = section(show_contracts, frame.complete::<tables::MarketContractTerms>(), "Market requests and contracts", &mut reply.warnings);
    let none_can_match = wanted.as_ref().is_some_and(BTreeSet::is_empty);
    let leases = show_agreements.then(|| dashboard_query::coverage::witnessed_rows::<tables::LendLeaseAgreements>(frame)).and_then(|witnessed| match witnessed.missing {
        Some(_) if none_can_match => Some(witnessed.rows),
        Some(reason) if witnessed.rows.is_empty() => {
            reply.warnings.push(format!("Lend-lease agreements are not recorded at this hour: {reason}"));
            None
        }
        Some(reason) => {
            let missing = format!("Lend-lease agreements are listed only where they were read at this hour: {reason}");
            reply.warnings.push(missing.clone());
            reply.agreements_missing = Some(missing);
            Some(witnessed.rows)
        }
        None => Some(witnessed.rows),
    });
    let clicked = row_key(params);
    let offers: Option<Vec<_>> = offers.map(|rows| rows.iter().filter(|row| clicked.is_none_or(|key| key == u128::from(row.country)) && keep(&[row.country])).collect());
    let terms: Option<Vec<_>> = terms.map(|rows| rows.iter().filter(|row| clicked.is_none_or(|key| key == terms_key(row)) && terms_kind(params, row) && keep(&[row.country, row.buyer])).collect());
    let leases: Option<Vec<_>> = leases.map(|rows| {
        rows.iter()
            .filter(|row| match params.lease_side {
                Some(LeaseSide::Sent) => keep(&[row.country]),
                Some(LeaseSide::Received) => keep(&[row.receiver]),
                None => keep(&[row.country, row.receiver]),
            })
            .collect()
    });
    reply.unattributed = unattributed;

    let mut variants: BTreeSet<u64> = BTreeSet::new();
    for row in offers.iter().flatten() {
        variants.extend(pool_variants(&row.inventory));
        variants.extend(row.prices.iter().map(|level| level.variant));
    }
    for row in terms.iter().flatten() {
        variants.extend(pool_variants(&row.equipment).chain(pool_variants(&row.prices)));
        variants.extend(row.price_levels.iter().map(|level| level.variant));
    }
    for row in leases.iter().flatten() {
        variants.extend(lease_variants(row));
    }
    variants.remove(&0);
    let ids: Vec<_> = variants.into_iter().collect();
    let named = context.designs(frame, &ids, reuse_designs);
    let lexical = frame.complete::<tables::LexicalTokens>().ok();
    let texts: Option<&dyn Texts> = context.names.as_ref().ok().map(|content| content.catalogue.as_ref() as &dyn Texts);

    reply.offers = offers.map(|rows| {
        rows.into_iter()
            .map(|row| MarketOffer {
                country: row.country,
                tag: parties.tag(row.country),
                inventory: pool(&named, &row.inventory),
                prices: levels(&named, row.prices.iter()),
                subsidies: grants(row.subsidies.iter(), &parties, lexical, texts),
                subsidy_total_raw: row.subsidy_total_raw.to_string(),
                auto_send_market_access: row.auto_send_market_access,
            })
            .collect()
    });
    if let Some(rows) = terms {
        let purchases: HashMap<u64, (u32, u32)> = match frame.complete::<tables::PurchaseContracts>() {
            Ok(rows) => rows.iter().map(|purchase| (purchase.id, (purchase.seller, purchase.buyer))).collect(),
            Err(error) => {
                reply.purchases_missing = Some(format!("Purchase progress is not recorded at this hour: {error}"));
                HashMap::new()
            }
        };
        let column = |name| dashboard_declare::field(TableId::PurchaseContracts, name);
        let progress = [("assigned", column("convoys_assigned")?), ("convoys", column("convoys_total")?), ("collected", column("collected_raw")?), ("deliveries", column("completed_deliveries")?)];
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let key = terms_key(row).to_string();
            let purchase = row.kind == 1 && purchases.get(&row.id) == Some(&(row.country, row.buyer));
            if purchase {
                for (name, column) in progress {
                    live.value(&key, name, Cell { table: TableId::PurchaseContracts, column, key: u128::from(row.id) }, 1.0);
                }
            }
            let (seller, buyer) = (parties.tag(row.country), parties.tag(row.buyer));
            out.push(MarketTerms {
                key,
                id: format!("{:016x}", row.id),
                seller,
                buyer,
                request: row.kind == 0,
                speed: row.speed,
                equipment: pool(&named, &row.equipment),
                prices: pool(&named, &row.prices),
                levels: levels(&named, row.price_levels.iter()),
                subsidies: grants(row.subsidies.iter(), &parties, lexical, texts),
                purchase,
            });
        }
        reply.contracts = Some(out);
    }
    if let Some(rows) = leases {
        let sent = dashboard_declare::field(TableId::LendLeaseAgreements, "fuel_sent_raw")?;
        let lost = dashboard_declare::field(TableId::LendLeaseAgreements, "fuel_sunk_raw")?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let terms = lease_terms(row, |raw| parties.named(raw), &named);
            let key = lease_key(row);
            live.value(&terms.key, "fuelSent", Cell { table: TableId::LendLeaseAgreements, column: sent, key }, 1.0);
            live.value(&terms.key, "fuelLost", Cell { table: TableId::LendLeaseAgreements, column: lost, key }, 1.0);
            out.push(terms);
        }
        reply.agreements = Some(out);
    }
    if reply.unattributed > 0 {
        reply.warnings.push(format!("{} market or lend-lease rows name a country with no tag at this hour; a chosen country may take part in them, so they are not listed", reply.unattributed));
    }
    reply.warnings.extend(named.warnings.iter().cloned());
    reply.icons = Arc::clone(&named.icons);
    reply.variants = Arc::clone(&named.variants);
    Ok(reply)
}
