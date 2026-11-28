
use dashboard_base::products::Country;
use dashboard_base::products::{
    LoadContext, Product,
    at_hour::{DateSelection, ReadDates},
    check_reply_budget,
    idea_format::{self, LocalisedText, ModifierFormat},
    politics_content::{PoliticsContent, politics_content},
    read_ahead::Prepare,
    scoped_text::{self, CompanionTables, KeyedVariables, Lexical, Scope, ScopedText, Validity},
    validate_country,
};
use dashboard_core::dto::Artwork;
use dashboard_base::{Source, catalogue};
use dashboard_text::metadata::check_cancel;
use dashboard_base::span::HourSpan;
use dashboard_query::{Query, cost, identity::Holder};
use dashboard_text::{
    catalogue::View,
    identity::{NamingCountry, built_template},
    idea_names::IdeaCountry,
};
use content_reader_key_names::characters::BuiltTemplate;
use content_reader_parser::coverage::Gap;
use content_reader_script::modifiers::{Modifier, Modifiers};
use replay_lanes::lanes::{self, Cell, History, Keys};
use replay_data::rows::{self, CharacterRow, LexicalTokenRow, ModifierDefinitionRow, TableId, mask, tables};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{Arc, Mutex, PoisonError, Weak, atomic::AtomicBool},
};
#[cfg(any(test, feature = "export-types"))]
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IdeaSpan {
    pub token: String,
    pub first_hour: i32,
    pub last_hour: i32,
    pub slot: Option<String>,
    pub character: Option<IdeaCharacter>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdeaHistoryParams {
    pub branch_id: String,
    pub country: Country,
    #[doc = " The hour answered: nothing recorded after it is read into the reply."]
    pub hour: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum HoldingStart {
    #[doc = " Witnessed not held the hour before, so it began at `firstHour`."]
    Seen,
    #[doc = " Held when the country's identity was first read with no unread hour before it (the"]
    #[doc = " branch's first hour, or another holder taking over): it began before `firstHour`."]
    HeldBefore,
    #[doc = " First seen after hours the country's ideas were not read: it began by `firstHour`, not at it."]
    FirstObserved,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum HoldingEnd {
    #[doc = " Held at the reply's hour; `lastHour` is that hour."]
    Held,
    #[doc = " Witnessed not held at `lastHour + 1`."]
    Ended,
    #[doc = " Not read after `lastHour` until `hour`, which witnessed it no longer held."]
    EndedBy { hour: i32 },
    #[doc = " Not read from `lastHour + 1` through the reply's hour: whether it is still held is unknown."]
    LastSeen,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IdeaHolding {
    pub token: String,
    pub first_hour: i32,
    #[doc = " The last hour it is known held, never after the reply's hour. Hours not read inside it are"]
    #[doc = " in `unobserved`."]
    pub last_hour: i32,
    pub start: HoldingStart,
    pub end: HoldingEnd,
    pub slot: Option<String>,
    pub character: Option<IdeaCharacter>,
    #[doc = " Its presentation in `looks`, as named and classified when it was first observed."]
    pub look: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IdeaLook {
    pub info: IdeaInfo,
    #[doc = " The idea's name and icon; for an advisor, its holder's."]
    pub artwork: Artwork,
    #[doc = " Why this look's artwork is missing; omitted when artwork is present."]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(any(test, feature = "export-types"), ts(optional))]
    pub artwork_missing: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IdeaCharacter {
    #[doc = " The recorded character id."]
    pub id: u32,
    #[doc = " The script token (resolved through LexicalTokens); empty when unresolved."]
    pub token: String,
    #[doc = " The saved name as recorded (a localisation key or a literal); a rename changes it, so it"]
    #[doc = " comes before the authored name."]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EffectLine {
    #[doc = " The modifier's case-folded key; formats are keyed the same way."]
    pub key: String,
    #[doc = " The stored fixed5 value as a decimal string."]
    pub fixed5: String,
    #[doc = " Named in hidden_modifier: applied, never listed."]
    pub hidden: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EffectGroup {
    pub target: Option<String>,
    #[doc = " An equipment bonus that applies at once to existing equipment."]
    pub instant: bool,
    pub lines: Vec<EffectLine>,
    #[doc = " A targeted modifier's custom tooltip keys in stored order; none for an equipment bonus."]
    pub tooltips: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct TraitEffects {
    pub token: String,
    #[doc = " Content defines the trait. When it does not, its effects are unknown and the lists below"]
    #[doc = " are empty for that reason, not because it has none."]
    pub known: bool,
    #[doc = " Why content cannot say whether it defines the trait; None when it can."]
    pub unknown: Option<String>,
    pub name: Option<String>,
    pub lines: Vec<EffectLine>,
    pub targeted: Vec<EffectGroup>,
    pub equipment: Vec<EffectGroup>,
    pub tooltips: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IdeaInfo {
    pub token: String,
    #[doc = " The idea category; None when neither content nor the recording places it."]
    pub category: Option<String>,
    #[doc = " The slot or character seat it fills; None for category-wide ideas such as national spirits."]
    pub slot: Option<String>,
    pub law: bool,
    pub designer: bool,
    pub effects: Vec<EffectLine>,
    pub targeted: Vec<EffectGroup>,
    pub equipment: Vec<EffectGroup>,
    pub research: Vec<EffectLine>,
    pub traits: Vec<TraitEffects>,
    #[doc = " Custom tooltip localisation keys the idea shows instead of values."]
    pub tooltips: Vec<String>,
    #[doc = " Content defines the idea or its advisor role, so the lists above are its effects."]
    pub effects_known: bool,
    #[doc = " Why content cannot say what the idea is; None when it can."]
    pub unknown: Option<String>,
    #[doc = " Why the idea's artwork has no name (a localisation key missing or refused, or what the"]
    #[doc = " hour cannot say about the naming country); None when it is named."]
    pub name_missing: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SlotInfo {
    pub token: String,
    pub name: Option<String>,
    #[doc = " A character seat (advisors) rather than an idea slot (laws, designers)."]
    pub character: bool,
    #[doc = " How many seats of this slot the category has."]
    pub seats: u32,
    #[doc = " For a law slot: its laws in content order, so a law's escalation is its place here."]
    pub laws: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CategoryInfo {
    pub token: String,
    pub name: Option<String>,
    #[doc = " `type`, such as `national_spirit`."]
    pub kind: Option<String>,
    pub hidden: bool,
    pub politics_tab: bool,
    pub slots: Vec<SlotInfo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(any(test, feature = "export-types"), derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IdeaHistoryReply {
    pub branch_id: String,
    pub tag: String,
    #[doc = " The country the request named, which a reply is matched to: a slot follows its country through any tag."]
    pub asked: dashboard_base::products::Country,
    #[doc = " The hour answered, never after the branch's last hour."]
    pub hour: i32,
    #[doc = " The first hour after `hour` at which anything in this reply changes, other than held"]
    #[doc = " holdings and an unread stretch running on; null when nothing does."]
    pub next_change: Option<i32>,
    #[doc = " Why the country's ideas are not known at `hour`; null when they were read then."]
    pub unrecorded: Option<String>,
    #[doc = " Every holding begun by `hour`, by first hour. A holding continues across hours not read."]
    pub holdings: Vec<IdeaHolding>,
    #[doc = " The holdings held at `hour`, in display order: the earliest begun first, then by name."]
    pub held: Vec<u32>,
    pub looks: Vec<IdeaLook>,
    #[doc = " Every category in content order, so widgets list mod categories too."]
    pub categories: Vec<CategoryInfo>,
    #[doc = " Hours through `hour` the country's ideas were not read."]
    pub unobserved: Vec<HourSpan>,
    #[doc = " How the game shows each modifier the effects above use; a key missing here is shown as"]
    #[doc = " authored, unformatted."]
    pub modifiers: Vec<ModifierFormat>,
    #[doc = " The texts the tooltips need: custom tooltips, research categories, equipment names, and"]
    #[doc = " `YES` / `NO`."]
    pub texts: Vec<LocalisedText>,
    pub warnings: Vec<String>,
}

dashboard_base::reply_warnings!(IdeaHistoryReply);

pub(crate) const TOKENLESS_SEATS: &str = "Some appointed advisors have no recorded idea, so their seats show as empty.";
pub(crate) const SEAT: &str = "hired advisors seat is not UTF-8; appointment cannot be identified.";
pub(crate) const TOKEN: &str = "Held idea token is not UTF-8; holding cannot be identified.";

fn missing_template(id: u32, hour: i32) -> String {
    format!("character {id} has no Characters row at hour {hour}")
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Ideas {
    pub spans: Vec<IdeaSpan>,
    pub unobserved: Vec<HourSpan>,
    pub identity_starts: Vec<i32>,
    pub starts: Vec<(u32, u32, u32)>,
    pub templates: BTreeMap<usize, Result<BuiltTemplate, String>>,
    pub warnings: BTreeMap<&'static str, i32>,
    pub boundaries: Vec<i32>,
    last: Option<(i32, (u32, u32))>,
    open: BTreeMap<String, usize>,
}

struct Columns {
    idea: usize,
    advisor: [usize; 3],
    character: [usize; 7],
    literal: usize,
}

impl Columns {
    fn new() -> io::Result<Self> {
        let field = dashboard_declare::field;
        let characters = ["name_token_id", "token", "template_present", "template_tag", "template_file", "template_line", "template_generated"];
        let mut character = [0; 7];
        for (at, name) in characters.into_iter().enumerate() {
            character[at] = field(TableId::Characters, name)?;
        }
        Ok(Self {
            idea: field(TableId::ActiveIdeas, "token")?,
            advisor: [field(TableId::HiredAdvisors, "character")?, field(TableId::HiredAdvisors, "idea")?, field(TableId::HiredAdvisors, "slot")?],
            character,
            literal: field(TableId::LexicalTokens, "literal")?,
        })
    }
}

struct Stepper<'a> {
    history: &'a History,
    at: Vec<usize>,
    now: Vec<Option<&'a [Cell]>>,
}

impl<'a> Stepper<'a> {
    fn new(history: &'a History) -> Self {
        Self { history, at: vec![0; history.keys.len()], now: vec![None; history.keys.len()] }
    }

    fn hours(&self) -> impl Iterator<Item = i32> + '_ {
        self.history.keys.iter().flat_map(|(_, steps)| steps.iter().map(|step| step.hour))
    }

    fn advance(&mut self, hour: i32) {
        for (index, (_, steps)) in self.history.keys.iter().enumerate() {
            while let Some(step) = steps.get(self.at[index]).filter(|step| step.hour <= hour) {
                self.now[index] = step.cells.as_deref();
                self.at[index] += 1;
            }
        }
    }

    fn shown(&self) -> impl Iterator<Item = (u128, &'a [Cell])> + '_ {
        self.history.keys.iter().zip(&self.now).filter_map(|((key, _), cells)| Some((*key, (*cells)?)))
    }

    fn cells(&self, key: u128) -> Option<&'a [Cell]> {
        let at = self.history.keys.binary_search_by_key(&key, |(held, _)| *held).ok()?;
        self.now[at]
    }

    fn place(&self, column: usize) -> Option<usize> {
        self.history.columns.binary_search(&column).ok()
    }

    fn text(&self, cell: Cell) -> Option<&'a [u8]> {
        self.history.text(cell)
    }
}

fn number(cell: Cell) -> Option<i64> {
    match cell {
        Cell::Number(value) => Some(value),
        _ => None,
    }
}

fn both(a: &[(i32, i32)], b: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (first, last) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if first <= last {
            out.push((first, last));
        }
        if a[i].1 < b[j].1 { i += 1 } else { j += 1 }
    }
    out
}

fn mark(spans: &mut Vec<HourSpan>, first_hour: i32, last_hour: i32) {
    match spans.last_mut() {
        Some(span) if i64::from(span.last_hour) + 1 >= i64::from(first_hour) => {
            span.first_hour = span.first_hour.min(first_hour);
            span.last_hour = span.last_hour.max(last_hour);
        }
        _ => spans.push(HourSpan { first_hour, last_hour }),
    }
}

pub(crate) fn read(query: &Query<'_>, tag: &Country, cancelled: &AtomicBool) -> io::Result<Ideas> {
    let columns = Columns::new()?;
    let mut ideas = Ideas::default();
    let runs = query.holders.runs(tag);
    let mut holes: Vec<(i32, i32)> = query.timeline.unheld.iter().map(|unheld| (unheld.first, unheld.last)).collect();
    for stretch in &query.timeline.stretches {
        check_cancel(cancelled)?;
        ideas.close();
        let mut covered = stretch.first;
        for run in runs.iter().filter(|run| run.last >= stretch.first && run.first <= stretch.last) {
            let (first, last) = (run.first.max(stretch.first), run.last.min(stretch.last));
            if covered < first {
                holes.push((covered, first - 1));
                ideas.close();
            }
            covered = last.saturating_add(1);
            match run.holder {
                Holder::Found { slot, id } => ideas.country(query, &columns, stretch.run, (slot, id), first, last, cancelled)?,
                Holder::Unresolved => {
                    holes.push((first, last));
                    ideas.close();
                }
            }
        }
        if covered <= stretch.last {
            holes.push((covered, stretch.last));
        }
    }
    holes.extend(ideas.unobserved.drain(..).map(|span| (span.first_hour, span.last_hour)));
    holes.sort_unstable();
    for (first, last) in holes {
        mark(&mut ideas.unobserved, first, last);
    }
    let mut boundaries: Vec<i32> = ideas.identity_starts.clone();
    for (first, last) in ideas.spans.iter().map(|span| (span.first_hour, span.last_hour)).chain(ideas.unobserved.iter().map(|gap| (gap.first_hour, gap.last_hour))) {
        boundaries.push(first);
        boundaries.extend(last.checked_add(1));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    ideas.boundaries = boundaries;
    Ok(ideas)
}

impl Ideas {
    fn close(&mut self) {
        self.last = None;
        self.open.clear();
    }

    fn country(&mut self, query: &Query<'_>, columns: &Columns, run: u32, who: (u32, u32), first: i32, last: i32, cancelled: &AtomicBool) -> io::Result<()> {
        let recording = query.replay.recording();
        let key = u128::from(who.1);
        let range = Keys::Range(key << 32, key << 32 | 0xFFFF_FFFF);
        let observed = cost::storage(|| -> io::Result<_> {
            let ideas = super::party_history::complete_reads(recording, run, TableId::ActiveIdeas, first, last)?;
            let advisors = super::party_history::complete_reads(recording, run, TableId::HiredAdvisors, first, last)?;
            Ok(both(&ideas, &advisors))
        })?;
        let held = cost::storage(|| lanes::history(recording, run, TableId::ActiveIdeas, &[columns.idea], range, first, last))?;
        let hired = cost::storage(|| lanes::history(recording, run, TableId::HiredAdvisors, &columns.advisor, range, first, last))?;
        check_cancel(cancelled)?;
        let at_character = hired.columns.binary_search(&columns.advisor[0]).map_err(|_| dashboard_core::invalid("An advisor history omitted a column"))?;
        let mut people: Vec<u128> = hired
            .keys
            .iter()
            .flat_map(|(_, steps)| steps.iter().filter_map(|step| step.cells.as_ref().and_then(|cells| number(cells[at_character]))))
            .filter_map(|id| u32::try_from(id).ok().map(u128::from))
            .collect();
        people.sort_unstable();
        people.dedup();
        let characters = if people.is_empty() {
            History::default()
        } else {
            cost::storage(|| lanes::history(recording, run, TableId::Characters, &columns.character, Keys::These(&people), first, last))?
        };
        let mut words: Vec<u128> = Vec::new();
        for column in [columns.character[0], columns.character[3]] {
            if let Ok(at) = characters.columns.binary_search(&column) {
                words.extend(
                    characters
                        .keys
                        .iter()
                        .flat_map(|(_, steps)| steps.iter().filter_map(|step| step.cells.as_ref().and_then(|cells| number(cells[at]))))
                        .filter_map(|word| u32::try_from(word).ok().filter(|word| *word != 0).map(u128::from)),
                );
            }
        }
        words.sort_unstable();
        words.dedup();
        let lexicon = if words.is_empty() {
            History::default()
        } else {
            cost::storage(|| lanes::history(recording, run, TableId::LexicalTokens, &[columns.literal], Keys::These(&words), first, last))?
        };
        check_cancel(cancelled)?;
        let mut held = Stepper::new(&held);
        let mut hired = Stepper::new(&hired);
        let mut characters = Stepper::new(&characters);
        let mut lexicon = Stepper::new(&lexicon);
        let mut hours: Vec<i32> = held.hours().chain(hired.hours()).chain(characters.hours()).chain(lexicon.hours()).chain([first]).collect();
        for &(start, end) in &observed {
            hours.push(start);
            if end < last {
                hours.push(end + 1);
            }
        }
        hours.retain(|hour| (first..=last).contains(hour));
        hours.sort_unstable();
        hours.dedup();
        let mut unread = 0;
        for (index, &hour) in hours.iter().enumerate() {
            let end = hours.get(index + 1).map_or(last, |next| next - 1);
            held.advance(hour);
            hired.advance(hour);
            characters.advance(hour);
            lexicon.advance(hour);
            while observed.get(unread).is_some_and(|&(_, until)| until < hour) {
                unread += 1;
            }
            if !observed.get(unread).is_some_and(|&(from, _)| from <= hour) {
                mark(&mut self.unobserved, hour, end);
                self.close();
                continue;
            }
            self.observe(hour, end, run, who, columns, &held, &hired, &characters, &lexicon);
        }
        Ok(())
    }

    fn observe(&mut self, hour: i32, end: i32, run: u32, who: (u32, u32), columns: &Columns, held: &Stepper<'_>, hired: &Stepper<'_>, characters: &Stepper<'_>, lexicon: &Stepper<'_>) {
        let continues = self.last.is_some_and(|(at, seen)| seen == who && i64::from(at) + 1 == i64::from(hour));
        if !continues {
            self.identity_starts.push(hour);
            self.starts.push((run, who.0, who.1));
            self.open.clear();
        }
        self.last = Some((end, who));
        let literal = |word: i64| -> Option<&[u8]> {
            let word = u32::try_from(word).ok().filter(|word| *word != 0)?;
            let place = lexicon.place(columns.literal)?;
            lexicon.cells(u128::from(word)).and_then(|cells| lexicon.text(cells[place]))
        };
        let place = |stepper: &Stepper<'_>, column: usize| stepper.place(column);
        let mut roles: BTreeMap<Vec<u8>, (String, IdeaCharacter)> = BTreeMap::new();
        if let (Some(at_character), Some(at_idea), Some(at_seat)) = (place(hired, columns.advisor[0]), place(hired, columns.advisor[1]), place(hired, columns.advisor[2])) {
            for (_, cells) in hired.shown() {
                let seat = hired.text(cells[at_seat]).unwrap_or_default();
                let Ok(seat) = std::str::from_utf8(seat) else {
                    self.warnings.entry(SEAT).or_insert(hour);
                    continue;
                };
                let idea = hired.text(cells[at_idea]).unwrap_or_default();
                if idea.is_empty() {
                    self.warnings.entry(TOKENLESS_SEATS).or_insert(hour);
                    continue;
                }
                let Some(id) = number(cells[at_character]).and_then(|id| u32::try_from(id).ok()) else { continue };
                let row = character_row(characters, columns, id);
                let token = row.as_ref().and_then(|row| literal(i64::from(row.name_token_id))).and_then(|token| std::str::from_utf8(token).ok()).unwrap_or_default();
                let name = row.as_ref().and_then(|row| std::str::from_utf8(&row.token).ok()).unwrap_or_default();
                roles.insert(idea.to_vec(), (seat.to_owned(), IdeaCharacter { id, token: token.to_owned(), name: name.to_owned() }));
            }
        }
        let mut tokens: BTreeSet<Vec<u8>> = BTreeSet::new();
        if let Some(at) = held.place(columns.idea) {
            tokens.extend(held.shown().filter_map(|(_, cells)| held.text(cells[at])).filter(|token| !token.is_empty()).map(<[u8]>::to_vec));
        }
        tokens.extend(roles.keys().cloned());
        self.open.retain(|token, _| tokens.contains(token.as_bytes()));
        for bytes in tokens {
            let Ok(token) = std::str::from_utf8(&bytes) else {
                self.warnings.entry(TOKEN).or_insert(hour);
                continue;
            };
            let role = roles.get(&bytes);
            match self.open.get(token) {
                Some(&index) if self.spans[index].slot.as_deref() == role.map(|role| role.0.as_str()) && self.spans[index].character.as_ref() == role.map(|role| &role.1) => {
                    self.spans[index].last_hour = end;
                }
                _ => {
                    if let Some((_, holder)) = role {
                        let template = match character_row(characters, columns, holder.id) {
                            Some(row) => {
                                let spelled: Vec<LexicalTokenRow> = literal(i64::from(row.template_tag)).map(|literal| LexicalTokenRow { token: row.template_tag, literal: literal.to_vec() }).into_iter().collect();
                                built_template(&row, Some(&spelled))
                            }
                            None => Err(missing_template(holder.id, hour)),
                        };
                        self.templates.insert(self.spans.len(), template);
                    }
                    self.open.insert(token.to_owned(), self.spans.len());
                    self.spans.push(IdeaSpan {
                        token: token.to_owned(),
                        first_hour: hour,
                        last_hour: end,
                        slot: role.map(|role| role.0.clone()),
                        character: role.map(|role| role.1.clone()),
                    });
                }
            }
        }
    }
}

fn character_row(characters: &Stepper<'_>, columns: &Columns, id: u32) -> Option<CharacterRow> {
    let cells = characters.cells(u128::from(id))?;
    let at = |column: usize| characters.place(column).map(|place| cells[place]);
    let number = |column: usize| at(column).and_then(number);
    let text = |column: usize| at(column).and_then(|cell| characters.text(cell)).map(<[u8]>::to_vec).unwrap_or_default();
    let [name_token, token, present, tag, file, line, generated] = columns.character;
    Some(CharacterRow {
        id,
        name_token_id: number(name_token).and_then(|value| i32::try_from(value).ok()).unwrap_or(-1),
        token: text(token),
        template_present: number(present).and_then(|value| u8::try_from(value).ok()).unwrap_or_default(),
        template_tag: number(tag).and_then(|value| u32::try_from(value).ok()).unwrap_or_default(),
        template_file: text(file),
        template_line: number(line).and_then(|value| i32::try_from(value).ok()).unwrap_or_default(),
        template_generated: number(generated).and_then(|value| u8::try_from(value).ok()).unwrap_or_default(),
        ..Default::default()
    })
}

pub(crate) fn naming_at(frames: &mut dashboard_base::products::at_hour::Frames<'_>, hour: i32, id: u32) -> io::Result<NamingCountry> {
    use dashboard_base::products::at_hour::Read;
    let reads = [
        Read::these(TableId::Countries, [u128::from(id)]),
        Read::these(TableId::CountryAppearance, [u128::from(id)]),
        Read::all(TableId::CountryLinks),
        Read::all(TableId::CountryCivilWars),
        Read::all(TableId::CountryWars),
        Read::all(TableId::WarRelations),
        Read::all(TableId::CountryCounterInputs),
    ];
    let frame = frames.frame(hour, &reads, "An identity's naming")?;
    let Some(country) = frame.complete::<tables::Countries>().ok().and_then(|rows| rows.iter().find(|row| row.id == id)) else {
        return Ok(NamingCountry::default());
    };
    let appearance = frame.complete::<tables::CountryAppearance>().ok().and_then(|rows| rows.iter().find(|row| row.country == id));
    Ok(NamingCountry::read_with_appearance(&frame, country, appearance))
}

pub(crate) type RecordedDefinitions = Option<BTreeMap<String, ModifierDefinitionRow>>;

#[derive(Default)]
struct KeptDefinitions(Vec<((u32, i32), Arc<RecordedDefinitions>)>);

pub(crate) fn recorded_definitions(source: &Source, query: &Query<'_>) -> io::Result<Arc<RecordedDefinitions>> {
    let Some(stretch) = query.timeline.stretches.last() else { return Ok(Arc::new(None)) };
    let at = (stretch.run, stretch.last);
    let state = source.state::<KeptDefinitions>();
    let mut kept = state.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, held)) = kept.0.iter().find(|(held, _)| *held == at) {
        return Ok(Arc::clone(held));
    }
    let read = Arc::new(definitions_at(query, stretch.run, stretch.last)?);
    kept.0.push((at, Arc::clone(&read)));
    Ok(read)
}

fn definitions_at(query: &Query<'_>, run: u32, hour: i32) -> io::Result<RecordedDefinitions> {
    let recording = query.replay.recording();
    let definitions = cost::storage(|| replay_lanes::at::frame(recording, run, hour, &[replay_lanes::at::Read::all(TableId::ModifierDefinitions)]))?;
    let Ok(rows) = definitions.complete::<tables::ModifierDefinitions>() else { return Ok(None) };
    let mut words: Vec<u128> = rows.iter().map(|row| u128::from(row.token)).collect();
    words.sort_unstable();
    words.dedup();
    let reads = [replay_lanes::at::Read { table: TableId::LexicalTokens, columns: None, keys: replay_lanes::at::Keys::These(&words) }];
    let lexicon = cost::storage(|| replay_lanes::at::frame(recording, run, hour, &reads))?;
    let Ok(lexical) = lexicon.complete::<tables::LexicalTokens>() else { return Ok(None) };
    Ok(Some(idea_format::index_recorded(rows, lexical)))
}

fn line(modifier: &Modifier, hidden: bool) -> EffectLine {
    EffectLine { key: modifier.name.clone(), fixed5: modifier.value.to_string(), hidden }
}

fn known<'a, T>(found: Result<Option<&'a T>, &Gap>) -> (Option<&'a T>, Option<String>) {
    match found {
        Ok(found) => (found, None),
        Err(gap) => (None, Some(gap.to_string())),
    }
}

fn targeted(groups: &[content_reader_script::modifiers::TargetedModifier]) -> Vec<EffectGroup> {
    groups.iter().map(|group| EffectGroup { target: group.tag.clone(), instant: false, lines: lines(&group.modifiers), tooltips: group.modifiers.tooltips() }).collect()
}

fn equipment(bonuses: &[content_reader_script::modifiers::EquipmentBonus]) -> Vec<EffectGroup> {
    bonuses
        .iter()
        .map(|bonus| EffectGroup { target: Some(bonus.equipment.clone()), instant: bonus.instant, lines: bonus.values.iter().map(|m| line(m, false)).collect(), tooltips: Vec::new() })
        .collect()
}

fn lines(modifiers: &Modifiers) -> Vec<EffectLine> {
    modifiers.values.iter().map(|m| line(m, modifiers.hidden.contains(&m.name))).collect()
}

pub(crate) fn idea_info(content: &PoliticsContent, token: &str, recorded_seat: Option<&str>, built: Result<&BuiltTemplate, &str>) -> IdeaInfo {
    let tags = &content.ideas.tags;
    let mut unknown = Vec::new();
    let (idea, gap) = known(content.ideas.idea(token));
    unknown.extend(gap);
    let authored_role = content.characters.held_advisor(token, built).inspect_err(|reason| unknown.push(reason.clone())).ok().flatten();
    let (group_category, gap) = match idea {
        Some(idea) => known(tags.resolve(&idea.group)),
        None => (None, None),
    };
    unknown.extend(gap.clone());
    let named = group_category.zip(idea).is_some_and(|(category, idea)| category.name == idea.group);
    let slot = recorded_seat
        .map(str::to_owned)
        .or_else(|| authored_role.map(|role| role.slot.clone()))
        .or_else(|| idea.filter(|_| !named && gap.is_none()).map(|idea| idea.group.clone()));
    let category = match (group_category, &slot) {
        (Some(category), None) => Some(category.name.clone()),
        (_, Some(slot)) => {
            let (category, gap) = known(tags.resolve(slot));
            unknown.extend(gap);
            category.map(|category| category.name.clone())
        }
        (None, None) => None,
    };
    let trait_tokens = idea.map(|idea| idea.traits.clone()).or_else(|| authored_role.map(|role| role.traits.clone())).unwrap_or_default();
    let traits = trait_tokens
        .iter()
        .map(|trait_token| {
            let (found, unknown) = known(content.traits.get(trait_token));
            TraitEffects {
                token: trait_token.clone(),
                known: found.is_some(),
                unknown,
                name: found.and_then(|found| content.localised(&found.token, View::Plain)),
                lines: found.map(|found| lines(&found.modifiers)).unwrap_or_default(),
                targeted: found.map(|found| targeted(&found.targeted)).unwrap_or_default(),
                equipment: found.map(|found| equipment(&found.equipment_bonuses)).unwrap_or_default(),
                tooltips: found.map(|found| found.modifiers.tooltips()).unwrap_or_default(),
            }
        })
        .collect();
    let modifiers = idea.map(|idea| &idea.modifiers).or_else(|| authored_role.map(|role| &role.modifiers));
    let research = idea.map(|idea| &idea.research_bonuses).or_else(|| authored_role.map(|role| &role.research_bonuses));
    let effects_known = idea.is_some() || authored_role.is_some();
    IdeaInfo {
        token: token.to_owned(),
        category,
        slot,
        law: idea.is_some_and(|idea| idea.law),
        designer: idea.is_some_and(|idea| idea.designer),
        effects: modifiers.map(lines).unwrap_or_default(),
        targeted: idea.map(|idea| targeted(&idea.targeted)).unwrap_or_default(),
        equipment: idea.map(|idea| equipment(&idea.equipment_bonuses)).unwrap_or_default(),
        research: research.map(|bonuses| bonuses.iter().map(|m| line(m, false)).collect()).unwrap_or_default(),
        traits,
        tooltips: modifiers.map(Modifiers::tooltips).unwrap_or_default(),
        effects_known,
        unknown: (!effects_known || !unknown.is_empty()).then(|| unknown.join("; ")).filter(|text| !text.is_empty()),
        name_missing: None,
    }
}

pub(crate) fn categories(content: &PoliticsContent) -> Vec<CategoryInfo> {
    let (categories, _) = content.ideas.tags.categories();
    categories
        .iter()
        .map(|category| {
            let mut slots: Vec<SlotInfo> = category
                .slots
                .iter()
                .map(|slot| SlotInfo {
                    token: slot.clone(),
                    name: content.localised(slot, View::Plain),
                    character: false,
                    seats: 1,
                    laws: content.ideas.laws(slot).0.into_iter().map(|idea| idea.token.clone()).collect(),
                })
                .collect();
            for seat in &category.character_slots {
                match slots.iter_mut().find(|slot| slot.character && slot.token == *seat) {
                    Some(slot) => slot.seats += 1,
                    None => slots.push(SlotInfo { token: seat.clone(), name: content.localised(seat, View::Plain), character: true, seats: 1, laws: Vec::new() }),
                }
            }
            CategoryInfo {
                token: category.name.clone(),
                name: content.localised(&category.name, View::Plain),
                kind: category.kind.clone(),
                hidden: category.hidden,
                politics_tab: category.politics_tab,
                slots,
            }
        })
        .collect()
}

pub(crate) fn effect_keys<'a>(ideas: impl IntoIterator<Item = &'a IdeaInfo>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut modifier_keys = BTreeSet::new();
    let mut text_keys: BTreeSet<String> = ["YES", "NO"].map(str::to_owned).into();
    for idea in ideas {
        let mut effects = |lines: &[EffectLine], targeted: &[EffectGroup], tooltips: &[String]| {
            modifier_keys.extend(lines.iter().chain(targeted.iter().flat_map(|group| &group.lines)).map(|line| line.key.clone()));
            text_keys.extend(tooltips.iter().cloned());
            text_keys.extend(targeted.iter().flat_map(|group| group.tooltips.iter().cloned()));
        };
        effects(&idea.effects, &idea.targeted, &idea.tooltips);
        for found in &idea.traits {
            effects(&found.lines, &found.targeted, &found.tooltips);
        }
        text_keys.extend(idea.research.iter().map(|line| format!("{}_research", line.key)));
        text_keys.extend(idea.equipment.iter().chain(idea.traits.iter().flat_map(|found| &found.equipment)).filter_map(|group| group.target.clone()));
    }
    (modifier_keys, text_keys)
}

pub(crate) const NOT_READ: &str = "The country's ideas were not read at this hour";
pub(crate) const BEFORE_BRANCH: &str = "The hour is before this branch's first recorded hour";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Holding {
    pub span: usize,
    pub first_hour: i32,
    pub last_hour: i32,
    pub start: HoldingStart,
    pub end: HoldingEnd,
}

impl Holding {
    fn last_at(&self, hour: i32) -> i32 {
        if self.end == HoldingEnd::Held { hour } else { self.last_hour }
    }
}

type HoldingKey<'a> = (&'a str, Option<&'a str>, Option<(u32, &'a str, &'a str)>);

fn holding_key(span: &IdeaSpan) -> HoldingKey<'_> {
    (span.token.as_str(), span.slot.as_deref(), span.character.as_ref().map(|holder| (holder.id, holder.token.as_str(), holder.name.as_str())))
}

impl Ideas {
    fn gap_at(&self, hour: i32) -> Option<&HourSpan> {
        let at = self.unobserved.partition_point(|gap| gap.last_hour < hour);
        self.unobserved.get(at).filter(|gap| gap.first_hour <= hour)
    }

    fn unread(&self, from: i32, to: i32) -> bool {
        from > to || self.gap_at(from).is_some_and(|gap| gap.last_hour >= to)
    }

    fn holder_at(&self, hour: i32) -> Option<(u32, u32)> {
        let at = self.identity_starts.partition_point(|start| *start <= hour).checked_sub(1)?;
        self.starts.get(at).map(|start| (start.1, start.2))
    }

    pub(crate) fn unread_at(&self, hour: i32) -> bool {
        self.gap_at(hour).is_some()
    }

    pub(crate) fn next_change(&self, hour: i32) -> Option<i32> {
        self.boundaries.get(self.boundaries.partition_point(|boundary| *boundary <= hour)).copied()
    }

    pub(crate) fn at(&self, hour: i32) -> Vec<Holding> {
        let begun = self.spans.partition_point(|span| span.first_hour <= hour);
        let mut holdings: Vec<Holding> = Vec::new();
        let mut through: Vec<i32> = Vec::new();
        let mut latest: BTreeMap<HoldingKey<'_>, usize> = BTreeMap::new();
        for (index, span) in self.spans[..begun].iter().enumerate() {
            let key = holding_key(span);
            match latest
                .get(&key)
                .copied()
                .filter(|&at| self.unread(holdings[at].last_hour.saturating_add(1), span.first_hour - 1) && self.holder_at(holdings[at].last_hour) == self.holder_at(span.first_hour))
            {
                Some(at) => {
                    holdings[at].last_hour = span.last_hour.min(hour);
                    through[at] = span.last_hour;
                }
                None => {
                    latest.insert(key, holdings.len());
                    holdings.push(Holding { span: index, first_hour: span.first_hour, last_hour: span.last_hour.min(hour), start: HoldingStart::Seen, end: HoldingEnd::Held });
                    through.push(span.last_hour);
                }
            }
        }
        for (holding, through) in holdings.iter_mut().zip(through) {
            holding.start = if self.unread_at(holding.first_hour - 1) {
                HoldingStart::FirstObserved
            } else if self.identity_starts.binary_search(&holding.first_hour).is_ok() {
                HoldingStart::HeldBefore
            } else {
                HoldingStart::Seen
            };
            holding.end = if through >= hour {
                HoldingEnd::Held
            } else {
                match self.gap_at(holding.last_hour + 1) {
                    Some(gap) if gap.last_hour >= hour => HoldingEnd::LastSeen,
                    Some(gap) => HoldingEnd::EndedBy { hour: gap.last_hour + 1 },
                    None => HoldingEnd::Ended,
                }
            };
        }
        holdings
    }
}

fn span_template<'a>(
    span: &IdeaSpan,
    template: Option<&'a Result<BuiltTemplate, String>>,
) -> Result<&'a BuiltTemplate, &'a str> {
    match template {
        Some(Ok(template)) if span.character.is_some() => Ok(template),
        Some(Err(reason)) if span.character.is_some() => Err(reason.as_str()),
        _ => Err("no recorded holder names its template"),
    }
}

fn resolved_slot(
    content: &PoliticsContent,
    token: &str,
    recorded_seat: Option<&str>,
    built: Result<&BuiltTemplate, &str>,
) -> Option<String> {
    let idea = content.ideas.idea(token).ok().flatten();
    let authored_role = content.characters.held_advisor(token, built).ok().flatten();
    let (group_category, gap) = match idea {
        Some(idea) => known(content.ideas.tags.resolve(&idea.group)),
        None => (None, None),
    };
    let named = group_category.zip(idea).is_some_and(|(category, idea)| category.name == idea.group);
    recorded_seat
        .map(str::to_owned)
        .or_else(|| authored_role.map(|role| role.slot.clone()))
        .or_else(|| idea.filter(|_| !named && gap.is_none()).map(|idea| idea.group.clone()))
}

fn look(content: &PoliticsContent, span: &IdeaSpan, template: Option<&Result<BuiltTemplate, String>>, country: IdeaCountry<'_>) -> (IdeaLook, Option<String>) {
    look_with(content, span, template, country, None, true)
}

fn look_with(
    content: &PoliticsContent,
    span: &IdeaSpan,
    template: Option<&Result<BuiltTemplate, String>>,
    country: IdeaCountry<'_>,
    resolve: Option<&mut dyn FnMut(&str) -> Result<String, String>>,
    include_description: bool,
) -> (IdeaLook, Option<String>) {
    let token = span.token.as_str();
    let built = span_template(span, template);
    let mut info = idea_info(content, token, span.slot.as_deref(), built);
    let held = span.character.as_ref().map(|holder| ((!holder.token.is_empty()).then_some(holder.token.as_str()), holder.name.as_str()));
    let (mut artwork, mut problem, name_missing) = match resolve {
        Some(resolve) => {
            let (script, saved) = match held {
                Some((script, saved)) => (script, Some(saved)),
                None => (None, None),
            };
            if include_description {
                content.idea_artwork_scoped(token, script, country, saved, resolve)
            } else {
                content.idea_marker_artwork_scoped(token, script, country, saved, resolve)
            }
        }
        None => {
            let (art, problem) = match held {
                Some((script, saved)) => content.held_idea_artwork(token, script, saved, country),
                None => content.idea_artwork(token, None, country),
            };
            let missing = art.name.is_none().then(|| match held {
                Some((script, saved)) => content.idea_name_missing(token, script, country, Some(saved)),
                None => content.idea_name_missing(token, None, country, None),
            }).flatten();
            (art, problem, missing)
        }
    };
    if held.is_some_and(|(script, _)| script.is_none()) {
        artwork.icon = None;
        artwork.sprite = None;
        problem = Some(format!("{token}: artwork is unknown because the holder has no recorded script token; a placeholder is shown"));
    }
    if artwork.name.is_none() {
        info.name_missing = name_missing;
    }
    let artwork_missing = problem.clone();
    (IdeaLook { info, artwork, artwork_missing }, problem)
}

fn index(at: usize) -> io::Result<u32> {
    u32::try_from(at).map_err(|_| dashboard_core::invalid("An idea reply holds more entries than it can index"))
}

pub(crate) struct Asked<'a> {
    pub params: &'a IdeaHistoryParams,
    pub tag: &'a str,
    pub hour: i32,
    pub unrecorded: Option<String>,
}

pub(crate) fn present(
    query: &Query<'_>,
    content: &Arc<PoliticsContent>,
    asked: &Asked<'_>,
    read: &Read,
    recorded: Option<&Arc<RecordedDefinitions>>,
    cancelled: &AtomicBool,
) -> io::Result<IdeaHistoryReply> {
    check_cancel(cancelled)?;
    if asked.unrecorded.is_some() {
        read.lexical.release();
        read.companions.release();
        read.variables.release_prepared();
    }
    let mut kept = read.presentation.lock().unwrap_or_else(PoisonError::into_inner);
    if !kept.content.upgrade().is_some_and(|held| Arc::ptr_eq(&held, content)) {
        *kept = Presentation { content: Arc::downgrade(content), ..Presentation::default() };
    }
    let definitions = recorded.map_or_else(Weak::new, Arc::downgrade);
    let mut reply = if let Some(cached) = kept.exact.as_ref().filter(|cached|
        cached.hour == asked.hour && cached.branch == asked.params.branch_id
            && cached.tag == asked.tag && cached.unrecorded.as_deref() == asked.unrecorded.as_deref()
            && cached.definitions.ptr_eq(&definitions)
    ) {
        let (reply, noted): (IdeaHistoryReply, catalogue::OwnRefusals) = serde_json::from_slice(&cached.bytes)?;
        catalogue::credit(&noted);
        reply
    } else {
        let (reply, noted) = catalogue::capture(|| present_new(query, content, asked, read, recorded.and_then(|defs| defs.as_ref().as_ref()), &mut kept, cancelled));
        let reply = reply?;
        if let Ok(bytes) = serde_json::to_vec(&(&reply, noted))
            && bytes.len() <= PRESENTATION_BYTES
        {
            kept.exact = Some(Exact {
                hour: asked.hour, branch: asked.params.branch_id.clone(), tag: asked.tag.to_owned(),
                unrecorded: asked.unrecorded.clone(), definitions,
                bytes: bytes.into_boxed_slice(),
            });
        }
        reply
    };
    reply.warnings.extend(content.catalogue.refusal_lines());
    reply.categories = categories(content);
    Ok(reply)
}

#[expect(clippy::too_many_arguments, reason = "presentation uses the recorded history and its bounded cache")]
fn present_new(
    query: &Query<'_>,
    content: &Arc<PoliticsContent>,
    asked: &Asked<'_>,
    read: &Read,
    recorded: Option<&BTreeMap<String, ModifierDefinitionRow>>,
    kept: &mut Presentation,
    cancelled: &AtomicBool,
) -> io::Result<IdeaHistoryReply> {
    let (tag, hour) = (asked.tag, asked.hour);
    let ideas = &read.ideas;
    let mut warnings = vec![dashboard_base::products::artwork::CURRENT_INSTALL_ART_NOTICE.to_owned()];
    warnings.extend(content.warnings.iter().cloned());
    let (_, category_gap) = content.ideas.tags.categories();
    let (_, law_gap) = content.ideas.laws("");
    for gap in category_gap.into_iter().chain(law_gap) {
        warnings.push(format!("Idea categories and laws may be incomplete: {gap}."));
    }
    warnings.extend(ideas.warnings.iter().filter(|(_, at)| **at <= hour).map(|(warning, _)| (*warning).to_owned()));
    let scoped = ScopedText::new(query.replay, &content.catalogue)
        .with_keyed_variables(Arc::clone(&read.variables)).with_lexical(&read.lexical)
        .with_companions(&read.companions);
    let mut validity = Validity::default();
    let unnamed = NamingCountry::default();
    let mut looks: Vec<IdeaLook> = Vec::new();
    let mut keyed: BTreeMap<(HoldingKey<'_>, Option<usize>, Option<i32>, &str), u32> = BTreeMap::new();
    let mut unplaced: BTreeSet<&str> = BTreeSet::new();
    let mut holdings = Vec::new();
    let slot_at = |at_hour: i32| {
        query.timeline.at(at_hour).and_then(|stretch| {
            query.holders.at(&asked.params.country, at_hour).and_then(|run| match run.holder {
                Holder::Found { slot, .. } => Some((stretch.run, slot)),
                Holder::Unresolved => None,
            })
        })
    };
    let describe = |holding: &Holding| {
        let span = &ideas.spans[holding.span];
        let identity = ideas.identity_starts.partition_point(|start| *start <= span.first_hour).checked_sub(1);
        let named = identity.and_then(|identity| read.naming.get(identity)).unwrap_or(&unnamed);
        let at_hour = holding.last_at(hour);
        let holding_tag = query.holders.tag_at(&asked.params.country, at_hour).unwrap_or(tag);
        (span, identity, named.for_tag(holding_tag), at_hour, holding_tag)
    };
    let scripted = |span: &IdeaSpan, country| {
        let character = span.character.as_ref();
        let token = character.map(|holder| holder.token.as_str()).filter(|token| !token.is_empty());
        content.idea_is_scripted(&span.token, token, country, character.map(|holder| holder.name.as_str()))
    };
    let scoped_look = |holding: &Holding, country, at_hour, scope_target, mut validity: Option<&mut Validity>| {
        let mut resolve = |key: &str| match scope_target {
            Some((run, slot)) => scoped.localise(run, at_hour, Scope::Country(slot), key, &[], validity.as_deref_mut()),
            None => Err(format!("localisation {key}: country scope is unavailable at hour {at_hour}")),
        };
        look_with(content, &ideas.spans[holding.span], ideas.templates.get(&holding.span), country, Some(&mut resolve), true)
    };
    let previous = kept.holdings.as_ref().filter(|plan| plan.through < hour).map(|plan| plan.hour);
    let held_structure = kept.holdings(ideas, hour);
    let current_scope = slot_at(hour);
    let mut finished = BTreeMap::new();
    if let Some(previous) = previous
        && let Some((run, _)) = current_scope
        && query.timeline.at(previous).is_some_and(|stretch| stretch.run == run)
    {
        let mut ended: Vec<_> = held_structure.iter().filter(|holding| {
            holding.end != HoldingEnd::Held && (previous..hour).contains(&holding.last_hour)
        }).collect();
        ended.sort_by_key(|holding| holding.last_hour);
        for holding in ended {
            check_cancel(cancelled)?;
            let (span, _, country, at_hour, holding_tag) = describe(holding);
            let scope_target = slot_at(at_hour);
            let (look, noted) = catalogue::capture_isolated(|| {
                scripted(span, country).then(|| {
                    kept.past((holding.span, at_hour, scope_target, holding_tag.into()), || scoped_look(holding, country, at_hour, scope_target, None))
                }).transpose()
            });
            if let Some(look) = look? {
                finished.insert(holding.span, (look, noted));
            }
        }
    }
    for holding in held_structure.iter() {
        check_cancel(cancelled)?;
        let (span, identity, country, at_hour, holding_tag) = describe(holding);
        let eval_hour = scripted(span, country).then_some(at_hour);
        let key = (holding_key(span), identity, eval_hour, holding_tag);
        let at = match keyed.get(&key) {
            Some(&at) => at,
            None => {
                let (found, problem) = match eval_hour {
                    Some(at_hour) => {
                        let scope_target = slot_at(at_hour);
                        if holding.end == HoldingEnd::Held {
                            scoped_look(holding, country, at_hour, scope_target, Some(&mut validity))
                        } else if let Some((look, noted)) = finished.remove(&holding.span) {
                            catalogue::credit(&noted);
                            look
                        } else {
                            kept.past((holding.span, at_hour, scope_target, holding_tag.into()), || scoped_look(holding, country, at_hour, scope_target, None))?
                        }
                    }
                    None => read.looked(content, holding_tag, holding.span),
                };
                if found.info.category.is_none() {
                    unplaced.insert(span.token.as_str());
                }
                warnings.extend(problem);
                let at = index(looks.len())?;
                looks.push(found);
                keyed.insert(key, at);
                at
            }
        };
        holdings.push(IdeaHolding {
            token: span.token.clone(),
            first_hour: holding.first_hour,
            last_hour: at_hour,
            start: holding.start,
            end: holding.end,
            slot: span.slot.clone(),
            character: span.character.clone(),
            look: at,
        });
    }
    if !unplaced.is_empty() {
        warnings.push(format!("{} ideas are in no category the content defines: {}.", unplaced.len(), unplaced.iter().take(8).copied().collect::<Vec<_>>().join(", ")));
    }
    let name = |holding: &IdeaHolding| looks[holding.look as usize].artwork.name.clone().unwrap_or_else(|| holding.token.clone());
    let mut held: Vec<(i32, String, &str, usize)> = holdings
        .iter()
        .enumerate()
        .filter(|(_, holding)| holding.end == HoldingEnd::Held)
        .map(|(at, holding)| (holding.first_hour, name(holding), holding.token.as_str(), at))
        .collect();
    held.sort_unstable();
    let held = held.into_iter().map(|(_, _, _, at)| index(at)).collect::<io::Result<Vec<_>>>()?;
    let (modifier_keys, text_keys) = effect_keys(looks.iter().map(|look| &look.info));
    let mut localise_text = |key: &str, args: &[(&str, content_reader_localisation::localisation::Argument<'_>)]| {
        if let Some((run, slot)) = current_scope
            && scoped_text::key_needs_scope(&content.catalogue.localisation, key, args)
        {
            return scoped.localise(run, hour, Scope::Country(slot), key, args, Some(&mut validity)).ok();
        }
        content.catalogue.localised_with(key, args, View::Scripted)
    };
    let (modifiers, format_warnings) = idea_format::modifier_formats_with(recorded, &modifier_keys, content, &mut localise_text);
    warnings.extend(format_warnings);
    let texts = idea_format::texts_with(&text_keys, &mut |key| localise_text(key, &[]));
    let unobserved = ideas
        .unobserved
        .iter()
        .filter(|gap| gap.first_hour <= hour)
        .map(|gap| HourSpan { first_hour: gap.first_hour, last_hour: gap.last_hour.min(hour) })
        .collect();
    let mut next_change = ideas.next_change(hour);
    if matches!(asked.params.country, Country::Slot(_)) {
        let next_naming = query
            .holders
            .naming_spans(&asked.params.country)
            .into_iter()
            .flat_map(|(first, last)| [Some(first), last.checked_add(1)])
            .flatten()
            .filter(|&edge| edge > hour)
            .min();
        next_change = [next_change, next_naming].into_iter().flatten().min();
    }
    if !validity.is_empty()
        && next_change != Some(hour.saturating_add(1))
        && let Some(stretch) = query.timeline.at(hour)
    {
        let mut dates = read.dates.lock().unwrap_or_else(PoisonError::into_inner);
        let through = validity.through(&mut dates, query.replay, stretch.run, stretch.first, hour, stretch.last, cancelled)?;
        let scripted_next = if through < stretch.last {
            Some(through.saturating_add(1))
        } else if stretch.last < query.timeline.last {
            Some(stretch.last.saturating_add(1))
        } else {
            None
        };
        next_change = [next_change, scripted_next].into_iter().flatten().min();
    }
    Ok(IdeaHistoryReply {
        branch_id: asked.params.branch_id.clone(),
        tag: tag.to_owned(),
        asked: asked.params.country.clone(),
        hour,
        next_change,
        unrecorded: asked.unrecorded.clone(),
        holdings,
        held,
        looks,
        categories: Vec::new(),
        unobserved,
        modifiers,
        texts,
        warnings,
    })
}

type Looked = (IdeaLook, Option<String>);
type CachedLook = (String, Looked, catalogue::OwnRefusals);
type HistoricalKey = (usize, i32, Option<(u32, u32)>, Box<str>);

const PRESENTATION_BYTES: usize = 512 * 1024;

struct Exact {
    hour: i32,
    branch: String,
    tag: String,
    unrecorded: Option<String>,
    definitions: Weak<RecordedDefinitions>,
    bytes: Box<[u8]>,
}

struct HoldingPlan {
    hour: i32,
    through: i32,
    holdings: Arc<[Holding]>,
}

#[derive(Default)]
struct Presentation {
    content: Weak<PoliticsContent>,
    exact: Option<Exact>,
    holdings: Option<HoldingPlan>,
    past: VecDeque<(HistoricalKey, Box<[u8]>)>,
    bytes: usize,
}

impl Presentation {
    fn holdings(&mut self, ideas: &Ideas, hour: i32) -> Arc<[Holding]> {
        if let Some(held) = &mut self.holdings
            && (held.hour..=held.through).contains(&hour)
        {
            held.hour = hour;
            return Arc::clone(&held.holdings);
        }
        let holdings: Arc<[Holding]> = ideas.at(hour).into();
        self.holdings = (size_of_val(&*holdings) + 2 * size_of::<usize>() <= PRESENTATION_BYTES).then(|| HoldingPlan {
            hour, through: ideas.next_change(hour).map_or(i32::MAX, |next| next - 1), holdings: Arc::clone(&holdings),
        });
        holdings
    }

    fn past(&mut self, key: HistoricalKey, make: impl FnOnce() -> Looked) -> io::Result<Looked> {
        if let Some((_, bytes)) = self.past.iter().find(|(held, _)| *held == key) {
            let (look, noted): (Looked, catalogue::OwnRefusals) = serde_json::from_slice(bytes)?;
            catalogue::credit(&noted);
            return Ok(look);
        }
        let (look, noted) = catalogue::capture(make);
        if let Ok(bytes) = serde_json::to_vec(&(&look, noted)) {
            let size = bytes.len() + key.3.len() + std::mem::size_of::<(HistoricalKey, Box<[u8]>)>();
            if size <= PRESENTATION_BYTES {
                while self.bytes + size > PRESENTATION_BYTES {
                    let (key, bytes) = self.past.pop_front().expect("retained bytes");
                    self.bytes -= bytes.len() + key.3.len() + std::mem::size_of::<(HistoricalKey, Box<[u8]>)>();
                }
                self.bytes += size;
                self.past.push_back((key, bytes.into_boxed_slice()));
            }
        }
        Ok(look)
    }
}

pub(crate) struct Read {
    pub ideas: Ideas,
    pub naming: Vec<NamingCountry>,
    looks: Mutex<(Weak<PoliticsContent>, Vec<Vec<CachedLook>>)>,
    dates: Arc<Mutex<ReadDates>>,
    variables: Arc<KeyedVariables>,
    lexical: Lexical,
    companions: CompanionTables,
    presentation: Mutex<Presentation>,
}

impl Read {
    fn looked(&self, content: &Arc<PoliticsContent>, tag: &str, run: usize) -> Looked {
        let mut kept = self.looks.lock().unwrap_or_else(PoisonError::into_inner);
        if !kept.0.upgrade().is_some_and(|held| Arc::ptr_eq(&held, content)) {
            *kept = (Arc::downgrade(content), vec![Vec::new(); self.ideas.spans.len()]);
        }
        let bucket = &mut kept.1[run];
        let pos = match bucket.iter().position(|(cached_tag, _, _)| cached_tag == tag) {
            Some(pos) => pos,
            None => {
                let span = &self.ideas.spans[run];
                let unnamed = NamingCountry::default();
                let identity = self.ideas.identity_starts.partition_point(|start| *start <= span.first_hour).checked_sub(1);
                let named = identity.and_then(|identity| self.naming.get(identity)).unwrap_or(&unnamed);
                let (looked, noted) = catalogue::capture(|| look(content, span, self.ideas.templates.get(&run), named.for_tag(tag)));
                bucket.push((tag.to_owned(), looked, noted));
                bucket.len() - 1
            }
        };
        let (_, looked, noted) = &bucket[pos];
        catalogue::credit(noted);
        looked.clone()
    }
}

type HeldHistory = Arc<Read>;

const KEPT_HISTORIES: usize = 8;

#[derive(Default)]
struct Kept(Vec<((u64, Country), Arc<Mutex<Option<HeldHistory>>>)>);

pub(crate) fn history(source: &Source, branch: u64, tag: &Country) -> io::Result<HeldHistory> {
    let slot = {
        let state = source.state::<Kept>();
        let mut kept = state.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (branch, tag.clone());
        let entry = match kept.0.iter().position(|(held, _)| *held == key) {
            Some(at) => kept.0.remove(at),
            None => (key, Arc::new(Mutex::new(None))),
        };
        let slot = Arc::clone(&entry.1);
        kept.0.push(entry);
        if kept.0.len() > KEPT_HISTORIES {
            kept.0.remove(0);
        }
        slot
    };
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(read) = held.as_ref() {
        return Ok(Arc::clone(read));
    }
    let query = dashboard_base::queries::query(source, branch)?;
    let ideas = read(&query, tag, &AtomicBool::new(false))?;
    let mut frames = dashboard_base::products::at_hour::Frames::new(&query)?;
    let naming = ideas
        .identity_starts
        .iter()
        .zip(&ideas.starts)
        .map(|(&hour, &(_, _, id))| naming_at(&mut frames, hour, id))
        .collect::<io::Result<Vec<_>>>()?;
    let variables = KeyedVariables::for_source(source);
    let read = Arc::new(Read { ideas, naming, looks: Mutex::new((Weak::new(), Vec::new())), dates: ReadDates::for_selected_source(source, DATE_SELECTIONS),
        variables, lexical: Lexical::for_source(source), companions: CompanionTables::for_source(source),
        presentation: Mutex::default() });
    *held = Some(Arc::clone(&read));
    Ok(read)
}

pub(crate) fn load_for_markers(
    source: &Source,
    params: &IdeaHistoryParams,
    context: &LoadContext,
    cancelled: &AtomicBool,
) -> io::Result<IdeaHistoryReply> {
    let content = politics_content(source, context.content()?)?;
    let branch = params.branch_id.parse().map_err(|_| dashboard_core::invalid("Invalid branch"))?;
    let query = dashboard_base::queries::query(source, branch)?;
    let read = history(source, branch, &params.country)?;
    let hour = params.hour.min(query.timeline.last);
    let tag = query.holders.name(&params.country, hour);
    let ideas = &read.ideas;
    let categories = categories(&content);
    let laws: BTreeSet<&str> = categories
        .iter()
        .flat_map(|category| category.slots.iter().flat_map(|slot| slot.laws.iter().map(String::as_str)))
        .collect();
    let seats: BTreeSet<&str> = categories
        .iter()
        .flat_map(|category| category.slots.iter().filter(|slot| slot.character).map(|slot| slot.token.as_str()))
        .collect();
    let mut warnings = vec![dashboard_base::products::artwork::CURRENT_INSTALL_ART_NOTICE.to_owned()];
    warnings.extend(content.warnings.iter().cloned());
    let (_, category_gap) = content.ideas.tags.categories();
    let (_, law_gap) = content.ideas.laws("");
    for gap in category_gap.into_iter().chain(law_gap) {
        warnings.push(format!("Idea categories and laws may be incomplete: {gap}."));
    }
    warnings.extend(ideas.warnings.iter().filter(|(_, at)| **at <= hour).map(|(warning, _)| (*warning).to_owned()));
    let scoped = ScopedText::new(query.replay, &content.catalogue);
    let unnamed = NamingCountry::default();
    let mut looks: Vec<IdeaLook> = Vec::new();
    let mut keyed: BTreeMap<(HoldingKey<'_>, Option<usize>, Option<i32>, &str), u32> = BTreeMap::new();
    let mut holdings = Vec::new();
    for holding in ideas.at(hour) {
        check_cancel(cancelled)?;
        if holding.start != HoldingStart::Seen {
            continue;
        }
        let span = &ideas.spans[holding.span];
        let is_law = laws.contains(span.token.as_str())
            || content.idea_is_law(&span.token).ok().and_then(|v| *v).unwrap_or(false);
        let slot = resolved_slot(
            &content,
            &span.token,
            span.slot.as_deref(),
            span_template(span, ideas.templates.get(&holding.span)),
        );
        let is_seat = slot.as_deref().is_some_and(|slot| seats.contains(slot));
        if !is_law && !is_seat {
            continue;
        }
        let identity = ideas.identity_starts.partition_point(|start| *start <= span.first_hour).checked_sub(1);
        let named = identity.and_then(|identity| read.naming.get(identity)).unwrap_or(&unnamed);
        let held = span.character.as_ref().map(|holder| ((!holder.token.is_empty()).then_some(holder.token.as_str()), holder.name.as_str()));
        let holding_tag = query.holders.tag_at(&params.country, holding.first_hour).unwrap_or(tag.as_str());
        let is_scripted = content.idea_name_is_scripted(&span.token, held.and_then(|(s, _)| s), named.for_tag(holding_tag), held.map(|(_, saved)| saved));
        let eval_hour = is_scripted.then_some(holding.first_hour);
        let key = (holding_key(span), identity, eval_hour, holding_tag);
        let at = match keyed.get(&key) {
            Some(&at) => at,
            None => {
                let (found, problem) = match eval_hour {
                    Some(at_hour) => {
                        let scope_target = query.timeline.at(at_hour).and_then(|stretch| {
                            query.holders.at(&params.country, at_hour).and_then(|run| match run.holder {
                                Holder::Found { slot, .. } => Some((stretch.run, slot)),
                                Holder::Unresolved => None,
                            })
                        });
                        let mut resolve = |key: &str| match scope_target {
                            Some((run, slot)) => scoped.localise(run, at_hour, Scope::Country(slot), key, &[], None),
                            None => Err(format!("localisation {key}: country scope is unavailable at hour {at_hour}")),
                        };
                        look_with(&content, span, ideas.templates.get(&holding.span), named.for_tag(holding_tag), Some(&mut resolve), false)
                    }
                    None => read.looked(&content, holding_tag, holding.span),
                };
                warnings.extend(problem);
                let at = index(looks.len())?;
                looks.push(found);
                keyed.insert(key, at);
                at
            }
        };
        holdings.push(IdeaHolding {
            token: span.token.clone(),
            first_hour: holding.first_hour,
            last_hour: holding.last_hour,
            start: holding.start,
            end: holding.end,
            slot: span.slot.clone(),
            character: span.character.clone(),
            look: at,
        });
    }
    warnings.extend(content.catalogue.refusal_lines());
    Ok(IdeaHistoryReply {
        branch_id: params.branch_id.clone(),
        tag,
        asked: params.country.clone(),
        hour,
        next_change: ideas.next_change(hour),
        unrecorded: None,
        holdings,
        held: Vec::new(),
        looks,
        categories,
        unobserved: Vec::new(),
        modifiers: Vec::new(),
        texts: Vec::new(),
        warnings,
    })
}

pub struct IdeaHistory;

const DATE_SELECTIONS: &[DateSelection] = &[
    DateSelection { table: TableId::ActiveIdeas, fields: mask::<rows::ActiveIdeaRow>(&["token"]), shift: Some(32) },
    DateSelection { table: TableId::ScriptVariables, fields: mask::<rows::ScriptVariablesRow>(&["entries"]), shift: Some(0) },
    DateSelection { table: TableId::HiredAdvisors, fields: mask::<rows::HiredAdvisorRow>(&["idea"]), shift: Some(32) },
    DateSelection { table: TableId::CountryCharacters, fields: mask::<rows::CountryCharacterRow>(&["character"]), shift: None },
    DateSelection { table: TableId::AdvisorTokens, fields: mask::<rows::AdvisorTokenRow>(&["token"]), shift: None },
    DateSelection { table: TableId::CountryAppearance, fields: mask::<rows::CountryAppearanceRow>(&["cosmetic_tag", "name_tag", "leader_ideology", "overlord"]), shift: None },
    DateSelection { table: TableId::FactionAdvisors, fields: mask::<rows::FactionAdvisorRow>(&["token"]), shift: None },
    DateSelection { table: TableId::CountryRetiredCharacters, fields: mask::<rows::CountryCharacterRow>(&["character"]), shift: None },
    DateSelection { table: TableId::ReservedCountryNaming, fields: mask::<rows::ReservedCountryNamingRow>(&["tag", "name_tag", "cosmetic_tag", "leader_ideology", "group", "overlord", "autonomy", "built"]), shift: None },
];

impl Product for IdeaHistory {
    const KEY: &'static str = "idea-history";
    const CONTENT_DEPENDENT: bool = true;
    const PREPARE: Option<Prepare<Self::Params>> = Some(|source, params, load| {
        if load.content.is_none() { return Ok(None); }
        let branch = params.branch_id.parse().map_err(|_| dashboard_core::invalid("Invalid branch"))?;
        let query = dashboard_base::queries::query(source, branch)?;
        let hour = params.hour.min(query.timeline.last);
        let Some(span) = query.timeline.at(hour) else { return Ok(None) };
        if let Some(holding) = query.holders.at(&params.country, hour)
            && let Holder::Found { id, .. } = holding.holder
        {
            KeyedVariables::for_source(source).prepare_country(span.run, id);
        }
        let lexical = Lexical::for_source(source);
        let companions = CompanionTables::for_source(source);
        let dates = ReadDates::prepare_selected(source, span.run, (span.first, span.last), DATE_SELECTIONS,
            move |replay, run| {
                lexical.prepare_checkpoints(replay, run);
                companions.prepare_characters(replay, run);
            })?;
        dashboard_base::products::read_ahead::together(
            dates,
            dashboard_base::products::artwork::prepare_artwork(source, load)?,
        )
    });
    type Params = IdeaHistoryParams;
    const FIELDS: dashboard_base::products::Fields = dashboard_base::fields![branchId, country, hour: Read];
    type Reply = IdeaHistoryReply;

    #[cfg(any(test, feature = "export-types"))]
    fn types() -> Vec<String> {
        vec![
            IdeaHistoryParams::decl(),
            IdeaHistoryReply::decl(),
            IdeaHolding::decl(),
            HoldingStart::decl(),
            HoldingEnd::decl(),
            IdeaLook::decl(),
            IdeaCharacter::decl(),
            EffectLine::decl(),
            EffectGroup::decl(),
            TraitEffects::decl(),
            ModifierFormat::decl(),
            LocalisedText::decl(),
            IdeaInfo::decl(),
            SlotInfo::decl(),
            CategoryInfo::decl(),
        ]
    }

    fn validate(params: &Self::Params) -> Result<(), &'static str> {
        params.branch_id.parse::<u64>().map_err(|_| "Invalid branch")?;
        validate_country(&params.country)
    }

    fn load(source: &Source, params: &Self::Params, context: &LoadContext, cancelled: &AtomicBool) -> io::Result<Self::Reply> {
        let content = politics_content(source, context.content()?)?;
        let branch = params.branch_id.parse().map_err(|_| dashboard_core::invalid("Invalid branch"))?;
        let query = dashboard_base::queries::query(source, branch)?;
        let (read, recorded) = rayon::join(|| history(source, branch, &params.country), || recorded_definitions(source, &query));
        let (read, recorded) = (read?, recorded?);
        let hour = params.hour.min(query.timeline.last);
        let unrecorded = if hour < query.timeline.first {
            Some(BEFORE_BRANCH.to_owned())
        } else {
            read.ideas.unread_at(hour).then(|| NOT_READ.to_owned())
        };
        let tag = query.holders.name(&params.country, hour);
        let asked = Asked { params, tag: &tag, hour, unrecorded };
        let reply = present(&query, &content, &asked, &read, Some(&recorded), cancelled)?;
        check_reply_budget(&reply)?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests;
