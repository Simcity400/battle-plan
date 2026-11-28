import { createSnapshotIdentityProvider } from '../identity/provider.ts';
import { IdentityIndex } from '../identity/index.ts';
import { PENDING_ART, productArt, readyArt, unavailableArt, type ArtResult } from '../art/types.ts';
import assert from 'node:assert/strict';
import test from 'node:test';
import { CELL, parseAnswer } from '../../dashboard/follow/message.ts';
import { parseFollowed } from '../../test-support/followMessage.ts';
import { message, type Spec } from '../../dashboard/follow/messages.fixture.ts';
import { assertContinuity, assertFreshPaint, assertQuietFrames, assertTooltipDated, driveLane, firstRow, installRecordingCanvas, LaneTransport } from '../laneRuntime.fixture.ts';
import { repliedCells } from '../../dashboard/follow/liveCells.ts';
import { liveKey } from '../../charts/live.ts';
import { valuesOf } from '../../test-support/liveValues.ts';
import { ROW_HEIGHT, tableSettingsOf, type TableSettings, type TableTheme } from './settings.ts';
import { datedIdentity } from '../identity/dated.fixture.ts';
import { resolveHeadSpan } from '../broker.ts';
import { tableHostAdapter, type TableChromeState } from './hostAdapter.ts';
import { tableLane } from './laneView.ts';
import { registerFamilyProjection } from '../registry.ts';
import type { ApmReply, CampaignClockReply, DivisionTemplatesReply, MarketLedgerTailReply, ScoreboardColumn, ScoreboardReply, ScriptPageRow } from '../../generated/dashboard';
import { pictureError } from '../../shell/perf.ts';
import { CLOCK_COLUMNS, CLOCK_TABLE_BASE, campaignClockProjection } from '../../widgets/campaign-clock/clockProjection.ts';
import { decisionsProjection } from '../../widgets/decisions/decisionsProjection.ts';
import { divisionTemplateColumns, divisionTemplateProjection } from '../../widgets/division-template/divisionTemplateProjection.ts';
import { eventChoicesProjection } from '../../widgets/event-choices/eventChoicesProjection.ts';
import { eventFeedProjection } from '../../widgets/event-feed/eventFeedProjection.ts';
import { gameScoreColumns, gameScoreProjection } from '../../widgets/game-score/gameScoreProjection.ts';
import { marketLedgerProjection } from '../../widgets/market-ledger/marketLedgerProjection.ts';
import { buildRowSourceFromHead, validateGenericTailReply } from './rowSource.ts';
import { logCountryCell } from './logHelpers.ts';
import { COLUMNS as OCCUPIED_DEFAULTS, TABLE_BASE as OCCUPIED_BASE } from '../../widgets/occupied-states/definition.ts';
import { occupiedStatesProjection } from '../../widgets/occupied-states/occupiedProjection.ts';
import { apmTableColumns, apmTableProjection } from '../../widgets/player-apm/apmProjection.ts';
import { TABLE_BASE as SCOREBOARD_BASE } from '../../widgets/scoreboard/definition.ts';
import { RANK_SOURCE, scoreboardColumnsSpec, scoreboardProjection } from '../../widgets/scoreboard/scoreboardProjection.ts';
import { AuditSnapshotStore, canonicalAuditData, canonicalAuditValue } from '../audit.ts';
import { CadenceGate, TrailingFlushScheduler } from '../cadence.ts';
import { createBoundedReadView } from '../boundedView.ts';
import { holderRunAt, nextHoldersChange, nextRangesChange, nextWindowRangesChange, overlapsHourRanges, resolveSlotHolderRuns } from '../holders.ts';
import { showBundle } from '../readUnits.ts';
import { effectiveCountryUnobserved } from '../sources.ts';
import { SlotTags } from '../../dashboard/countrySlots.ts';
import { capturePictures, capturePicturesBatch } from '../../dashboard/pulled.ts';
import { isFamilyAuditing, subscribeFamilyAuditing } from '../audit.ts';
import { InvalidFamilyValueError, knownValue, notApplicable, notRecorded, partialValue, zeroValue, type Value } from '../value.ts';
import { buildColumnLayouts, layoutFamilyTable, measureFixedColumnWidths } from './layout.ts';
import { computeColumnMaxima, declareTableProjection, tableRank, deriveTableAudit, deriveTableMirror, deriveTableTooltip, resolveCellPaintModel, resolveTableRows, resolveTableRowsIncremental, updateColumnMaximaIncremental, type FlatTableRow, type TableColumnSpec, type TableModel, type TableModelSlot, type TableRow } from './model.ts';
import { identityCell, resolveName } from './identityCell.ts';
import { createTablePicture, drawFamilyTable, glideCrossing, hasGlideCrossing, hitFamilyTable, NO_GLIDE, tableGlide } from './painter.ts';
import { combinedPaneRows, resolvePanes } from './panes.ts';
import { createPlanAuditVerifier } from '../host/auditBinding.ts';
import { LaneArtCache } from '../art/laneCache.ts';
import { treeLevelsOf, treeTextStart } from './cellPainters.ts';
import { TREE_CHEVRON_LANE } from './layout.ts';
import { computeRowLayout, TABLE_WINDOW_OVERSCAN, tableRowWindow } from './layout.ts';
import { NOT_NAMED } from '../../widgets/occupied-states/occupiedStatesData.ts';
import { createLaneView } from '../laneRuntime.ts';
import { PayloadSender } from '../../dashboard/follow/payloads.ts';
import { valueStateText } from '../../components/valueStateText.ts';
import { engineHour } from '../../dashboard/calendar.ts';
import { formatDateHover } from '../valueFormat.ts';

const NO_OPTIONS = {};
const NO_SOURCES = {};
const answer = (spec: Spec) => parseFollowed(message(100, [{ id: 1, answer: spec }])).answers[0]!.answer;

const floatBits = (value: number): bigint => {
  const view = new DataView(new ArrayBuffer(8));
  view.setFloat64(0, value, true);
  return view.getBigInt64(0, true);
};

function headTextAnswer(rows: readonly { readonly key: bigint; readonly id: string; readonly values: readonly (bigint | string | null)[] }[]) {
  const cols = rows[0]?.values.length ?? 0;
  const texts: string[] = [];
  const held = new Map<string, number>();
  const intern = (s: string) => {
    let at = held.get(s);
    if (at === undefined) held.set(s, at = texts.push(s) - 1);
    return at;
  };
  const names: number[] = [];
  const rawVals: bigint[] = [];
  const states: number[] = [];
  for (const row of rows) {
    names.push(intern(row.id));
    for (const v of row.values) {
      if (v === null) { rawVals.push(0n); states.push(CELL.missing); }
      else if (typeof v === 'string') { rawVals.push(BigInt(intern(v))); states.push(CELL.value); }
      else { rawVals.push(v); states.push(CELL.value); }
    }
  }
  const bytes: number[] = [];
  const u8 = (v: number) => bytes.push(v & 0xff);
  const u32 = (v: number) => { for (let i = 0; i < 4; i++) u8(Number((BigInt(v) >> BigInt(8 * i)) & 0xffn)); };
  const u64 = (v: bigint) => { for (let i = 0; i < 8; i++) u8(Number((BigInt.asUintN(64, v) >> BigInt(8 * i)) & 0xffn)); };
  const pad = () => { while (bytes.length % 8) u8(0); };
  u8(1); u8(1); u8(0); u8(0); u32(rows.length); u32(cols); u32(texts.length);
  rows.forEach(r => u64(r.key));
  rows.forEach(r => u64(r.key >> 64n));
  rawVals.forEach(v => u64(v));
  states.forEach(s => u8(s)); pad();
  for (let i = 0; i < rows.length; i++) u32(0xffff_ffff); pad();
  names.forEach(n => u32(n)); pad();
  const enc = texts.map(t => new TextEncoder().encode(t));
  let end = 0; u32(0);
  for (const b of enc) { end += b.length; u32(end); }
  pad();
  for (const b of enc) b.forEach(u8);
  pad();
  return parseAnswer(new Uint8Array(bytes));
}

const TEST_THEME: TableTheme = {
  surface: '#111111',
  text: '#eeeeee',
  muted: '#888888',
  line: '#333333',
  lineSoft: '#222222',
  accent: '#3b82f6',
  blue: '#3b82f6',
  red: '#ef4444',
  amber: '#f59e0b',
  green: '#22c55e',
  grey: '#888888',
  hatch: '#555555',
  font: 'sans-serif',
  fontSize: { small: 11, body: 13 },
  lineWidth: 1,
  hatchSpacing: 6,
};

function instrumentedContext() {
  let measureCalls = 0;
  let fillTextCalls = 0;
  const ctx = {
    font: '',
    fillStyle: '',
    strokeStyle: '',
    lineWidth: 1,
    textAlign: 'left',
    textBaseline: 'alphabetic',
    getTransform: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }),
    save() {},
    restore() {},
    beginPath() {},
    closePath() {},
    rect() {},
    roundRect() {},
    clip() {},
    fillRect() {},
    strokeRect() {},
    clearRect() {},
    moveTo() {},
    lineTo() {},
    stroke() {},
    fill() {},
    arc() {},
    drawImage() {},
    setLineDash() {},
    createLinearGradient() { return { addColorStop() {} }; },
    measureText(text: string) {
      measureCalls++;
      return { width: text.length * 7 };
    },
    fillText() {
      fillTextCalls++;
    },
  };
  return {
    ctx: ctx as unknown as CanvasRenderingContext2D,
    counts: () => ({ measureCalls, fillTextCalls }),
    reset: () => { measureCalls = 0; fillTextCalls = 0; },
  };
}

test('declarative table projection refuses bare null where a family value is required', () => {
  const broken = declareTableProjection<{ items: { id: string; count: number }[] }, { id: string; count: number }, Record<string, never>>({
    kind: 'test-null-guard',
    columns: [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, value: row => knownValue(row.id) },
      { key: 'count', label: 'Count', as: 'plain', offered: ['plain'], priority: 1, numeric: true, value: () => null as unknown as ReturnType<typeof knownValue> },
    ],
    rows: reply => ({ rows: reply.items }),
    rowKey: row => row.id,
    rowName: row => row.id,
  });
  const model = broken.project(createBoundedReadView({
    hour: 10,
    panels: new Map([['main', { items: [{ id: 'a', count: 1 }] }]]),
  }), {});
  const settings = tableSettingsOf(undefined, model.columns, { density: 'comfortable', sort: { column: 'count', direction: 'descending' } });
  assert.throws(() => resolveTableRows(model, settings), InvalidFamilyValueError);
});

test('campaign-clock projection produces identical audit output for followed wire answers and exact replies', () => {
  const wire = answer({
    keys: [0n],
    columns: 4,
    values: [floatBits(3661), floatBits(2400), floatBits(1261), 1n],
    states: [CELL.value, CELL.value, CELL.value, CELL.value],
  });
  const reply: CampaignClockReply = {
    available: true,
    recordedSeconds: 3661,
    playingSeconds: 2400,
    pausedSeconds: 1261,
    complete: true,
    warnings: [],
  };
  const settings = tableSettingsOf(undefined, CLOCK_COLUMNS, CLOCK_TABLE_BASE);
  const clockLayouts = buildColumnLayouts(CLOCK_COLUMNS, settings);
  const followedModel = campaignClockProjection.project(createBoundedReadView({ hour: 50, answers: new Map([['main', wire]]) }), {});
  const exactModel = campaignClockProjection.project(createBoundedReadView({ hour: 50, panels: new Map([['main', reply]]) }), {});
  const followedAudit = deriveTableAudit({ model: followedModel, flatRows: resolveTableRows(followedModel, settings), visibleColumns: clockLayouts });
  const exactAudit = deriveTableAudit({ model: exactModel, flatRows: resolveTableRows(exactModel, settings), visibleColumns: clockLayouts });
  assert.equal(pictureError('table', followedAudit, { values: new Map(exactAudit.map(r => [r.key, r.value!])), ordered: true }), null);
  assert.ok(followedAudit.find(r => r.key === 'total:time')?.value?.includes(canonicalAuditValue(knownValue(3661))));
});

test('scoreboard projection matches followed and exact audits with missing, partial, zero, and tied rows', () => {
  const columns: ScoreboardColumn[] = [
    { key: 'factories', label: 'Factories', unit: 'factories', decimals: 0, available: true, reason: null, family: { kind: 'f', measure: 'm' }, partial: null, noneAtZero: false },
    { key: 'casualties', label: 'Casualties', unit: 'people', decimals: 0, available: true, reason: null, family: { kind: 'c', measure: 't' }, partial: { kind: 'c', measure: 'p' }, noneAtZero: true },
  ];
  const identity = datedIdentity({
    GER: [{ from: 0, name: 'Germany' }],
    FRA: [{ from: 0, name: 'France' }],
    ITA: [{ from: 0, name: 'Italy' }],
    ENG: [{ from: 0, name: 'United Kingdom' }],
  });
  const rankAns = answer({ keys: [1n, 2n, 3n, 4n], columns: 1, names: ['ITA', 'FRA', 'GER', 'ENG'], values: [10n, 10n, 0n, 0n], states: [CELL.value, CELL.value, CELL.value, CELL.value] });
  const facAns = answer({
    keys: [1n, 2n, 3n, 4n],
    columns: 1,
    names: ['ITA', 'FRA', 'GER', 'ENG'],
    values: [10n, 10n, 0n, 0n],
    states: [CELL.value, CELL.value, CELL.value, CELL.missing],
    reasons: [null, null, null, 'Factories not recorded'],
  });
  const casAns = answer({
    keys: [1n, 2n, 3n, 4n],
    columns: 1,
    names: ['ITA', 'FRA', 'GER', 'ENG'],
    values: [500n, 0n, 200n, 0n],
    states: [CELL.value, CELL.value, CELL.value, CELL.value],
  });
  const partAns = answer({
    keys: [1n, 2n, 3n, 4n],
    columns: 1,
    names: ['ITA', 'FRA', 'GER', 'ENG'],
    values: [1n, 0n, 0n, 0n],
    states: [CELL.value, CELL.value, CELL.value, CELL.value],
  });
  const exactReply: ScoreboardReply = {
    branchId: '1',
    hour: 40,
    missing: null,
    warnings: [],
    rows: [
      { country: 'GER', tag: 'GER', cells: [{ valueRaw: '0', missing: null, partial: false }, { valueRaw: '200', missing: null, partial: false }] },
      { country: 'ENG', tag: 'ENG', cells: [{ valueRaw: null, missing: 'Factories not recorded', partial: false }, { valueRaw: '0', missing: null, partial: false }] },
      { country: 'ITA', tag: 'ITA', cells: [{ valueRaw: '10', missing: null, partial: false }, { valueRaw: '500', missing: null, partial: true }] },
      { country: 'FRA', tag: 'FRA', cells: [{ valueRaw: '10', missing: null, partial: false }, { valueRaw: '0', missing: null, partial: false }] },
    ],
  };

  const options = { shown: columns, candidates: ['GER', 'FRA', 'ITA', 'ENG'], top: 4, rankMarkers: false };
  const specs = scoreboardColumnsSpec(columns, 4);

  for (const direction of ['descending', 'ascending'] as const) {
    const settings: TableSettings = { ...tableSettingsOf(undefined, specs, SCOREBOARD_BASE), order: specs.map(s => s.key), sort: { column: 'factories', direction } };
    const followedModel = scoreboardProjection.project(createBoundedReadView({
      hour: 40,
      answers: new Map([[RANK_SOURCE, rankAns], ['factories', facAns], ['casualties', casAns], ['casualties:partial', partAns]]),
      identity,
    }), options);
    const exactModel = scoreboardProjection.project(createBoundedReadView({
      hour: 40,
      panels: new Map([['main', exactReply]]),
      identity,
    }), options);

    const followedFlat = resolveTableRows(followedModel, settings);
    const exactFlat = resolveTableRows(exactModel, settings);
    const sbLayouts = buildColumnLayouts(specs, settings);
    const followedAudit = deriveTableAudit({ model: followedModel, flatRows: followedFlat, visibleColumns: sbLayouts });
    const exactAudit = deriveTableAudit({ model: exactModel, flatRows: exactFlat, visibleColumns: sbLayouts });

    assert.equal(pictureError('table', followedAudit, { values: new Map(exactAudit.map(r => [r.key, r.value!])), ordered: true }), null);
    assert.equal(followedFlat.at(-1)?.kind === 'data' && followedFlat.at(-1)?.key, 'ENG', `missing ENG sorts last in ${direction}`);
    const order = followedFlat.map(r => r.key);
    assert.ok(order.indexOf('FRA') < order.indexOf('ITA'), 'tied France precedes Italy by name A-Z regardless of wire vs reply input order');
    assert.ok(followedAudit.find(r => r.key === 'FRA:casualties')?.value?.includes(canonicalAuditValue(zeroValue(0))), 'known zero renders zero state');
    assert.ok(followedAudit.find(r => r.key === 'ITA:casualties')?.value?.includes(canonicalAuditValue(partialValue(500n, 'Some inputs were not recorded at this hour'))));
  }

  const hourly = { shown: columns, candidates: ['GER', 'FRA', 'ITA', 'ENG'], top: null, rankMarkers: false };
  const at = (hour: number, gerFactories: bigint) => scoreboardProjection.project(createBoundedReadView({
    hour,
    identity,
    answers: new Map([
      ['factories', answer({ keys: [1n, 2n, 3n, 4n], columns: 1, names: ['ITA', 'FRA', 'GER', 'ENG'], values: [10n, 10n, gerFactories, 0n], states: [CELL.value, CELL.value, CELL.value, CELL.missing], reasons: [null, null, null, 'Factories not recorded'] })],
      ['casualties', answer({ keys: [1n, 2n, 3n, 4n], columns: 1, names: ['ITA', 'FRA', 'GER', 'ENG'], values: [500n, 0n, 200n, 0n], states: [CELL.value, CELL.value, CELL.value, CELL.value] })],
      ['casualties:partial', answer({ keys: [1n, 2n, 3n, 4n], columns: 1, names: ['ITA', 'FRA', 'GER', 'ENG'], values: [1n, 0n, 0n, 0n], states: [CELL.value, CELL.value, CELL.value, CELL.value] })],
    ]),
  }), hourly);
  const index = new IdentityIndex({ sourceId: 's', revision: '1', branchId: '1', content: 1 });
  const style = (name: string, valid: [number, number]) => ({ tag: 'GER', name, color: [102, 102, 102] as [number, number, number], valid, flagChoice: { keys: ['GER', null, null] as [string, null, null], reasons: [null, null, null] as [null, null, null] } });
  index.ingestStylesReply({ branchId: '1', hour: 0, styles: [style('Germany', [0, 49])], warnings: [] });
  index.ingestStylesReply({ branchId: '1', hour: 50, styles: [style('German Reich', [50, 100])], warnings: [] });
  const renamed = createSnapshotIdentityProvider(index);
  const factoriesOnly = new Map([['factories', answer({ keys: [1n], columns: 1, names: ['GER'], values: [1n], states: [CELL.value] })]]);
  assert.equal(scoreboardProjection.project(createBoundedReadView({ hour: 40, identity: renamed, answers: factoriesOnly }), hourly).rows.find(row => row.key === 'GER')?.name, 'Germany');
  assert.equal(scoreboardProjection.project(createBoundedReadView({ hour: 50, identity: renamed, answers: factoriesOnly }), hourly).rows.find(row => row.key === 'GER')?.name, 'German Reich', 'a dated rename reaches the country cell through identity');

  const first = at(40, 0n);
  const quiet = at(41, 0n);
  assert.deepEqual(quiet.rows.map((row, index) => row === first.rows[index]), [true, true, true, true], 'a quiet hour\'s fresh answers rebuild no row');
  const moved = at(42, 7n);
  assert.deepEqual(moved.rows.map(row => row === quiet.rows.find(held => held.key === row.key)), moved.rows.map(row => row.key !== 'GER'), 'only the row whose answer changed is rebuilt');
});

test('apm-table projection matches followed wire answers and exact replies, including partial gaps and notApplicable zero minutes', () => {
  const wire = headTextAnswer([
    { key: 1n, id: 'GER', values: [floatBits(45.2), floatBits(45.2), 113n, 15n, floatBits(2.5), 1n, 1n, 'GER', 100n, 'GER', null, '113', '5', '2'] },
    { key: 2n, id: 'ITA', values: [null, null, 0n, 0n, floatBits(0), 1n, 0n, 'ITA', 100n, 'ITA', null, '0', '0', '1'] },
  ]);
  const reply: ApmReply = {
    available: true,
    warnings: [],
    players: [],
    series: [
      { id: 'GER', name: 'GER', nameMissing: null, tag: 'GER', hour: 100, summary: { apm: 45.2, confirmedApm: 45.2, orders: '113', confirmedOrders: '113', pausedOrders: '15', unpausedMinutes: 2.5, unpausedMinutesNum: '5', unpausedMinutesDen: '2', complete: true, gaps: true }, points: [] },
      { id: 'ITA', name: 'ITA', nameMissing: null, tag: 'ITA', hour: 100, summary: { apm: null, confirmedApm: null, orders: '0', confirmedOrders: '0', pausedOrders: '0', unpausedMinutes: 0, unpausedMinutesNum: '0', unpausedMinutesDen: '1', complete: true, gaps: false }, points: [] },
    ],
  };
  const opts = {
    players: false,
    showPaused: true,
   
    rankMarkers: false,
  };
  const cols = apmTableColumns(false, true);
  const settings = tableSettingsOf(undefined, cols, { density: 'comfortable', sort: { column: 'apm', direction: 'descending' } });
  const apmLayouts = buildColumnLayouts(cols, settings);
  const fModel = apmTableProjection.project(createBoundedReadView({ hour: 100, answers: new Map([['main', wire]]) }), opts);
  const eModel = apmTableProjection.project(createBoundedReadView({ hour: 100, panels: new Map([['main', reply]]) }), opts);
  const fAudit = deriveTableAudit({ model: fModel, flatRows: resolveTableRows(fModel, settings), visibleColumns: apmLayouts });
  const eAudit = deriveTableAudit({ model: eModel, flatRows: resolveTableRows(eModel, settings), visibleColumns: apmLayouts });
  assert.equal(pictureError('table', fAudit, { values: new Map(eAudit.map(r => [r.key, r.value!])), ordered: true }), null);
  assert.ok(fAudit.find(r => r.key === 'GER:apm')?.value?.includes('["partial",["str","45.2"],'));
  assert.ok(fAudit.find(r => r.key === 'ITA:apm')?.value?.includes('["notApplicable",'));
});

test('occupied-states, game-score (with faction groups), and division-template (with obsolete tree) project deterministically', () => {
  const occReply = {
    states: [
      { state: 1, name: 'Warsaw', nameMissing: null, owner: 'POL', occupier: 'GER', occupied: 'POL', garrisonRequiredSign: 1, garrisonAlert: false, assignedKnown: true, garrisonMissing: null, law: 'civilian', lawMissing: null },
    ],
    complete: true,
    missing: null,
    laws: [{ token: 'civilian', name: 'Civilian Oversight', icon: null, missing: null }],
    strengthMissing: 'Missing strength',
    garrisonRedRaw: '30000',
    warnings: [],
  };
  const occModel = occupiedStatesProjection.project(createBoundedReadView({
    hour: 50,
    identity: datedIdentity({ GER: [{ from: 0, name: 'Germany' }], POL: [{ from: 0, name: 'Poland' }] }),
    panels: new Map([['main', occReply]]),
    live: new Map([['main', { get: k => (k.endsWith('resistance') ? 2_500_000 : k.endsWith('compliance') ? 1_000_000 : k.endsWith('garrison') ? 500_000 : 100_000), missing: () => 'Not recorded' }]]),
  }), {
    filter: { passing: null, unrecorded: [], bounds: [] },
    role: 'occupier',
    lawAs: 'dot',
    filterAll: true,
  });
  const occSettings = tableSettingsOf(undefined, OCCUPIED_DEFAULTS, OCCUPIED_BASE);
  const occRows = resolveTableRows(occModel, occSettings);
  assert.equal(occRows.length, 1);
  const occLayouts = buildColumnLayouts(occModel.columns, occSettings);
  assert.ok(deriveTableAudit({ model: occModel, flatRows: occRows, visibleColumns: occLayouts }).find(r => r.key === '1:resistance')?.value?.includes(canonicalAuditValue(knownValue(2_500_000))));

  const gsReply = {
    missing: null,
    warnings: [],
    hints: {},
    multipliers: [{ part: 'total', raw: '100000', missing: null }],
    rows: [
      { tag: 'GER', total: { missing: null }, army: { missing: null }, navy: { missing: null }, air: { missing: null }, industry: { missing: null }, land: { missing: null } },
      { tag: 'ENG', total: { missing: null }, army: { missing: null }, navy: { missing: null }, air: { missing: null }, industry: { missing: null }, land: { missing: null } },
    ],
  };
  const gsModel = gameScoreProjection.project(createBoundedReadView({
    hour: 50,
    identity: datedIdentity({ GER: [{ from: 0, name: 'Germany' }], ENG: [{ from: 0, name: 'United Kingdom' }] }),
    panels: new Map([['main', gsReply]]),
    live: new Map([['main', { get: k => (k.startsWith('GER') ? 300 : 200), missing: () => 'Not recorded' }]]),
  }), {
    all: true,
    holders: null,
    membership: null,
    top: null,
    by: 'total',
    factions: true,
    factionRuns: [
      { id: '1', tag: 'GER', firstHour: 0, lastHour: 100, names: [{ firstHour: 0, lastHour: 100, name: 'Axis' }] },
      { id: '2', tag: 'ENG', firstHour: 0, lastHour: 100, names: [{ firstHour: 0, lastHour: 100, name: 'Allies' }] },
    ],
    rankMarkers: false,
   
  });
  const gsCols = gameScoreColumns(undefined);
  const gsFlat = resolveTableRows(gsModel, tableSettingsOf(undefined, gsCols, { density: 'comfortable', sort: { column: 'total', direction: 'descending' } }));
  assert.deepEqual(gsFlat.filter(r => r.kind === 'group-header').map(r => r.label), ['Allies', 'Axis']);

  const gsIncOptions = { all: true, holders: null, membership: null, top: null, by: 'total' as const, factions: false, factionRuns: null, rankMarkers: false };
  const gsLive = new Map([['main', { get: (key: string) => (key.startsWith('GER') ? 300 : 200), missing: () => 'Not recorded' }]]);
  const gsAt = (hour: number, identity?: ReturnType<typeof datedIdentity>) => gameScoreProjection.project(createBoundedReadView({
    hour,
    panels: new Map([['main', gsReply]]),
    live: gsLive,
    ...(identity ? { identity } : {}),
  }), gsIncOptions);
  const waiting = gsAt(60, datedIdentity({ GER: [{ from: 0, name: 'Germany' }] }));
  const loadingEng = waiting.rows.find(row => row.key === 'ENG')!;
  assert.notEqual(loadingEng.name, 'ENG', 'a name still loading is never the tag as if it were the name');
  assert.deepEqual(loadingEng.cells.country!.value, { kind: 'pending', reason: 'Name is loading' }, 'a name still loading is a pending state, never notRecorded');
  const waitingFlat = resolveTableRows(waiting, tableSettingsOf(undefined, gameScoreColumns(undefined), { density: 'comfortable', sort: { column: 'total', direction: 'descending' } }));
  const waitingCol = buildColumnLayouts(gameScoreColumns(undefined), tableSettingsOf(undefined, gameScoreColumns(undefined), { density: 'comfortable', sort: { column: 'total', direction: 'descending' } })).find(col => col.key === 'country')!;
  const waitingPaint = resolveCellPaintModel(loadingEng.cells.country, waitingCol, new Map());
  assert.equal(waitingPaint.value.kind, 'pending', 'the painted cell knows the name is pending');
  assert.match(deriveTableMirror(waitingFlat, waiting.columns, false).rows.find(row => row.key === 'ENG')!.cells.find(cell => cell.column === 'country')!.spoken, /^Loading/, 'the mirror says loading, never not recorded');
  assert.deepEqual(waiting.rows.find(row => row.key === 'GER')!.cells.country!.value, knownValue('Germany'));
  const named = gsAt(61, datedIdentity({ GER: [{ from: 0, name: 'Germany' }], ENG: [{ from: 0, name: 'United Kingdom' }] }));
  assert.deepEqual(named.rows.map(row => row.name), ['Germany', 'United Kingdom'], 'names that arrive with no live change rebuild the rows');

  const renamed = gameScoreProjection.project(createBoundedReadView({
    hour: 50,
    panels: new Map([['main', gsReply]]),
    live: gsLive,
    identity: datedIdentity({ GER: [{ from: 0, name: 'Germany' }, { from: 70, name: 'German Reich' }], ENG: [{ from: 0, name: 'United Kingdom' }] }),
  }), gsIncOptions);
  assert.equal(renamed.nextChangeAt, 70, 'a declarative table names its dated name changes as its next change');

  const gsTopOptions = { all: false, holders: null, membership: null, top: 1, by: 'army' as const, factions: false, factionRuns: null, rankMarkers: false };
  const gsTopAt = (top: unknown, live: (key: string) => number) => gameScoreProjection.project(createBoundedReadView({
    hour: 50,
    panels: new Map([['main', { ...gsReply, ...(top === undefined ? {} : { top }) }]]),
    live: new Map([['main', { get: live, missing: () => 'Not recorded' }]]),
  }), gsTopOptions);
  const gsRanked = gsTopAt({ tags: ['ENG'], missing: null }, key => (key.startsWith('GER') ? 300 : 200));
  assert.deepEqual(gsRanked.rows.map(row => row.key), ['ENG'], 'the top is the product\u2019s, not a client sort of live values that favour GER');
  const gsRefused = gsTopAt({ tags: [], missing: 'The top 1 by army score needs every country\u2019s army score, and 1 of 3 were not recorded at this hour' }, () => 1);
  assert.deepEqual([gsRefused.rows.length, gsRefused.empty], [0, 'The top 1 by army score needs every country\u2019s army score, and 1 of 3 were not recorded at this hour'], 'an unproven top is refused with the product\u2019s reason');
  assert.equal(gsTopAt(undefined, () => 1).rows.length, 0, 'a top never answered is not ranked in the client');

  const dtReply: DivisionTemplatesReply = {
    tag: 'GER',
    countFor: 'owner',
    fieldedMissing: null,
    warnings: [],
    templates: [
      { template: 'inf', name: 'Infantry', nameMissing: null, icon: null, iconMissing: null, rosterOrdinal: 1, obsolete: false, obsoleteHour: null, fielded: 24, baseWidthRaw: '2000000', trainingDays: 90, priority: 1, priorityName: 'Default', priorityMissing: null, units: [{ key: 'infantry', name: 'Infantry', nameMissing: null, local: true, count: 9, support: false }], unitsMissing: null, locked: false, forceRecruiting: false, newEquipment: true, foreignEquipment: true, manpower: 9000, settingsMissing: null, designer: 'ordinary' },
      { template: 'old', name: 'Old Cav', nameMissing: null, icon: null, iconMissing: null, rosterOrdinal: 2, obsolete: true, obsoleteHour: 10, fielded: 0, baseWidthRaw: '1200000', trainingDays: 60, priority: 0, priorityName: 'Reserve', priorityMissing: null, units: [], unitsMissing: null, locked: false, forceRecruiting: false, newEquipment: true, foreignEquipment: true, manpower: 4000, settingsMissing: null, designer: 'ordinary' },
    ],
  };
  const dtCols = divisionTemplateColumns();
  const dtSettings = tableSettingsOf(undefined, dtCols, { density: 'comfortable', sort: { column: 'template', direction: 'ascending' } });
  const dtModel = divisionTemplateProjection.project(createBoundedReadView({ hour: 50, panels: new Map([['main', dtReply]]), live: new Map([['main', repliedCells(dtReply).read()]]) }), {
    countryName: 'Germany',
   
    canOpenDesigner: true,
  });
  assert.equal(resolveTableRows(dtModel, dtSettings, {}).length, 2, 'obsolete group collapsed by default');
  assert.equal(resolveTableRows(dtModel, dtSettings, { obsolete: true }).length, 3, 'expanding obsolete reveals child template');
});

test('dirty cell painter repaints only changed cells and bounds text measurements after warm-up', () => {
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true, widest: ['Country 999'] },
    { key: 'v1', label: 'Score', as: 'faint', tone: 'blue', offered: ['plain', 'faint'], priority: 1, numeric: true, format: 'integer', widest: ['999,999'] },
    { key: 'v2', label: 'Rate', as: 'plain', offered: ['plain'], priority: 2, numeric: true, format: 'decimal1', widest: ['999.9'] },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'comfortable', sort: { column: 'v1', direction: 'descending' } });
  const baseCols = buildColumnLayouts(columns, settings);
  const { ctx, counts, reset } = instrumentedContext();
  const fixedWidths = measureFixedColumnWidths(baseCols, settings, TEST_THEME, text => ctx.measureText(text).width);
  const layout = layoutFamilyTable(buildColumnLayouts(columns, settings, fixedWidths), 480, { rankShown: false, rankWidth: 0 });
  const picture = createTablePicture();

  const makeRows = (vals: [number, number][]) => resolveTableRows({
    columns,
    hour: 1,
    rows: vals.map(([a, b], idx) => ({
      key: `C${idx}`,
      name: `Country ${idx}`,
      cells: {
        name: { value: knownValue(`Country ${idx}`) },
        v1: { value: knownValue(a), format: 'integer' },
        v2: { value: idx === 2 ? notRecorded('No data') : idx === 3 ? notApplicable('N/A') : partialValue(b, 'Gaps'), format: 'decimal1' },
      },
    })),
  }, settings);

  const r1 = makeRows([[100, 1.5], [90, 2.5], [80, 0], [70, 0], [60, 4.5]]);
  const firstDraw = drawFamilyTable(ctx, { layout, rows: r1, settings, hour: 1, width: 480, height: 220, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  assert.equal(firstDraw.paintedCells, 15, 'initial paint draws all 5x3 visible cells');

  reset();
  const sameDraw = drawFamilyTable(ctx, { layout, rows: r1, settings, hour: 2, width: 480, height: 220, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 2, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  assert.equal(sameDraw.paintedCells, 0, 'unchanged frame paints zero cells');
  assert.equal(sameDraw.drew, false);
  assert.equal(counts().measureCalls, 0);

  const r2 = makeRows([[100, 1.5], [90, 2.5], [80, 0], [70, 0], [60, 9.9]]);
  reset();
  const oneCellDraw = drawFamilyTable(ctx, { layout, rows: r2, settings, hour: 3, width: 480, height: 220, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 3, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  assert.equal(oneCellDraw.paintedCells, 1, 'changing one cell without reordering repaints exactly 1 cell');

  const r3 = makeRows([[100, 1.5], [95, 2.5], [80, 0], [70, 0], [99, 9.9]]);
  reset();
  const swapDraw = drawFamilyTable(ctx, { layout, rows: r3, settings, hour: 4, width: 480, height: 220, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 4, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  assert.ok(swapDraw.paintedCells > 0 && swapDraw.paintedCells <= 15, `reorder repaints only dirty rows: ${swapDraw.paintedCells}`);

  reset();
  for (let frame = 0; frame < 60; frame++) {
    const stepRows = makeRows([[100, 1.5], [90, 2.5], [80, 0], [70, 0], [60, (frame % 5) + 0.5]]);
    drawFamilyTable(ctx, { layout, rows: stepRows, settings, hour: 10 + frame, width: 480, height: 220, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 10 + frame, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  }
  assert.ok(counts().measureCalls <= 15, `text measurements stay bounded after warm-up: ${counts().measureCalls} across 60 frames`);
});

test('1,500 countries project, sort, audit identically, and paint only visible rows with bounded measurements', () => {
  const count = 1500;
  const tags = Array.from({ length: count }, (_, i) => `C${String(i).padStart(4, '0')}`);
  const identity = datedIdentity(Object.fromEntries(tags.map(tag => [tag, [{ from: 0, name: `Country ${tag}` }]])));
  const columns: ScoreboardColumn[] = [
    { key: 'factories', label: 'Factories', unit: 'factories', decimals: 0, available: true, reason: null, family: { kind: 'f', measure: 'm' }, partial: null, noneAtZero: false },
  ];
  const values = tags.map((_, i) => BigInt((i * 7919 + 17) % 10000));
  const wire = answer({
    keys: tags.map((_, i) => BigInt(i + 1)),
    columns: 1,
    names: tags,
    values,
    states: tags.map((_, i) => (i % 50 === 0 ? CELL.unrecorded : CELL.value)),
  });
  const reply: ScoreboardReply = {
    branchId: '1',
    hour: 200,
    missing: null,
    warnings: [],
    rows: tags.map((tag, i) => ({
      country: tag,
      tag,
      cells: [{ valueRaw: i % 50 === 0 ? null : String(values[i]!), missing: i % 50 === 0 ? 'Not recorded at this hour' : null, partial: false }],
    })),
  };

  const options = { shown: columns, candidates: tags, top: null, rankMarkers: false };
  const specs = scoreboardColumnsSpec(columns, null);
  const settings: TableSettings = { ...tableSettingsOf(undefined, specs, SCOREBOARD_BASE), order: specs.map(s => s.key), sort: { column: 'factories', direction: 'descending' } };
  const followedModel = scoreboardProjection.project(createBoundedReadView({ hour: 200, answers: new Map([['factories', wire]]), identity }), options);
  const exactModel = scoreboardProjection.project(createBoundedReadView({ hour: 200, panels: new Map([['main', reply]]), identity }), options);
  const followedRows = resolveTableRows(followedModel, settings);
  const exactRows = resolveTableRows(exactModel, settings);
  assert.equal(followedRows.length, count);

  const sb1500Layouts = buildColumnLayouts(specs, settings);
  const followedAudit = deriveTableAudit({ model: followedModel, flatRows: followedRows, visibleColumns: sb1500Layouts, window: { first: 0, count: 30 } });
  const exactAudit = deriveTableAudit({ model: exactModel, flatRows: exactRows, visibleColumns: sb1500Layouts, window: { first: 0, count: 30 } });
  assert.equal(pictureError('table', followedAudit, { values: new Map(exactAudit.map(r => [r.key, r.value!])), ordered: true }), null);

  const { ctx, counts, reset } = instrumentedContext();
  const baseCols = buildColumnLayouts(specs, settings);
  const fixedWidths = measureFixedColumnWidths(baseCols, settings, TEST_THEME, text => ctx.measureText(text).width);
  const layout = layoutFamilyTable(buildColumnLayouts(specs, settings, fixedWidths), 480, { rankShown: true, rankWidth: 36 });
  const picture = createTablePicture();
  reset();
  const draw = drawFamilyTable(ctx, { layout, rows: followedRows, settings, hour: 200, width: 480, height: 320, scrollLeft: 0, scrollTop: 0, rankShown: true, rankWidth: 36, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, picture);
  assert.ok(draw.paintedCells <= 30, `only visible viewport rows painted out of 1,500: ${draw.paintedCells} cells`);
  assert.ok(counts().measureCalls <= 50, `bounded text measurements for 1,500-country table: ${counts().measureCalls}`);
});

test('occupied-states projection names countries at the hour, filter ranges, unrecorded membership, and live tooltip details', () => {
  const mkState = (id: number, occupier: string | null, occupied: string | null, extra: Record<string, unknown> = {}) => ({
    state: id, name: `State ${id}`, nameMissing: null, owner: occupied, occupier, occupied, garrisonRequiredSign: 1 as const,
    garrisonAlert: false, assignedKnown: true, garrisonMissing: null, law: 'mod_law', lawMissing: null, ...extra,
  });
  const reply = {
    states: [mkState(1, 'GER', 'POL'), mkState(2, 'SOV', 'POL'), mkState(3, null, 'POL')],
    complete: true, missing: null,
    laws: [{ token: 'mod_law', name: 'Moderate', icon: null, missing: null }],
    strengthMissing: 'Strength needs inputs', garrisonRedRaw: '30000', warnings: [],
  };
  const filter = { passing: { GER: [[10, 20]] as const }, unrecorded: [[30, 40, 'Membership not recorded']] as const, bounds: [10, 21, 30, 41] };
  const live = valuesOf(new Map([
    [liveKey('1', 'resistance'), 2_500_000],
    [liveKey('1', 'compliance'), 0],
    [liveKey('1', 'requiredManpower'), 1200],
    [liveKey('1', 'garrison'), 250_000],
    [liveKey('1', 'garrisonStrength'), 87_500],
  ]), NOT_NAMED);
  const index = new IdentityIndex({ sourceId: 's', revision: '1', branchId: '1', content: 1 });
  const style = (tag: string, name: string, valid: [number, number]) => ({ tag, name, color: [102, 102, 102] as [number, number, number], valid, flagChoice: { keys: [tag, null, null] as [string, null, null], reasons: [null, null, null] as [null, null, null] } });
  index.ingestStylesReply({ branchId: '1', hour: 0, styles: [style('GER', 'Germany', [0, 19]), style('POL', 'Poland', [0, 100])], warnings: [] });
  index.ingestStylesReply({ branchId: '1', hour: 20, styles: [style('GER', 'German Reich', [20, 100])], warnings: [] });
  const identity = createSnapshotIdentityProvider(index);
  const run = (hour: number) => occupiedStatesProjection.project(
    createBoundedReadView({ hour, identity, panels: new Map([['main', reply]]), live: new Map([['main', live]]) }),
    { filter, role: 'occupier', lawAs: 'dot', filterAll: false },
  );
  const at15 = run(15);
  assert.deepEqual(at15.rows.map(r => r.key), ['1']);
  assert.equal(at15.nextChangeAt, 20, 'the occupier is renamed at 20, so the model holds until then');
  const settings = tableSettingsOf(undefined, OCCUPIED_DEFAULTS, OCCUPIED_BASE);
  const flat15 = resolveTableRows(at15, settings);
  const tip = deriveTableTooltip(flat15, '1', at15.columns);
  assert.ok(tip);
  assert.equal(tip.rows.find(r => r.label === 'Resistance')?.valueState?.text, '25%');
  assert.equal(tip.rows.find(r => r.label === 'Assigned manpower')?.valueState?.text, '0', 'known assigned with no live cell reads 0');
  assert.equal(tip.rows.find(r => r.label === 'Garrison strength')?.valueState?.text, '87.5%');
  assert.equal(tip.rows.find(r => r.label === 'Occupied country')?.valueState?.text, 'Poland');
  const waitingNames = (names: Parameters<typeof datedIdentity>[0]) => occupiedStatesProjection.project(
    createBoundedReadView({ hour: 15, identity: datedIdentity(names), panels: new Map([['main', reply]]), live: new Map([['main', live]]) }),
    { filter, role: 'occupier', lawAs: 'dot', filterAll: false },
  );
  const beforeNames = waitingNames({ GER: [{ from: 0, name: 'Germany' }] });
  const beforeTip = deriveTableTooltip(resolveTableRows(beforeNames, settings), '1', beforeNames.columns)!;
  assert.deepEqual(beforeTip.rows.find(r => r.label === 'Occupied country')?.valueState, { kind: 'pending', reason: 'Name is loading' }, 'the occupied country still loading says pending, never missing or the tag');
  const afterNames = waitingNames({ GER: [{ from: 0, name: 'Germany' }], POL: [{ from: 0, name: 'Poland' }] });
  const afterTip = deriveTableTooltip(resolveTableRows(afterNames, settings), '1', afterNames.columns)!;
  assert.equal(afterTip.rows.find(r => r.label === 'Occupied country')?.valueState?.text, 'Poland', 'once names arrive the tooltip names the occupied country');

  assert.equal(run(25).empty, 'Occupied states unavailable for these countries.');
  assert.equal(run(35).empty, 'Membership not recorded');
  assert.equal(run(20).rows[0]!.cells.occupier!.value.kind, 'known');
  assert.equal((run(20).rows[0]!.cells.occupier!.value as { value: string }).value, 'German Reich');
});

test('layout hides lowest-priority columns into tooltip or keeps all columns in scroll mode, and bigints sort beyond 2^53', () => {
  const specs: TableColumnSpec[] = [
    { key: 'state', label: 'State', as: 'plain', offered: ['plain'], priority: 0, text: true, widest: ['State Name'] },
    { key: 'resistance', label: 'Resistance', as: 'meterAfter', offered: ['meterAfter'], priority: 1, numeric: true },
    { key: 'owner', label: 'Owner', as: 'plain', offered: ['plain'], priority: 6, text: true },
  ];
  const settings = tableSettingsOf(undefined, specs, { density: 'comfortable', sort: { column: 'resistance', direction: 'descending' } });
  const cols = buildColumnLayouts(specs, settings, { state: 140, resistance: 130, owner: 140 });
  const narrowLayout = layoutFamilyTable(cols, 370);
  assert.deepEqual(narrowLayout.columns.map(c => c.key), ['state', 'resistance', 'owner'], 'a narrow table keeps every column and scrolls');
  assert.equal(narrowLayout.scrollable, true);
  assert.deepEqual(narrowLayout.columns.map(c => [c.leadInset ?? 0, c.trailInset ?? 0]), [[6, 0], [0, 0], [0, 6]], 'the 6 px side margin sits on the first and last columns');
  const scrollLayout = layoutFamilyTable(cols, 250);
  assert.equal(scrollLayout.scrollable, true);
  assert.equal(scrollLayout.pinnedWidth, 146);
  const narrowing = buildColumnLayouts(specs.map(spec => spec.key === 'owner' ? { ...spec, narrow: 'hide' as const } : spec), settings, { state: 140, resistance: 130, owner: 140 });
  assert.equal(layoutFamilyTable(narrowing, 500).columns.some(c => c.narrowed), false, 'a table that fits keeps every cell\'s full text');
  assert.deepEqual(layoutFamilyTable(narrowing, 370).columns.map(c => [c.key, c.narrowed]), [['state', true], ['resistance', true]], 'a narrowed table lets cells draw their narrow text');
  const primary = buildColumnLayouts(specs.map(spec => spec.key === 'owner' ? { ...spec, narrow: 'hide' as const }
    : spec.key === 'state' ? { ...spec, narrow: 'primary' as const, minWidth: 60 } : spec), settings, { state: 140, resistance: 130, owner: 140 });
  const squeezed = layoutFamilyTable(primary, 220);
  assert.equal(squeezed.scrollable, false, 'the primary column gives way to its declared minimum before the table scrolls');
  assert.deepEqual(squeezed.columns.map(c => [c.key, c.width]), [['state', 71], ['resistance', 149]]);

  const bigModel = {
    columns: specs.slice(0, 2),
    hour: 1,
    rows: [
      { key: 'A', name: 'A', cells: { state: { value: knownValue('A') }, resistance: { value: knownValue((1n << 60n) + 1n) } } },
      { key: 'B', name: 'B', cells: { state: { value: knownValue('B') }, resistance: { value: knownValue((1n << 60n) + 2n) } } },
    ],
  };
  assert.deepEqual(resolveTableRows(bigModel, settings).map(r => r.key), ['B', 'A']);
});

test('lane view skips projection on next(), caches unchanged sources across serves, throttles mirror to 1 Hz while playing, and omits audit unless requested', () => {
  let projectCalls = 0;
  const proj = declareTableProjection<{ rows: { id: string; v: number }[] }, { id: string; v: number }, Record<string, never>>({
    kind: 'test-lane-cache',
    columns: [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, path: 'id' },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true, format: 'integer', path: 'v' },
    ],
    rows: reply => {
      projectCalls++;
      return { rows: reply.rows };
    },
    rowKey: row => row.id,
    rowName: row => row.id,
  });
  registerFamilyProjection(proj);
  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  try {
    const view = createLaneView(tableLane, () => {});
    const settings = tableSettingsOf(undefined, [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'] },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'] },
    ], { density: 'comfortable', sort: { column: 'v', direction: 'descending' } });
    const staticPanels = { main: { rows: [{ id: 'GER', v: 10 }] } };
    const options = {};
    const model = {
      family: 'table' as const, theme: TEST_THEME,
      kind: 'test-lane-cache',
      options,
      settings,
      sources: {},
      staticPanels,
      width: 320,
      height: 200,
      scrollLeft: 0,
      scrollTop: 0,
      window: { first: 0, count: 10 },
    };
    const scene = { widget: 1, subs: [], pictures: [], model };
    assert.equal(view.next?.(scene, 10), null, 'with nothing held, next() proves no boundary and the lane paints at its target');
    assert.equal(projectCalls, 0, 'next() must never invoke projection.project()');

    const state = new Map<string, unknown>();
    let seq = 1;
    const content = () => `c${seq++}`;
    const transport = new LaneTransport();
    const plain = transport.carry(model);
    const audited = transport.carry({ ...model, audit: true });
    const mkInput = (now: number, speed: number, audit = false) => ({
      ...scene,
      model: audit ? audited : plain,
      hour: 10,
      seek: 0,
      speed,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now,
    });
    const first = view.paint(mkInput(1000, 1440), state, content);
    assert.equal(projectCalls, 1);
    assert.equal(first.fields.audit, undefined, 'audit omitted when bound.audit is false');
    const firstMirrorContent = first.fields.mirror!.content;

    const second = view.paint(mkInput(1100, 1440), state, content);
    assert.equal(projectCalls, 1, 'same sources/options/hour reuses cached projection');
    assert.equal(second.fields.mirror!.content, firstMirrorContent, 'mirror stays cached under 1 Hz while playing');

    const withAudit = view.paint(mkInput(1200, 1440, true), state, content);
    const auditVal = withAudit.fields.audit?.value as { id: number; rows: unknown[] } | undefined;
    assert.ok(auditVal && Array.isArray(auditVal.rows), 'audit built only when requested');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('dirty mirror and tooltip flush on cadence or pause, focus and hover stay independent, animated hit testing uses painted Y, and meta/audit/headSpan hold invariants', () => {
  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  let wakes = 0;
  try {
    const view = createLaneView(tableLane, () => { wakes++; });
    const transport = new LaneTransport();
    const settings = tableSettingsOf(undefined, [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'] },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'] },
    ], { density: 'comfortable', sort: { column: 'v', direction: 'descending' } });
    const makeModel = (rows: { id: string; v: number }[], focusedKey: string | null = null, extra: Record<string, unknown> = {}) => ({
      family: 'table' as const, theme: TEST_THEME,
      kind: 'test-lane-cache',
      options: NO_OPTIONS,
      settings,
      sources: NO_SOURCES,
      staticPanels: { main: { rows } },
      width: 320,
      height: 200,
      scrollLeft: 0,
      scrollTop: 0,
      focusedKey,
      window: { first: 0, count: 10 },
      ...extra,
    });
    const state = new Map<string, unknown>();
    let seq = 1;
    const content = () => `c${seq++}`;
    const paintAt = (rows: { id: string; v: number }[], now: number, speed: number, inspect: string | null = null, focusedKey: string | null = null, extra: Record<string, unknown> = {}) =>
      view.paint({
        widget: 1,
        subs: [],
        pictures: [],
        model: transport.carry(makeModel(rows, focusedKey, extra)),
        hour: 10,
        seek: 0,
        speed,
        binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
        inspect,
        now,
      }, state, content);

    const rowsA = [{ id: 'GER', v: 20 }, { id: 'FRA', v: 10 }];
    const rowsB = [{ id: 'GER', v: 5 }, { id: 'FRA', v: 30 }];
    const p1 = paintAt(rowsA, 1000, 1440, 'row:FRA', null);
    assert.deepEqual((p1.fields.mirror!.value as { rows: { key: string }[] }).rows.map(r => r.key), ['GER', 'FRA']);
    assert.equal((p1.fields.tooltip!.value as { text: { rowKey: string } }).text.rowKey, 'FRA', 'hover tooltip tracks inspected FRA');

    const p2 = paintAt(rowsB, 1100, 1440, 'row:FRA', null);
    assert.equal(p2.fields.mirror, undefined, 'unfocused playback withholds changed mirror rows, never the old rows beside the new picture');

    const p3 = paintAt(rowsB, 2100, 1440, 'row:FRA', 'GER');
    assert.deepEqual((p3.fields.mirror!.value as { rows: { key: string }[] }).rows.map(r => r.key), ['FRA', 'GER'], 'focusing syncs the mirror immediately during playback');
    assert.equal((p3.fields.focusTooltip?.value as { text: { rowKey: string } } | undefined)?.text.rowKey, 'GER', 'focusTooltip tracks focused GER independently of hover');

    paintAt(rowsA, 2200, 1440, 'row:FRA', null);
    const pPause = paintAt(rowsA, 2250, 0, 'row:FRA', null);
    assert.deepEqual((pPause.fields.mirror!.value as { rows: { key: string }[] }).rows.map(r => r.key), ['GER', 'FRA'], 'pause immediately flushes pending mirror revision');
    assert.ok(wakes >= 0);

    const audit1 = paintAt(rowsA, 2300, 0, null, null, { audit: true, auditId: 7 });
    const audit2 = paintAt(rowsA, 2310, 0, null, null, { audit: true, auditId: 8 });
    assert.equal((audit1.fields.audit!.value as { id: number }).id, 7, 'audit carries snapshot id 7');
    assert.equal((audit2.fields.audit!.value as { id: number }).id, 8, 'audit carries snapshot id 8 when snapshot changes');

    const cols: TableColumnSpec[] = [{ key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true }];
    const hitSettings = tableSettingsOf(undefined, cols, { density: 'comfortable', sort: { column: 'name', direction: 'ascending' } });
    const hitLayout = layoutFamilyTable(buildColumnLayouts(cols, hitSettings, { name: 160 }), 320);
    const hitRows = resolveTableRows({
      columns: cols,
      hour: 1,
      rows: [
        { key: 'A', name: 'Alpha', cells: { name: { value: knownValue('Alpha') } } },
        { key: 'B', name: 'Bravo', cells: { name: { value: knownValue('Bravo') } } },
      ],
    }, hitSettings);
    const glide = new Map([
      ['A', { from: 34, to: 0, started: 100 }],
      ['B', { from: -34, to: 0, started: 100 }],
    ]);
    const hitTop = hitFamilyTable(hitLayout, hitRows, { x: 20, y: 10 }, 34, 0, 0, false, 0, tableGlide(glide, hitRows, new Map([['A', 0], ['B', 1]]), null, 34), 100, 180);
    assert.equal(hitTop.rowKey, 'B', 'clicking y=10 during glide hits B which is painted at y=0, not destination A');

    const branch = { firstHour: 10, lastHour: 500 };
    assert.deepEqual(resolveHeadSpan('trailing-window', 168, 'campaign', branch, { firstHour: 10, lastHour: 500 }, 500), { first: 10, last: 500 }, 'campaign mode keeps whole branch span');
    assert.deepEqual(resolveHeadSpan('trailing-window', 168, 'range', branch, { firstHour: 50, lastHour: 120 }, 120), { first: 50, last: 120 }, 'range mode keeps explicit range');
    assert.deepEqual(resolveHeadSpan('trailing-window', 168, 'follow', branch, { firstHour: 10, lastHour: 500 }, 300), { first: 133, last: 300 }, 'follow mode uses trailing window');
    assert.deepEqual(resolveHeadSpan('branch-to-hour', 168, 'pin', branch, { firstHour: 10, lastHour: 240 }, 240), { first: 10, last: 240 }, 'branch-to-hour pin runs from branch start to pinned hour');
    assert.deepEqual(resolveHeadSpan('branch-to-hour', 168, 'range', branch, { firstHour: 40, lastHour: 240 }, 240), { first: 40, last: 240 }, 'branch-to-hour range keeps explicit range');

    const { ctx } = instrumentedContext();
    const numCols: TableColumnSpec[] = [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
      { key: 'v', label: 'Val', as: 'faint', tone: 'blue', offered: ['faint'], priority: 1, numeric: true, format: 'integer' },
    ];
    const numSettings = tableSettingsOf(undefined, numCols, { density: 'comfortable', sort: { column: 'v', direction: 'descending' } });
    const numLayout = layoutFamilyTable(buildColumnLayouts(numCols, numSettings, { name: 120, v: 120 }), 320);
    const numRows = resolveTableRows({
      columns: numCols,
      hour: 1,
      rows: [
        { key: 'A', name: 'A', cells: { name: { value: knownValue('A') }, v: { value: knownValue(100), format: 'integer' } } },
        { key: 'B', name: 'B', cells: { name: { value: knownValue('B') }, v: { value: knownValue(50), format: 'integer' } } },
      ],
    }, numSettings);
    const pic = createTablePicture();
    drawFamilyTable(ctx, { layout: numLayout, rows: numRows, settings: numSettings, hour: 1, width: 320, height: 200, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: null, selectedKey: null, focusedKey: null, dpr: 1 }, pic);
    const firstMaxima = pic.maxima;
    drawFamilyTable(ctx, { layout: numLayout, rows: numRows, settings: numSettings, hour: 1, width: 320, height: 200, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 2, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), inspectedKey: 'A', selectedKey: null, focusedKey: null, dpr: 1 }, pic);
    assert.equal(pic.maxima, firstMaxima, 'hover repaint reuses cached column maxima without scanning all rows');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('round 2 regressions: canonical audit, snapshot store, retained static span, cadence trailing flush, density invalidation, off-screen focus index, long-distance glide hit/draw, dated country colours, missing occupation law, and meta/priority invalidation', () => {
  const cols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'rate', label: 'Rate', as: 'faint', tone: 'country', offered: ['faint'], priority: 1, numeric: true, format: 'decimal1' },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'comfortable', sort: { column: 'rate', direction: 'descending' } });
  const mkModel = (rate: number, groupLabel: string) => ({
    columns: cols,
    hour: 10,
    rank: true,
    rows: [
      { key: 'A', name: 'A', group: { key: 'g1', label: groupLabel, order: 0 }, cells: { name: { value: knownValue('A') }, rate: { value: rate === 0 ? zeroValue(0) : knownValue(rate), format: 'decimal1' as const, country: 'GER' } } },
    ],
  });
  const colLayouts = buildColumnLayouts(cols, settings);
  const mZero = mkModel(0, 'Axis');
  const mTiny = mkModel(0.04, 'Axis');
  const mRenamed = mkModel(0.04, 'New Axis');
  const auditZero = deriveTableAudit({ model: mZero, flatRows: resolveTableRows(mZero, settings), visibleColumns: colLayouts, rankShown: true });
  const auditTiny = deriveTableAudit({ model: mTiny, flatRows: resolveTableRows(mTiny, settings), visibleColumns: colLayouts, rankShown: true });
  assert.notEqual(
    pictureError('table', auditZero, { values: new Map(auditTiny.map(r => [r.key, r.value!])), ordered: true }),
    null,
    'canonical audit distinguishes 0 from 0.04 despite identical display rounding',
  );
  const auditRenamedGroup = deriveTableAudit({ model: mRenamed, flatRows: resolveTableRows(mRenamed, settings), visibleColumns: colLayouts, rankShown: true });
  assert.notEqual(
    pictureError('table', auditTiny, { values: new Map(auditRenamedGroup.map(r => [r.key, r.value!])), ordered: true }),
    null,
    'canonical audit catches group header changes',
  );
  const auditOneCol = deriveTableAudit({ model: mTiny, flatRows: resolveTableRows(mTiny, settings), visibleColumns: [colLayouts[0]!], rankShown: true });
  assert.notEqual(
    pictureError('table', auditTiny, { values: new Map(auditOneCol.map(r => [r.key, r.value!])), ordered: true }),
    null,
    'canonical audit catches visible column set changes',
  );

  const store = new AuditSnapshotStore<{ tag: string }>(2);
  const id1 = store.create({ tag: 'first' });
  const id2 = store.create({ tag: 'second' });
  assert.equal(store.require(id1).tag, 'first');
  assert.equal(store.require(id2).tag, 'second');
  assert.throws(() => store.require(undefined), /Not auditable/);
  assert.throws(() => store.require(999), /Not auditable/);

  assert.deepEqual(effectiveCountryUnobserved({ kind: 'group', group: { type: 'humans' }, membership: 'ever', aggregate: false }, [{ firstHour: 10, lastHour: 20 }]), [[10, Number.MAX_SAFE_INTEGER]]);
  assert.deepEqual(effectiveCountryUnobserved({ kind: 'group', group: { type: 'humans' }, membership: 'hour', aggregate: false }, [{ firstHour: 10, lastHour: 20 }]), [[10, 20]]);

  const gate = new CadenceGate(20);
  assert.equal(gate.due(100, 24, 'w'), true);
  gate.record(100, 'w');
  gate.invalidate();
  assert.equal(gate.due(110, 24, 'w'), false);
  let scheduledMs = -1;
  let scheduledCb: (() => void) | null = null;
  let flushed = false;
  const flusher = new TrailingFlushScheduler([gate], {
    set: (fn, ms) => {
      scheduledCb = fn;
      scheduledMs = ms;
      return 1 as unknown as ReturnType<typeof setTimeout>;
    },
    clear: () => { scheduledCb = null; },
  });
  flusher.sync(110, 24, () => { flushed = true; });
  assert.equal(scheduledMs, 10, 'schedules trailing flush for remaining interval');
  assert.ok(scheduledCb !== null);
  (scheduledCb as () => void)();
  assert.equal(flushed, true, 'trailing flush callback fires cleanly');
  assert.equal(gate.due(115, 0, 'w'), true, 'pause flushes immediately');
  flusher.cancel();

  const occMissingLaw = occupiedStatesProjection.project(createBoundedReadView({
    hour: 50,
    panels: new Map([['main', {
      states: [{ state: 1, name: 'Warsaw', nameMissing: null, owner: 'POL', occupier: 'GER', occupied: 'POL', garrisonRequiredSign: 0, garrisonAlert: false, assignedKnown: true, garrisonMissing: null, law: 'custom_law', lawMissing: null }],
      complete: true,
      missing: null,
      laws: [{ token: 'custom_law', name: null, icon: null, missing: 'Law localization absent' }],
      strengthMissing: 'Missing strength',
      garrisonRedRaw: null,
      warnings: [],
    }]]),
    live: new Map([['main', { get: () => 0, missing: () => 'Not recorded' }]]),
  }), { filter: { passing: null, unrecorded: [], bounds: [] }, role: 'occupier', lawAs: 'dot', filterAll: true });
  assert.deepEqual(occMissingLaw.rows[0]!.cells.law!.value, { kind: 'notRecorded', reason: 'Law localization absent' });

  const manyRows = Array.from({ length: 15 }, (_, i) => ({
    key: `R${i}`,
    name: `Row ${i}`,
    cells: { name: { value: knownValue(`Row ${i}`) }, rate: { value: knownValue(15 - i), format: 'decimal1' as const, country: 'GER' } },
  }));
  const flatMany = resolveTableRows({ columns: cols, hour: 10, rows: manyRows }, settings);
  const mirror = deriveTableMirror(flatMany, cols, false, { first: 5, count: 3 }, 'R0');
  assert.equal(mirror.rows[0]!.key, 'R0');
  assert.equal(mirror.rows[0]!.rowIndex, 0, 'off-screen focused row carries its true rowIndex, not window offset');

  const layout = layoutFamilyTable(buildColumnLayouts(cols, settings, { name: 140, rate: 140 }), 320);
  const longGlide = new Map([['R12', { from: -12 * 28, to: 0, started: 100 }]]);
  const hitGliding = hitFamilyTable(layout, flatMany, { x: 20, y: 10 }, 28, 0, 0, false, 0, tableGlide(longGlide, flatMany, new Map(flatMany.flatMap((item, index) => (item.kind === 'data' ? [[item.key, index] as const] : []))), null, 28), 100, 180);
  assert.equal(hitGliding.rowKey, 'R12', 'row gliding from index 12 into y=0 is hit-tested accurately');

  const { ctx } = instrumentedContext();
  const pic = createTablePicture();
  const fills: string[] = [];
  const origFillRect = ctx.fillRect.bind(ctx);
  const origFill = ctx.fill.bind(ctx);
  (ctx as unknown as { fillRect: (...a: unknown[]) => void; fill: (...a: unknown[]) => void }).fillRect = (...a: unknown[]) => {
    fills.push(String(ctx.fillStyle));
    (origFillRect as (...b: unknown[]) => void)(...a);
  };
  (ctx as unknown as { fill: (...a: unknown[]) => void }).fill = (...a: unknown[]) => {
    fills.push(String(ctx.fillStyle));
    (origFill as (...b: unknown[]) => void)(...a);
  };
  const tinted = (hour: number, colour: string) => resolveTableRows({ columns: cols, hour, rows: manyRows.slice(0, 2).map(row => ({ ...row, cells: { ...row.cells, rate: { ...row.cells.rate, countryColor: colour } } })) }, settings);
  drawFamilyTable(ctx, { layout, rows: tinted(10, 'rgb(10, 20, 30)'), settings, hour: 10, width: 320, height: 200, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 }, pic);
  assert.ok(fills.includes('rgb(10, 20, 30)'), 'paints the country colour its cell carries at hour 10');
  const compactSettings = { ...settings, density: 'compact' as const };
  const afterDensity = drawFamilyTable(ctx, { layout, rows: tinted(25, 'rgb(200, 10, 10)'), settings: compactSettings, hour: 25, width: 320, height: 200, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 2, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 }, pic);
  assert.equal(afterDensity.drew, true, 'density change invalidates frameKey and repaints');
  assert.ok(fills.includes('rgb(200, 10, 10)'), 'paints the colour a later projection gives the cell at hour 25');

  const metaProj = declareTableProjection<{ designer: string; priority: number }, { id: string; designer: string }, Record<string, never>>({
    kind: 'test-meta-priority-inv',
    columns: reply => [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, path: 'id' },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: reply.priority, numeric: true, value: () => knownValue(1) },
    ],
    rows: reply => ({ rows: [{ id: 'A', designer: reply.designer }] }),
    rowKey: r => r.id,
    rowName: r => r.id,
    rowMeta: r => ({ designer: r.designer }),
  });
  registerFamilyProjection(metaProj);
  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  try {
    const laneView = createLaneView(tableLane, () => {});
    const laneState = new Map<string, unknown>();
    const transport = new LaneTransport();
    let seq = 1;
    const mk = (designer: string, priority: number) => laneView.paint({
      widget: 1,
      subs: [],
      pictures: [],
      model: transport.carry({
        family: 'table', theme: TEST_THEME,
        kind: 'test-meta-priority-inv',
        options: NO_OPTIONS,
        settings: tableSettingsOf(undefined, [
          { key: 'name', label: 'Name', as: 'plain', offered: ['plain'] },
          { key: 'v', label: 'Val', as: 'plain', offered: ['plain'] },
        ], { density: 'comfortable', sort: { column: 'name', direction: 'ascending' } }),
        sources: NO_SOURCES,
        staticPanels: { main: { designer, priority } },
        width: 320,
        height: 200,
        scrollLeft: 0,
        scrollTop: 0,
      }),
      hour: 10,
      seek: 0, speed: 0,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: 100,
    }, laneState, () => `c${seq++}`);
    const firstPaint = mk('ordinary', 1);
    const secondPaint = mk('headquarters', 4);
    assert.equal(firstPaint.mode, 'picture', 'N4: tableLane returns explicit mode picture');
    assert.notEqual(firstPaint.fields.nav!.content, secondPaint.fields.nav!.content, 'row.meta change invalidates nav field');
    assert.notEqual(firstPaint.fields.meta!.content, secondPaint.fields.meta!.content, 'column priority change invalidates meta field');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('round 3 regressions: M1 typed scalars, N1 by-construction audit (bar/alert/flag/missing name/pill/tooltip), N6 native slot holder tie-break, M4 ever-gap refusal, S1 frozen static snapshot, M5 dateless game-score empty, and N18 frozen change caches', () => {
  assert.notEqual(canonicalAuditValue(knownValue('1000')), canonicalAuditValue(knownValue(1000)), 'M1: string "1000" and number 1000 do not collide');
  assert.equal(canonicalAuditValue(knownValue(1e21)), canonicalAuditValue(knownValue(10n ** 21n)), 'M1: 1e21 and 10n**21n serialize identically');
  assert.equal(canonicalAuditValue(knownValue(-0)), canonicalAuditValue(knownValue(0)));
  assert.notEqual(canonicalAuditData({ a: '1000' }), canonicalAuditData({ a: 1000 }));
  assert.equal(canonicalAuditData({ b: 1, a: 2 }), canonicalAuditData({ a: 2, b: 1 }), 'canonicalAuditData sorts object keys deterministically');

  const occCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true },
    { key: 'garrison', label: 'Garrison', as: 'meterBefore', tone: 'blue', offered: ['meterBefore'], priority: 1, numeric: true, format: 'integer' },
  ];
  const occSettings = tableSettingsOf(undefined, occCols, { density: 'comfortable', sort: { column: 'garrison', direction: 'descending' } });
  const occLayouts = buildColumnLayouts(occCols, occSettings);
  const mkOcc = (opts: { fraction: number; alert: boolean; flag: string; missingName: boolean; pill: string; extra: string }) => {
    const model = {
      columns: occCols,
      hour: 50,
      rank: true,
      titleSummary: opts.pill,
      rows: [{
        key: 'S1',
        name: opts.missingName ? 'Name not recorded' : 'Germany',
        cells: {
          country: {
            value: opts.missingName ? notRecorded('Name absent') : knownValue('Germany'),
            iconSlot: true,
            country: 'GER',
            art: readyArt(opts.flag),
            artKey: opts.flag,
          },
          garrison: {
            value: knownValue(12),
            format: 'integer' as const,
            bar: { fraction: knownValue(opts.fraction), alert: opts.alert },
          },
        },
        tooltipExtra: [{ label: 'Equipment', value: knownValue(opts.extra) }],
      }],
    };
    return deriveTableAudit({ model, flatRows: resolveTableRows(model, occSettings), visibleColumns: occLayouts, rankShown: true });
  };
  const baseAudit = mkOcc({ fraction: 0.8, alert: false, flag: 'ger.png', missingName: false, pill: '1 state', extra: '80%' });
  const diff = (other: ReturnType<typeof mkOcc>) => pictureError('table', baseAudit, { values: new Map(other.map(r => [r.key, r.value!])), ordered: true });
  assert.notEqual(diff(mkOcc({ fraction: 0.4, alert: false, flag: 'ger.png', missingName: false, pill: '1 state', extra: '80%' })), null, 'N1: catches garrison bar fraction change');
  assert.notEqual(diff(mkOcc({ fraction: 0.8, alert: true, flag: 'ger.png', missingName: false, pill: '1 state', extra: '80%' })), null, 'N1: catches garrison alert flag change');
  assert.notEqual(diff(mkOcc({ fraction: 0.8, alert: false, flag: 'pol.png', missingName: false, pill: '1 state', extra: '80%' })), null, 'N1: catches flag identity change');
  assert.notEqual(diff(mkOcc({ fraction: 0.8, alert: false, flag: 'ger.png', missingName: true, pill: '1 state', extra: '80%' })), null, 'N1: catches a missing name');
  assert.notEqual(diff(mkOcc({ fraction: 0.8, alert: false, flag: 'ger.png', missingName: false, pill: '2 states', extra: '80%' })), null, 'N1: catches titleSummary change');
  assert.notEqual(diff(mkOcc({ fraction: 0.8, alert: false, flag: 'ger.png', missingName: false, pill: '1 state', extra: '40%' })), null, 'N1: catches tooltipExtra change');

  const slotRuns = resolveSlotHolderRuns([
    { tag: 'Z', firstHour: 0, lastHour: 20, countryId: 5 },
    { tag: 'A', firstHour: 10, lastHour: 20, countryId: 5 },
  ]);
  assert.deepEqual(slotRuns, [{ tag: 'Z', firstHour: 0, lastHour: 20, countryId: 5 }], 'N6: equal countryId breaks tie by earlier firstHour before tag');
  assert.equal(holderRunAt([
    { tag: 'Z', firstHour: 0, lastHour: 20, countryId: 5 },
    { tag: 'A', firstHour: 10, lastHour: 20, countryId: 5 },
  ], 10)?.tag, 'Z', 'N6: unsorted holderRunAt uses identical (countryId, firstHour, tag) precedence');
  const nativeCase = resolveSlotHolderRuns([
    { tag: 'GER', firstHour: 1, lastHour: 10, countryId: 10 },
    { tag: 'PRU', firstHour: 5, lastHour: 15, countryId: 10 },
    { tag: 'D01', firstHour: 6, lastHour: 8, countryId: 9 },
  ]);
  assert.deepEqual(nativeCase.map(r => [r.tag, r.firstHour, r.lastHour]), [['GER', 1, 5], ['D01', 6, 8], ['GER', 9, 10], ['PRU', 11, 15]]);


  const ctxA = { tag: 'A', role: 'occupier', settings: 'sA', looks: 'lA' };
  const ctxB = { tag: 'B', role: 'occupied', settings: 'sB', looks: 'lB' };
  const bundleAt50 = { span: [10, 50] };
  const bundleAt60 = { span: [20, 60] };
  const first = showBundle(null, bundleAt50, false, 'plan', ctxA);
  const refreshed = showBundle(first.last, bundleAt50, false, 'plan', ctxB);
  assert.equal(refreshed.shown?.frozen, ctxB, 'F1: when every reply answers the current requests, option, role, settings and looks changes refresh immediately');
  const retained = showBundle(refreshed.last, bundleAt50, true, 'plan', ctxA);
  assert.equal(retained.shown, refreshed.shown, 'S1: while a request is superseded, the last complete bundle is shown with its own context, settings and looks');
  assert.equal(retained.shown?.bundle, bundleAt50);
  const updated = showBundle(retained.last, bundleAt60, false, 'plan', ctxA);
  assert.equal(updated.shown?.bundle, bundleAt60, 'S1: updates once the new replies complete a bundle');
  assert.equal(updated.shown?.frozen, ctxA, 'S1: the updated bundle carries the new selection context');

  const emptyGsA = gameScoreProjection.project(createBoundedReadView({
    hour: 10,
    panels: new Map([['main', { missing: null, warnings: [], hints: {}, multipliers: [], rows: [] }]]),
    live: new Map([['main', { get: () => undefined, missing: () => 'Not recorded' }]]),
  }), { all: true, membership: null, top: null, by: 'total', factions: false, factionRuns: null, rankMarkers: false });
  const emptyGsB = gameScoreProjection.project(createBoundedReadView({
    hour: 250,
    panels: new Map([['main', { missing: null, warnings: [], hints: {}, multipliers: [], rows: [] }]]),
    live: new Map([['main', { get: () => undefined, missing: () => 'Not recorded' }]]),
  }), { all: true, membership: null, top: null, by: 'total', factions: false, factionRuns: null, rankMarkers: false });
  assert.equal(emptyGsA.empty, 'No country’s scores recorded at this time.', 'M5: dateless empty message');
  assert.equal(emptyGsA.empty, emptyGsB.empty, 'M5: empty message does not depend on advancing hour');

  const winRanges: [number, number][] = [[10, 20], [50, 60]];
  assert.equal(nextWindowRangesChange(winRanges, 168, 5), 10);
  assert.equal(nextWindowRangesChange(winRanges, 168, 15), 50);
  assert.equal(nextWindowRangesChange(winRanges, 168, 55), 188);
  assert.equal(nextWindowRangesChange(winRanges, 168, 190), 228);
  assert.throws(() => { winRanges.push([100, 110]); }, TypeError, 'N18: indexed ranges are frozen against in-place mutation');
  const mutHolders: Record<string, { tag: string; firstHour: number; lastHour: number }[]> = { GER: [{ tag: 'GER', firstHour: 0, lastHour: 10 }] };
  assert.equal(nextHoldersChange(mutHolders, 5), 11);
  assert.throws(() => { mutHolders.GER![0]!.lastHour = 99; }, TypeError, 'N18: indexed holder runs are frozen against in-place mutation');
  const mutRanges: [number, number][] = [[1, 5]];
  assert.equal(nextRangesChange(mutRanges, 2), 6);
  assert.throws(() => { mutRanges[0]![1] = 20; }, TypeError, 'N18: indexed range tuples are frozen against in-place mutation');
});

test('round 4 regressions: P1/N1 country colour & dateless tooltip audit, P2 whole-panel refusal, P5 capture-gated audit serialization, N6 UTF-8 tag tie-break, N18 deep tuple freeze & indexed overlap, M4 filter ever-gap refusal, and M9 focused index', async () => {
  const cols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true },
    { key: 'score', label: 'Score', as: 'faint', tone: 'country', offered: ['faint'], priority: 1, numeric: true, format: 'integer' },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'comfortable', sort: { column: 'score', direction: 'descending' } });
  const layouts = buildColumnLayouts(cols, settings);
  const mkModel = (hour: number, colour = 'rgb(10, 20, 30)') => ({
    columns: cols,
    hour,
    rank: true,
    rows: [{
      key: 'GER',
      name: 'Germany',
      cells: {
        country: { value: knownValue('Germany'), iconSlot: true, country: 'GER' },
        score: { value: knownValue(100), format: 'integer' as const, country: 'GER', countryColor: colour },
      },
    }],
  });
  const m10 = mkModel(10);
  const m12 = mkModel(12);
  const flat10 = resolveTableRows(m10, settings);
  const flat12 = resolveTableRows(m12, settings);
  const audit10 = deriveTableAudit({ model: m10, flatRows: flat10, visibleColumns: layouts, rankShown: true });
  const audit12 = deriveTableAudit({ model: m12, flatRows: flat12, visibleColumns: layouts, rankShown: true });
  assert.equal(
    pictureError('table', audit10, { values: new Map(audit12.map(r => [r.key, r.value!])), ordered: true }),
    null,
    'P1: tooltip audit omits presentation hour so valid picture at hour 10 matches exact audit at hour 12',
  );
  const tip10 = deriveTableTooltip(flat10, 'GER', cols)!;
  assert.equal(tip10.subtitle, undefined, 'P1: deriveRowTooltip carries no hour-derived subtitle');
  const recoloured = mkModel(10, 'rgb(200, 10, 10)');
  const auditColorDiff = deriveTableAudit({ model: recoloured, flatRows: resolveTableRows(recoloured, settings), visibleColumns: layouts, rankShown: true });
  assert.notEqual(
    pictureError('table', audit10, { values: new Map(auditColorDiff.map(r => [r.key, r.value!])), ordered: true }),
    null,
    'N1/P1: resolved country colour change is caught by CellPaintModel audit',
  );
  const paintModel = resolveCellPaintModel(m10.rows[0]!.cells.score, layouts[1]!, new Map([['score', 100]]));
  assert.equal(paintModel.countryColor, 'rgb(10, 20, 30)', 'P1: CellPaintModel carries resolved countryColor for painter');

  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  try {
    const laneView = createLaneView(tableLane, () => {});
    const laneState = new Map<string, unknown>();
    const transport = new LaneTransport();
    let seq = 1;
    const refusedPaint = laneView.paint({
      widget: 1,
      subs: [{ id: 7, channel: 'panel' }],
      pictures: [{ parts: [{ value: { kind: 'panel', data: null, refused: 'Game score panel refused by recording' } }] }],
      model: transport.carry({
        family: 'table', theme: TEST_THEME,
        kind: 'game-score',
        view: 'table',
        options: { all: true, membership: null, top: null, by: 'total', factions: false, factionRuns: null, rankMarkers: false },
        settings: tableSettingsOf(undefined, gameScoreColumns(), { density: 'comfortable', sort: { column: 'total', direction: 'descending' } }),
        sources: { main: 7 },
        width: 320,
        height: 200,
        scrollLeft: 0,
        scrollTop: 0,
      }),
      hour: 10,
      seek: 0, speed: 0,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: 100,
    } as never, laneState, () => `r${seq++}`);
    assert.equal(refusedPaint.mode, 'refused', 'P2: whole-panel refusal produces mode refused, never empty mode picture');
    assert.equal(refusedPaint.reason, 'Game score panel refused by recording');

    const auditState = new Map<string, unknown>();
    const paintNoAudit = laneView.paint({
      widget: 2,
      subs: [],
      pictures: [],
      model: transport.carry({
        family: 'table', theme: TEST_THEME,
        kind: 'game-score',
        view: 'table',
        options: { all: true, membership: null, top: null, by: 'total', factions: false, factionRuns: null, rankMarkers: false },
        settings: tableSettingsOf(undefined, gameScoreColumns(), { density: 'comfortable', sort: { column: 'total', direction: 'descending' } }),
        sources: NO_SOURCES,
        staticPanels: { main: { missing: null, warnings: [], hints: {}, multipliers: [], rows: [{ tag: 'GER', total: { missing: null }, army: { missing: null }, navy: { missing: null }, air: { missing: null }, industry: { missing: null }, land: { missing: null } }] } },
        width: 320,
        height: 200,
        scrollLeft: 0,
        scrollTop: 0,
      }),
      hour: 10,
      seek: 0,
      speed: 24,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: 100,
    }, auditState, () => `a${seq++}`);
    assert.equal(paintNoAudit.fields.audit, undefined, 'P5: zero audit serialization when capture is inactive');
    assert.equal(paintNoAudit.mode, 'picture', 'P2: populated table returns explicit mode picture');

    const seen: boolean[] = [];
    const unsub = subscribeFamilyAuditing(() => { seen.push(isFamilyAuditing()); });
    try {
      assert.equal(isFamilyAuditing(), false);
      await capturePictures(true);
      assert.equal(isFamilyAuditing(), true);
      await capturePictures(false);
      assert.equal(isFamilyAuditing(), false);
      assert.deepEqual(seen, [true, false], 'P5: shared family auditing hook notifies on capture start and end');
    } finally {
      unsub();
      await capturePicturesBatch(100);
    }

    const paintWithAudit = laneView.paint({
      widget: 2,
      subs: [],
      pictures: [],
      model: transport.carry({
        family: 'table', theme: TEST_THEME,
        kind: 'game-score',
        view: 'table',
        options: { all: true, membership: null, top: null, by: 'total', factions: false, factionRuns: null, rankMarkers: false },
        settings: tableSettingsOf(undefined, gameScoreColumns(), { density: 'comfortable', sort: { column: 'total', direction: 'descending' } }),
        sources: NO_SOURCES,
        staticPanels: { main: { missing: null, warnings: [], hints: {}, multipliers: [], rows: [{ tag: 'GER', total: { missing: null }, army: { missing: null }, navy: { missing: null }, air: { missing: null }, industry: { missing: null }, land: { missing: null } }] } },
        width: 320,
        height: 200,
        scrollLeft: 0,
        scrollTop: 0,
        audit: true,
        auditId: 42,
      }),
      hour: 10,
      seek: 0,
      speed: 24,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: 100,
    }, auditState, () => `a${seq++}`);
    assert.equal((paintWithAudit.fields.audit?.value as { id?: number } | undefined)?.id, 42, 'P5: serializes audit once capture activates');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }

  const utf8Runs = [
    { tag: '\u{10000}', firstHour: 0, lastHour: 10, countryId: 1 },
    { tag: '\uE000', firstHour: 0, lastHour: 10, countryId: 1 },
  ];
  assert.equal(resolveSlotHolderRuns(utf8Runs)[0]!.tag, '\uE000', 'N6: U+E000 precedes U+10000 in UTF-8 byte order despite higher UTF-16 code unit');
  assert.equal(holderRunAt(utf8Runs, 5)?.tag, '\uE000', 'N6: holderRunAt uses native UTF-8 order');
  const slots = new SlotTags({
    countries: [
      { tag: '\u{10000}', identities: [{ slot: 1, countryId: 1, firstHour: 0, lastHour: 10 }], unresolved: [] },
      { tag: '\uE000', identities: [{ slot: 1, countryId: 1, firstHour: 0, lastHour: 10 }], unresolved: [] },
    ],
  });
  assert.equal(slots.tagAt(1, 5), '\uE000', 'N6: SlotTags.tagAt uses native UTF-8 order');

  const shallowFrozenOuter = Object.freeze([[10, 20] as [number, number], [40, 50] as [number, number]]);
  assert.equal(overlapsHourRanges(shallowFrozenOuter, 15, 25), true, 'N18: indexed overlap finds intersecting interval');
  assert.equal(overlapsHourRanges(shallowFrozenOuter, 21, 39), false, 'N18: indexed overlap skips disjoint interval');
  assert.equal(overlapsHourRanges(shallowFrozenOuter, 50, 60), true, 'N18: indexed overlap includes boundary');
  assert.throws(() => { (shallowFrozenOuter[0] as [number, number])[1] = 999; }, TypeError, 'N18: inner tuple frozen even when outer array was already frozen');


  const mirrorRows = Array.from({ length: 10 }, (_, i) => ({
    key: `R${i}`,
    name: `Row ${i}`,
    cells: { country: { value: knownValue(`Row ${i}`) }, score: { value: knownValue(10 - i) } },
  }));
  const flatMirror = resolveTableRows({ columns: cols, hour: 10, rows: mirrorRows }, settings);
  const mirrorWithIdx = deriveTableMirror(flatMirror, cols, false, { first: 5, count: 2 }, 'R1', 1);
  assert.equal(mirrorWithIdx.rows[0]!.key, 'R1', 'M9: deriveTableMirror uses supplied O(1) focusedIndex');
  assert.equal(mirrorWithIdx.rows[0]!.rowIndex, 1);
});

test('round 5 regressions: F5 revealAt & child revealAt lane validity, F6 1500-country/20-row viewport art loader & unmount cleanup, F7 local vs stream failure origin, and F8 typed tooltip ValueState & audit', async () => {
  const { Lane } = await import('../../draw/lane.ts');
  const { Page } = await import('../../dashboard/follow/page.ts');
  const { tooltipText } = await import('../../charts/tooltip.ts');

  const cols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'flagName', offered: ['flagName'], priority: 0, text: true },
    { key: 'val', label: 'Value', as: 'plain', offered: ['plain'], priority: 1, numeric: true, format: 'decimal1' },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'comfortable', sort: { column: 'name', direction: 'ascending' } });

  registerFamilyProjection({
    kind: 'test-r5-reveal',
    family: 'table', theme: TEST_THEME,
    project(readView) {
      return {
        columns: cols,
        hour: readView.hour,
        unsorted: true,
        rows: [
          {
            key: 'parent',
            name: 'Parent',
            cells: { name: { value: knownValue('Parent') }, val: { value: knownValue(1) } },
            initiallyOpen: true,
            children: [
              { key: 'child-late', name: 'Child Late', revealAt: 25, cells: { name: { value: knownValue('Child Late') }, val: { value: knownValue(2) } } },
            ],
          },
          { key: 'row-late', name: 'Row Late', revealAt: 40, cells: { name: { value: knownValue('Row Late') }, val: { value: knownValue(3) } } },
        ],
      };
    },
  });

  registerFamilyProjection({
    kind: 'test-r5-art-1500',
    family: 'table', theme: TEST_THEME,
    project(readView) {
      return {
        columns: cols,
        hour: readView.hour,
        unsorted: true,
        ...(readView.hour < 200 ? { nextChangeAt: readView.hour < 100 ? 100 : 200 } : {}),
        rows: Array.from({ length: 1500 }, (_, i) => {
          const tag = `C${String(i).padStart(4, '0')}`;
          return {
            key: tag,
            name: tag,
            cells: {
              name: {
                value: knownValue(tag),
                iconSlot: true,
                country: tag,
                art: readView.art({ kind: 'url', url: `/flags/${tag}_${readView.hour < 100 ? 'early' : readView.hour < 200 ? 'mid' : 'late'}.png` }),
              },
              val: { value: knownValue(i + 1) },
            },
          };
        }),
      };
    },
  });

  let throwLocalPaint = false;
  registerFamilyProjection({
    kind: 'test-r5-local-fail',
    family: 'table', theme: TEST_THEME,
    project(readView) {
      if (throwLocalPaint) throw new Error('local projection crash');
      return {
        columns: cols,
        hour: readView.hour,
        rows: [{ key: 'ok', name: 'Ok', cells: { name: { value: knownValue('Ok') }, val: { value: knownValue(7) } } }],
      };
    },
  });

  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  try {
    const presented: { widget: number; hour: number; navCount: number; mode: string; failureOrigin?: string }[] = [];
    const requestedArt: string[] = [];
    const abortedArt: string[] = [];
    const closedArt: string[] = [];
    const artResolvers = new Map<string, (bmp: ImageBitmap) => void>();

    const tableView = createLaneView(tableLane, () => {}, {
      artMaxEntries: 40,
      fetchBitmap: (url, signal) => new Promise<ImageBitmap>((resolve, reject) => {
        requestedArt.push(url);
        signal.addEventListener('abort', () => {
          abortedArt.push(url);
          reject(new DOMException('Aborted', 'AbortError'));
        }, { once: true });
        artResolvers.set(url, resolve);
      }),
    });

    const page = new Page<{ width: number; close(): void }>({ lanes: 1, post: (_lane, msg) => { if (msg.type === 'frame') lane.frame(msg); } });
    const lane = new Lane<{ width: number; close(): void }>({
      lane: 0,
      lanes: 1,
      views: () => tableView,
      crop: async sources => sources.map(s => ({ width: s.width, close() {} })),
      present: presentation => {
        for (const entry of presentation.entries) {
          const nav = entry.fields.nav as readonly unknown[] | undefined;
          presented.push({ widget: entry.widget, hour: entry.lo, navCount: nav ? nav.length : -1, mode: entry.mode, failureOrigin: entry.failureOrigin });
        }
        page.offer(presentation);
        page.frame();
      },
      release: () => {},
      fail: () => {},
      later: run => queueMicrotask(run),
    });

    const flushTicks = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

    const sender = new PayloadSender(message => lane.payloads(structuredClone(message)));
    const wired = (model: unknown) => structuredClone(sender.acquire(model).wire);
    page.mount(1, { route: 0, lane: 0, surfaces: [{ install() {}, clear() {} }] });
    lane.mount(1, [], 'table');
    lane.bind(1, { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null, dated: { h: 10, c: 100 } }, wired({
      family: 'table', theme: TEST_THEME, kind: 'test-r5-reveal', options: NO_OPTIONS, settings, sources: NO_SOURCES, width: 320, height: 200, scrollLeft: 0, scrollTop: 0,
    }));
    page.publish(10, { playing: false });
    await flushTicks();
    assert.equal((page.view(1).fields?.nav as readonly unknown[]).length, 1, 'F5: at hour 10 only parent is visible');
    page.publish(25, { playing: false });
    await flushTicks();
    assert.equal((page.view(1).fields?.nav as readonly unknown[]).length, 2, 'F5: crossing child revealAt=25 invalidates lane window and reveals child row');
    page.publish(40, { playing: false });
    await flushTicks();
    assert.equal((page.view(1).fields?.nav as readonly unknown[]).length, 3, 'F5: crossing row revealAt=40 invalidates lane window and reveals top-level row');

    page.mount(2, { route: 0, lane: 0, surfaces: [{ install() {}, clear() {} }] });
    lane.mount(2, [], 'table');
    const viewportHeight20Rows = 20 * ROW_HEIGHT[settings.density];
    lane.bind(2, { generation: 1, surfaces: [{ width: 320, height: viewportHeight20Rows, dpr: 1 }], offset: 0, pin: null, dated: { h: 120, c: 120 } }, wired({
      family: 'table', theme: TEST_THEME, kind: 'test-r5-art-1500', options: NO_OPTIONS, settings, sources: NO_SOURCES, width: 320, height: viewportHeight20Rows, scrollLeft: 0, scrollTop: 0,
    }));
    page.publish(120, { playing: false });
    await flushTicks();
    assert.equal(requestedArt.length, 6, `F6: 1500-country model starts bounded maxConcurrent=6 visible flags (got ${requestedArt.length})`);
    assert.ok(requestedArt.every(url => url.endsWith('_mid.png')), 'F6: requests only the displayed hour flag, never past/future look history');
    for (let i = 0; i < 10; i++) {
      const url = requestedArt[i]!;
      artResolvers.get(url)!({ width: 20, height: 14, close() { closedArt.push(url); } } as unknown as ImageBitmap);
      await flushTicks();
    }
    assert.equal(requestedArt.length, 16, 'F6: resolving 10 flags pumps the next visible flags up to 6 in flight');
    assert.ok(requestedArt.every(url => url.endsWith('_mid.png')), 'F6: every pumped visible flag is from the displayed hour');
    lane.unmount(2);
    assert.equal(abortedArt.length, 6, 'F6: unmount aborts remaining 6 in-flight fetches');
    assert.equal(closedArt.length, 10, 'F6: unmount closes all 10 decoded bitmaps');

    page.mount(3, { route: 0, lane: 0, surfaces: [{ install() {}, clear() {} }] });
    lane.mount(3, [], 'table');
    throwLocalPaint = true;
    lane.bind(3, { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: 10, dated: { h: 10, c: 10 } }, {
      family: 'table', theme: TEST_THEME, kind: 'test-r5-local-fail', options: {}, settings, sources: {}, width: 320, height: 200, scrollLeft: 0, scrollTop: 0,
    });
    page.publish(10, { playing: false });
    await flushTicks();
    assert.equal(page.view(3).mode, 'failed', 'F7: projection error puts widget in failed mode');
    assert.equal(page.view(3).failureOrigin, 'local', 'F7: local projection error carries local failure origin');
    throwLocalPaint = false;
    page.retryLocal(3);
    lane.bind(3, { generation: page.view(3).binding!, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: 10, dated: { h: 10, c: 10 } }, {
      family: 'table', theme: TEST_THEME, kind: 'test-r5-local-fail', options: {}, settings, sources: {}, width: 320, height: 200, scrollLeft: 0, scrollTop: 0,
    });
    await flushTicks();
    assert.equal(page.view(3).mode, 'picture', 'F7: local retry recovers without restarting pipeline');

    page.name(3, 'failed', 'stream broken', 'stream');
    assert.equal(page.view(3).failureOrigin, 'stream', 'F7: subscription/stream failure carries stream failure origin');

    lane.unmount(1);
    lane.unmount(3);
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }

  const layouts = buildColumnLayouts(cols, settings);
  const mkTipModel = (extraVal: ReturnType<typeof partialValue> | ReturnType<typeof knownValue>) => ({
    columns: cols,
    hour: 10,
    rows: [{
      key: 'GER',
      name: 'Germany',
      cells: { name: { value: knownValue('Germany') }, val: { value: knownValue(10) } },
      tooltipExtra: [{ label: 'Strength', value: extraVal, format: 'decimal1' as const }],
    }],
  });
  const mAtLeast = mkTipModel(partialValue(1.001, 'Missing corps', { atLeast: true }));
  const mPlainPartial = mkTipModel(partialValue(1.001, 'Missing corps', { atLeast: false }));
  const mDiffUnderlying = mkTipModel(partialValue(1.004, 'Missing corps', { atLeast: true }));
  const tipAtLeast = deriveTableTooltip(resolveTableRows(mAtLeast, settings), 'GER', cols)!;
  const extraRow = tipAtLeast.rows.find(r => r.label === 'Strength')!;
  assert.equal(extraRow.value, undefined, 'F8: tooltip row keeps typed ValueState instead of flattened string');
  assert.equal(extraRow.valueState?.kind, 'partial');
  assert.ok(tooltipText(tipAtLeast).includes('1, partial: Missing corps'), 'F8: tooltipText speaks via valueStateText without at least wording');
  assert.ok(!tooltipText(tipAtLeast).includes('at least'), 'F8: tooltipText omits at least wording');

  const auditA = deriveTableAudit({ model: mAtLeast, flatRows: resolveTableRows(mAtLeast, settings), visibleColumns: layouts });
  const auditB = deriveTableAudit({ model: mPlainPartial, flatRows: resolveTableRows(mPlainPartial, settings), visibleColumns: layouts });
  const auditC = deriveTableAudit({ model: mDiffUnderlying, flatRows: resolveTableRows(mDiffUnderlying, settings), visibleColumns: layouts });
  assert.notEqual(
    auditA.find(r => r.key === 'GER:_tooltip')?.value,
    auditB.find(r => r.key === 'GER:_tooltip')?.value,
    'F8: tooltip audit distinguishes atLeast: true vs false',
  );
  assert.notEqual(
    auditA.find(r => r.key === 'GER:_tooltip')?.value,
    auditC.find(r => r.key === 'GER:_tooltip')?.value,
    'F8: tooltip audit distinguishes distinct underlying numeric values that format identically',
  );
});

test('incremental row resolution, column maxima, scrollable/grouped dirty painting, and one-shot rank marker timers', () => {
  const cols: TableColumnSpec[] = [
    { key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'score', label: 'Score', as: 'faint', tone: 'blue', offered: ['faint'], priority: 1, numeric: true, format: 'integer' },
    { key: 'extra', label: 'Extra', as: 'faint', tone: 'blue', offered: ['faint'], priority: 2, numeric: true, format: 'integer' },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'compact', sort: { column: 'score', direction: 'descending' } });
  const layouts = buildColumnLayouts(cols, settings);
  const N = 120;
  let seed = 0x51a7f00d;
  const nextRand = () => {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return seed;
  };
  const scores = Array.from({ length: N }, (_, i) => (i * 17) % 97);
  const extras = Array.from({ length: N }, (_, i) => (i * 13) % 83);
  const makeRow = (i: number): TableRow => {
    const key = `C${String(i).padStart(3, '0')}`;
    return {
      key,
      name: `Country ${key}`,
      cells: {
        name: { value: knownValue(`Country ${key}`) },
        score: { value: scores[i] === 0 ? zeroValue(0) : knownValue(scores[i]!), format: 'integer' },
        extra: { value: extras[i] === 0 ? zeroValue(0) : knownValue(extras[i]!), format: 'integer' },
      },
    };
  };
  let rows = Array.from({ length: N }, (_, i) => makeRow(i));
  let prevModel: TableModel = { columns: cols, rows, hour: 1, rowTimeBoundaries: false, rank: true };
  let prevFlat = resolveTableRows(prevModel, settings);
  let prevIndex = new Map<string, number>();
  prevFlat.forEach((item, idx) => { if (item.kind === 'data') prevIndex.set(item.key, idx); });
  let prevMaxima = computeColumnMaxima(prevFlat, layouts);

  for (let step = 2; step <= 65; step++) {
    const k = step % 7 === 0 ? 0 : 1 + (nextRand() % 8);
    const nextRows = rows.slice();
    const changedKeys = new Set<string>();
    const changedRows = new Map<string, TableRow>();
    for (let j = 0; j < k; j++) {
      const idx = nextRand() % N;
      if (j === 0 && step % 5 === 0) {
        const leaderKey = prevFlat[0]!.key;
        const leaderIdx = Number(leaderKey.slice(1));
        scores[leaderIdx] = 1;
        extras[leaderIdx] = 1;
        const built = makeRow(leaderIdx);
        nextRows[leaderIdx] = built;
        changedKeys.add(built.key);
        changedRows.set(built.key, built);
      } else {
        scores[idx] = nextRand() % 120;
        extras[idx] = nextRand() % 150;
        const built = makeRow(idx);
        nextRows[idx] = built;
        changedKeys.add(built.key);
        changedRows.set(built.key, built);
      }
    }
    const nextModel: TableModel = {
      columns: cols,
      rows: nextRows,
      hour: step,
      rowTimeBoundaries: false,
      rank: true,
      changedKeys,
      changedRows,
    };
    const inc = resolveTableRowsIncremental(prevModel, prevFlat, prevIndex, nextModel, settings);
    assert.ok(inc !== null, `step ${step} uses incremental sort`);
    const incRows: ReturnType<typeof resolveTableRows> = inc.flatRows;
    const full = resolveTableRows(nextModel, settings);
    assert.equal(incRows.length, full.length, `step ${step} length matches`);
    for (let i = 0; i < full.length; i++) {
      const incItem = incRows[i]!;
      const fullItem = full[i]!;
      assert.equal(incItem.kind, 'data');
      assert.equal(fullItem.kind, 'data');
      assert.equal(incItem.key, fullItem.key, `step ${step} key at ${i}`);
      assert.equal(incItem.rank, fullItem.rank, `step ${step} rank at ${i} (${incItem.key})`);
      assert.equal(inc.rowIndex.get(incItem.key), i, `step ${step} rowIndex for ${incItem.key}`);
    }
    const incMax = updateColumnMaximaIncremental(prevMaxima, prevFlat, prevIndex, incRows, inc.rowIndex, layouts, changedKeys);
    const fullMax = computeColumnMaxima(full, layouts);
    assert.deepEqual([...incMax.entries()].sort(), [...fullMax.entries()].sort(), `step ${step} maxima match`);

    rows = nextRows;
    prevModel = nextModel;
    prevFlat = incRows;
    prevIndex = inc.rowIndex;
    prevMaxima = incMax;
  }

  const { ctx, counts } = instrumentedContext();
  (ctx as unknown as { createLinearGradient: () => { addColorStop: () => void } }).createLinearGradient = () => ({ addColorStop() {} });
  const scrollSettings = tableSettingsOf(undefined, cols, { density: 'compact', sort: { column: 'score', direction: 'descending' } });
  const scrollLayout = layoutFamilyTable(buildColumnLayouts(cols, scrollSettings, { name: 160, score: 160, extra: 160 }), 260);
  assert.equal(scrollLayout.scrollable, true);
  const groupedRows = resolveTableRows({
    columns: cols,
    hour: 1,
    rank: true,
    rows: [
      { key: 'A', name: 'Alpha', group: { key: 'g1', label: 'Axis', order: 0 }, cells: { name: { value: knownValue('Alpha') }, score: { value: knownValue(100), format: 'integer' }, extra: { value: knownValue(50), format: 'integer' } } },
      { key: 'B', name: 'Bravo', group: { key: 'g1', label: 'Axis', order: 0 }, cells: { name: { value: knownValue('Bravo') }, score: { value: knownValue(80), format: 'integer' }, extra: { value: knownValue(40), format: 'integer' } } },
      ...Array.from({ length: 20 }, (_, i) => ({
        key: `Z${i}`,
        name: `Z${i}`,
        group: { key: 'g2', label: 'Other', order: 1 },
        cells: { name: { value: knownValue(`Z${i}`) }, score: { value: knownValue(20 - i), format: 'integer' as const }, extra: { value: knownValue(10), format: 'integer' as const } },
      })),
    ],
  }, scrollSettings);
  const pic = createTablePicture();
  drawFamilyTable(ctx, { layout: scrollLayout, rows: groupedRows, settings: scrollSettings, hour: 1, width: 260, height: 100, scrollLeft: 0, scrollTop: 0, rankShown: true, rankWidth: 36, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 }, pic);
  const fillsAfterFull = counts().fillTextCalls;
  const offscreenMutated = groupedRows.slice();
  const lastIdx = offscreenMutated.length - 1;
  const lastItem = offscreenMutated[lastIdx]!;
  if (lastItem.kind === 'data') {
    offscreenMutated[lastIdx] = { ...lastItem, row: { ...lastItem.row, cells: { ...lastItem.row.cells, score: { value: knownValue(19), format: 'integer' } } } };
  }
  const offscreenRes = drawFamilyTable(ctx, { layout: scrollLayout, rows: offscreenMutated, settings: scrollSettings, hour: 2, width: 260, height: 100, scrollLeft: 0, scrollTop: 0, rankShown: true, rankWidth: 36, theme: TEST_THEME, now: 2, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 }, pic);
  assert.equal(offscreenRes.drew, false, 'off-screen row change in scrollable grouped table performs zero paint');
  assert.equal(counts().fillTextCalls, fillsAfterFull, 'off-screen row change emits zero fillText calls');

  const visibleMutated = offscreenMutated.slice();
  const visItem = visibleMutated[2]!;
  if (visItem.kind === 'data') {
    visibleMutated[2] = { ...visItem, row: { ...visItem.row, cells: { ...visItem.row.cells, score: { value: knownValue(81), format: 'integer' } } } };
  }
  const visRes = drawFamilyTable(ctx, { layout: scrollLayout, rows: visibleMutated, settings: scrollSettings, hour: 3, width: 260, height: 100, scrollLeft: 0, scrollTop: 0, rankShown: true, rankWidth: 36, theme: TEST_THEME, now: 3, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 }, pic);
  assert.equal(visRes.drew, true);
  assert.equal(visRes.paintedCells, 1, 'scrollable grouped single-cell change repaints only 1 dirty cell');
  assert.ok(counts().fillTextCalls - fillsAfterFull < fillsAfterFull, `scrollable grouped single-cell change draws fewer texts (${counts().fillTextCalls - fillsAfterFull} vs full ${fillsAfterFull})`);

  registerFamilyProjection(declareTableProjection<{ rows: { id: string; v: number }[] }, { id: string; v: number }, Record<string, never>>({
    kind: 'test-rank-marker-timer',
    rank: true,
    rankMarkers: true,
    columns: [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, field: 'id', text: true },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, field: 'v', numeric: true },
    ],
    rows: reply => ({ rows: reply.rows }),
    rowKey: r => r.id,
    rowName: r => r.id,
  }));
  const recording = installRecordingCanvas();
  try {
    const armed: number[] = [];
    let due: (() => void) | null = null;
    let wakes = 0;
    const view = createLaneView(tableLane, () => { wakes++; }, {
      timers: {
        set: (fn, ms) => { due = fn; armed.push(ms); return armed.length as unknown as ReturnType<typeof setTimeout>; },
        clear: () => { due = null; },
      },
    });
    const lane = driveLane(view, recording.canvases, { width: 320, height: 180, dpr: 1 });
    const rmSettings = tableSettingsOf(undefined, [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'] },
      { key: 'v', label: 'Val', as: 'plain', offered: ['plain'] },
    ], { density: 'compact', sort: { column: 'v', direction: 'descending' }, showRank: true });
    const options = {};
    const sources = {};
    const bound = (rows: { id: string; v: number }[]) => ({ family: 'table' as const, kind: 'test-rank-marker-timer', options, settings: rmSettings, sources, staticPanels: { main: { rows } }, width: 320, height: 180, scrollLeft: 0, scrollTop: 0, theme: TEST_THEME, reducedMotion: true });
    const arrow = () => lane.picture().some(mark => mark.includes('▲'));
    const swappedRows = bound([{ id: 'A', v: 5 }, { id: 'B', v: 30 }]);
    lane.paint({ label: 'before', binding: bound([{ id: 'A', v: 20 }, { id: 'B', v: 10 }]), hour: 1, speed: 24, seek: 1, now: 1000 });
    const swapped = lane.paint({ label: 'swapped', binding: swappedRows, hour: 2, speed: 24, seek: 1, now: 1050 });
    assert.ok(arrow(), 'the forward swap draws the rank arrow');
    assert.equal(swapped.motion, false, 'rank marker expiry does not hold the 60 FPS motion loop');
    assert.equal(swapped.fields.mirror, undefined, 'unfocused playback withholds the mirror');
    assert.deepEqual(armed, [2500], 'one timer, solely for the arrow’s own deadline (no mirror cadence timer)');
    (due as (() => void) | null)?.();
    lane.paint({ label: 'arrow expired', binding: swappedRows, hour: 2, speed: 24, seek: 1, now: 3550 });
    assert.equal(arrow(), false, 'the arrow disappears at its own deadline');
    assert.deepEqual([wakes, armed.length], [1, 1], 'one wake for the arrow deadline and no timer after it');
    view.unmount?.(1, lane.state);
  } finally {
    recording.restore();
  }
});

function gridContext(width: number, height: number) {
  type Box = readonly [number, number, number, number];
  const marks = new Map<string, string>();
  const at = (x: number, y: number) => `${Math.round(x)},${Math.round(y)}`;
  const canvasBox: Box = [0, 0, width, height];
  let clipBox: Box = canvasBox;
  let pending: Box | null = null;
  const stack: Box[] = [];
  const inside = (x: number, y: number) => x >= clipBox[0] && x < clipBox[2] && y >= clipBox[1] && y < clipBox[3];
  const clear = (x: number, y: number, w: number, h: number) => {
    for (const key of [...marks.keys()]) {
      const [px, py] = key.split(',').map(Number) as [number, number];
      if (px >= x && px < x + w && py >= y && py < y + h && inside(px, py)) marks.delete(key);
    }
  };
  const mark = (x: number, y: number, text: string) => { if (inside(x, y)) marks.set(at(x, y), text); };
  const target: Record<string, unknown> = {
    font: '', fillStyle: '', strokeStyle: '', lineWidth: 1, textAlign: 'left', textBaseline: 'alphabetic', globalAlpha: 1,
    save: () => { stack.push(clipBox); },
    restore: () => { clipBox = stack.pop() ?? canvasBox; },
    beginPath: () => { pending = null; },
    rect: (x: number, y: number, w: number, h: number) => { pending = [x, y, x + w, y + h]; },
    clip: () => {
      if (pending) clipBox = [Math.max(clipBox[0], pending[0]), Math.max(clipBox[1], pending[1]), Math.min(clipBox[2], pending[2]), Math.min(clipBox[3], pending[3])];
    },
    fillText: (text: string, x: number, y: number) => mark(x, y, String(text)),
    fillRect: (x: number, y: number, w: number, h: number) => clear(x, y, w, h),
    clearRect: (x: number, y: number, w: number, h: number) => clear(x, y, w, h),
    drawImage: (_img: unknown, x: number, y: number) => mark(x, y, 'image'),
    measureText: (text: string) => ({ width: String(text).length * 7 }),
    createLinearGradient: () => ({ addColorStop() {} }),
    createPattern: () => null,
    getTransform: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }),
  };
  const ctx = new Proxy(target, { get: (t, k) => (k in t ? t[k as string] : () => {}) });
  return { ctx, marks };
}

test('an incremental table paint after a seek back, a same-hour correction and a reorder equals a fresh paint, cell by cell', () => {
  type Row = { id: string; v: number; flag: string };
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'flag', label: 'Flag', as: 'plain', offered: ['plain'], priority: 1, text: true },
    { key: 'v', label: 'Val', as: 'faint', barEligible: true, offered: ['plain', 'faint'], priority: 2, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-fresh-equals-incremental',
    family: 'table', theme: TEST_THEME,
    project: readView => {
      const reply = readView.panel('main') as { rows: Row[]; refused?: string };
      return {
        hour: readView.hour,
        ...(reply.refused ? { refused: reply.refused } : {}),
        rank: true,
        defaultRankShown: true,
        rankMarkers: true,
        columns,
        rows: reply.rows.map(r => ({
          key: r.id,
          name: r.id,
          cells: {
            name: { value: knownValue(r.id) },
            flag: { value: knownValue('Owner'), flags: [{ tag: r.flag }] },
            v: { value: knownValue(r.v), format: 'integer' },
          },
        })),
      };
    },
  });
  const contexts: ReturnType<typeof gridContext>[] = [];
  const prevOffscreen = globalThis.OffscreenCanvas;
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { const grid = gridContext(this.width, this.height); contexts.push(grid); return grid.ctx; }
    transferToImageBitmap() { return { close() {} }; }
  };
  try {
    const settingsFor = (direction: 'ascending' | 'descending') => tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'v', direction } });
    const lane = () => {
      const view = createLaneView(tableLane, () => {});
      const state = new Map<string, unknown>();
      const transport = new LaneTransport();
      let seq = 1;
      const before = contexts.length;
      const paint = (rows: Row[], hour: number, settings: TableSettings) => view.paint({
        widget: 1,
        subs: [],
        pictures: [],
        model: transport.carry({ family: 'table' as const, kind: 'test-fresh-equals-incremental', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: { main: { rows } }, width: 360, height: 200, scrollLeft: 0, scrollTop: 0, theme: TEST_THEME, reducedMotion: true }),
        hour,
        seek: 0, speed: 24,
        binding: { generation: 1, surfaces: [{ width: 360, height: 200, dpr: 1 }], offset: 0, pin: null },
        inspect: null,
        now: 5000,
      }, state, () => `c${seq++}`);
      return { paint, marks: () => new Map(contexts[before]!.marks) };
    };
    const fresh = (rows: Row[], hour: number, settings: TableSettings) => {
      const other = lane();
      other.paint(rows, hour, settings);
      return other.marks();
    };
    const atOne: Row[] = [{ id: 'A', v: 20, flag: 'X' }, { id: 'B', v: 10, flag: 'Y' }, { id: 'C', v: 5, flag: 'Z' }];
    const atTwo: Row[] = [{ id: 'A', v: 5, flag: 'X' }, { id: 'B', v: 30, flag: 'Y' }, { id: 'C', v: 6, flag: 'Z' }];
    const corrected: Row[] = [{ id: 'A', v: 20, flag: 'W' }, { id: 'B', v: 10, flag: 'Y' }, { id: 'C', v: 5, flag: 'Z' }];
    const descending = settingsFor('descending');
    const ascending = settingsFor('ascending');
    const played = lane();
    const first = played.paint(atOne, 1, descending);
    const same = played.paint([...atOne], 1, descending);
    assert.equal(same.surfaces[0]?.content, first.surfaces[0]?.content, 'unchanged visible cells keep their bitmap');
    played.paint(atTwo, 2, descending);
    assert.ok([...played.marks().values()].some(text => text.includes('▲')), 'a forward step marks the rank change');

    played.paint(atOne, 1, descending);
    assert.deepEqual(played.marks(), fresh(atOne, 1, descending), 'after a seek back the picture equals a fresh paint');

    played.paint(corrected, 1, descending);
    assert.deepEqual(played.marks(), fresh(corrected, 1, descending), 'a same-hour correction of a cell’s sole flag repaints it');

    played.paint(corrected, 1, ascending);
    assert.deepEqual(played.marks(), fresh(corrected, 1, ascending), 'a reorder at the same hour equals a fresh paint');

    const replaced = played.paint([{ id: 'D', v: 40, flag: 'X' }, { id: 'B', v: 10, flag: 'Y' }, { id: 'C', v: 5, flag: 'Z' }], 2, ascending);
    const navKeys = (replaced.fields.nav?.value as readonly (readonly [string])[]).map(item => item[0]);
    assert.deepEqual([...navKeys].sort(), ['B', 'C', 'D'], 'navigation installed with the picture offers the subjects painted, never the replaced one');

    const grouped = resolveTableRows({
      hour: 10,
      columns,
      rows: [
        { key: 'P', name: 'P', cells: { name: { value: knownValue('P') }, v: { value: knownValue(1) } } },
        { key: 'Q', name: 'Q', revealAt: 20, group: { key: 'g', label: 'Group' }, cells: { name: { value: knownValue('Q') }, v: { value: knownValue(2) } } },
      ],
    }, descending);
    assert.deepEqual(grouped.map(item => item.kind), ['data'], 'a group revealed later adds no heading before its hour');

    const laneBinding = (rows: Row[], settings: TableSettings, scrollTop = 0, reducedMotion = true) => ({
      family: 'table' as const, theme: TEST_THEME, kind: 'test-fresh-equals-incremental', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: { main: { rows } },
      width: 360, height: 200, scrollLeft: 0, scrollTop, reducedMotion,
    });
    const tableSurface = { width: 360, height: 200, dpr: 1 };
    const heldOne = laneBinding(atOne, descending);
    assertQuietFrames(() => createLaneView(tableLane, () => {}), { label: 'table', binding: heldOne, hour: 1 }, 24, tableSurface);
    assertTooltipDated(() => createLaneView(tableLane, () => {}), { label: 'table', binding: heldOne, hour: 1 }, firstRow, 5, tableSurface);
    assertFreshPaint(() => createLaneView(tableLane, () => {}), [
      { label: 'at one', binding: heldOne, hour: 1, check: true },
      { label: 'at two', binding: laneBinding(atTwo, descending), hour: 2, speed: 24 },
      { label: 'seek back', binding: heldOne, hour: 1, check: true },
      { label: 'correction', binding: laneBinding(corrected, descending), hour: 1, check: true },
      { label: 'reorder', binding: laneBinding(corrected, ascending), hour: 1, check: true },
      { label: 'scrolled', binding: laneBinding(corrected, ascending, 10), hour: 1, check: true },
    ], tableSurface);
    const refusedBinding = { ...laneBinding([], descending), staticPanels: { main: { rows: [], refused: 'Scores were not recorded at this hour' } } };
    for (const reducedMotion of [true, false]) {
      assertContinuity(() => createLaneView(tableLane, () => {}), {
        from: { label: `at one${reducedMotion ? '' : ', gliding'}`, binding: laneBinding(atOne, descending, 0, reducedMotion), hour: 1 },
        refused: { label: 'a refused hour', binding: refusedBinding, hour: 2 },
        to: { label: `rank markers at three${reducedMotion ? '' : ' with a glide'}`, binding: laneBinding(atTwo, descending, 0, reducedMotion), hour: 3 },
      }, tableSurface);
    }
    type Side = { key: string; lead: string; total: number; rows: Row[] };
    const shareColumns: TableColumnSpec[] = [
      { key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true },
      ...['a', 'b', 'c', 'd', 'e', 'f'].map((key, at): TableColumnSpec => ({ key, label: key.toUpperCase(), as: 'plain', offered: ['plain'], priority: at + 1, numeric: true, cellKind: 'share', barEligible: false, minWidth: 96 })),
    ];
    const shareCell = (v: number) => ({ value: v === 0 ? { kind: 'zero' as const, value: 0 } : knownValue(v), format: { decimals: 1, percent: true } as const, bar: { fraction: knownValue(v / 100) }, countryColor: '#cc3333' });
    registerFamilyProjection({
      kind: 'test-panes-fresh',
      family: 'table', theme: TEST_THEME,
      project: readView => {
        const sides = (readView.panel('main') as { sides: Side[] }).sides;
        const row = (key: string, name: string, v: number, group?: boolean) => ({
          key, name,
          ...(group ? { group: { key: 'side', label: 'Share of the side' } } : {}),
          cells: { name: { value: knownValue(name) }, ...Object.fromEntries(['a', 'b', 'c', 'd', 'e', 'f'].map((col, at) => [col, shareCell(Math.max(0, v - at))])) },
        });
        return {
          hour: readView.hour,
          columns: shareColumns,
          rows: [],
          panes: sides.map(side => ({
            key: side.key,
            title: { name: knownValue(side.lead) },
            totalRow: row(`total:${side.key}`, 'Share of the war', side.total),
            rows: side.rows.map(r => row(`${side.key}:${r.id}`, r.id, r.v, true)),
          })),
        };
      },
    });
    const paneLane = () => {
      const view = createLaneView(tableLane, () => {});
      const state = new Map<string, unknown>();
      let seq = 1;
      const before = contexts.length;
      const transport = new LaneTransport();
      const paint = (sides: Side[], hour: number, settings: TableSettings, scrollLeft = 0) => view.paint({
        widget: 2,
        subs: [],
        pictures: [],
        model: { family: 'table' as const, kind: 'test-panes-fresh', options: transport.carry({}), settings, sources: {}, staticPanels: { main: { sides } }, width: 700, height: 240, scrollLeft, scrollTop: 0, theme: TEST_THEME, reducedMotion: true },
        hour,
        seek: 0, speed: 24,
        binding: { generation: 1, surfaces: [{ width: 700, height: 240, dpr: 1 }], offset: 0, pin: null },
        inspect: null,
        now: 5000,
      }, state, () => `p${seq++}`);
      return { paint, marks: () => new Map(contexts[before]!.marks) };
    };
    const freshPanes = (sides: Side[], hour: number, settings: TableSettings, scrollLeft = 0) => {
      const other = paneLane();
      other.paint(sides, hour, settings, scrollLeft);
      return other.marks();
    };
    const sharesBy = (direction: 'ascending' | 'descending') => tableSettingsOf(undefined, shareColumns, { density: 'compact', barsBehind: 'off', sort: { column: 'a', direction } });
    const sidesAt = (shift: number): Side[] => [
      { key: 'left', lead: 'Alpha', total: 60 + shift, rows: [{ id: 'P', v: 70 - shift, flag: '' }, { id: 'Q', v: 30 + shift, flag: '' }] },
      { key: 'right', lead: 'Beta', total: 40 - shift, rows: [{ id: 'R', v: 55, flag: '' }, { id: 'S', v: 45, flag: '' }, { id: 'T', v: 0, flag: '' }] },
    ];
    const panes = paneLane();
    panes.paint(sidesAt(0), 1, sharesBy('descending'));
    const stepped = panes.paint(sidesAt(30), 2, sharesBy('descending'));
    const paneNav = (stepped.fields.nav?.value as readonly (readonly [string])[]).map(item => item[0]);
    assert.deepEqual(paneNav, ['total:left', 'left:Q', 'left:P', 'total:right', 'right:R', 'right:S', 'right:T'], 'navigation runs down the first pane, then the second, each in its own sorted order under its total row');
    assert.ok([...panes.marks().values()].some(text => text.includes('Share of the war')), 'the total row is drawn');
    assert.ok([...panes.marks().values()].some(text => text.includes('Share of the side')), 'the group label is drawn');
    const widthsField = stepped.fields.widths?.value as { totalWidth: number };
    assert.ok(widthsField.totalWidth > 700, 'six share columns overflow each pane, so the view scrolls sideways');
    const paneHit = stepped.hit?.([700 - 20, 30 + 26 * 2]);
    assert.ok(typeof paneHit === 'string' && paneHit.startsWith('row:right:'), `a point in the second pane hits that pane's row (${String(paneHit)})`);
    panes.paint(sidesAt(0), 1, sharesBy('descending'));
    assert.deepEqual(panes.marks(), freshPanes(sidesAt(0), 1, sharesBy('descending')), 'two panes after a seek back equal a fresh paint');
    const correctedSides = sidesAt(0).map(side => (side.key === 'right' ? { ...side, lead: 'Beta renamed', total: 41 } : side));
    panes.paint(correctedSides, 1, sharesBy('descending'));
    assert.deepEqual(panes.marks(), freshPanes(correctedSides, 1, sharesBy('descending')), 'a same-hour correction in one pane equals a fresh paint');
    panes.paint(correctedSides, 1, sharesBy('ascending'));
    assert.deepEqual(panes.marks(), freshPanes(correctedSides, 1, sharesBy('ascending')), 'a reorder in both panes equals a fresh paint');
    panes.paint(correctedSides, 1, sharesBy('ascending'), 120);
    assert.deepEqual(panes.marks(), freshPanes(correctedSides, 1, sharesBy('ascending'), 120), 'one shared sideways offset moves both panes as a fresh paint would');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('on a 1,500-row table a scroll or a focus change derives no navigation again and serialises nothing that grows with the rows, and a rank-animation frame copies and sorts no mover off the viewport', () => {
  type Row = { id: string; v: number };
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-rank-1500',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, rank: true, defaultRankShown: true, rankMarkers: true, columns,
      rows: (readView.panel('main') as { rows: Row[] }).rows.map(r => ({ key: r.id, name: r.id, cells: { name: { value: knownValue(r.id) }, v: { value: knownValue(r.v), format: 'integer' } } })),
    }),
  });
  const N = 1500;
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'v', direction: 'descending' } });
  const ascending = Array.from({ length: N }, (_, i): Row => ({ id: `C${String(i).padStart(4, '0')}`, v: i }));
  const reversed = ascending.map((row, i): Row => ({ ...row, v: N - i }));
  const panels = new Map<Row[], { readonly main: { readonly rows: Row[] } }>();
  const bound = (rows: Row[], scrollTop = 0, focusedKey: string | null = null) => ({
    family: 'table' as const, theme: TEST_THEME, kind: 'test-rank-1500', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: panels.get(rows) ?? panels.set(rows, { main: { rows } }).get(rows)!,
    width: 600, height: 300, scrollLeft: 0, scrollTop, focusedKey, reducedMotion: false,
  });
  const counted = <T>(run: () => T): { readonly result: T; readonly serialised: number; readonly mapped: number; readonly sorted: number } => {
    const stringify = JSON.stringify;
    const set = Map.prototype.set;
    const sort = Array.prototype.sort;
    let serialised = 0;
    let mapped = 0;
    let sorted = 0;
    JSON.stringify = ((...args: Parameters<typeof stringify>) => { const text = stringify(...args); serialised += text?.length ?? 0; return text; }) as typeof stringify;
    Map.prototype.set = function (this: Map<unknown, unknown>, key: unknown, value: unknown) { mapped++; return set.call(this, key, value); };
    Array.prototype.sort = function (this: unknown[], compare?: (a: unknown, b: unknown) => number) { sorted += this.length; return sort.call(this, compare); };
    try {
      return { result: run(), serialised, mapped, sorted };
    } finally {
      JSON.stringify = stringify;
      Map.prototype.set = set;
      Array.prototype.sort = sort;
    }
  };
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 600, height: 300, dpr: 1 });
    const first = lane.paint({ label: 'ascending', binding: bound(ascending), hour: 1, speed: 24, seek: 1, now: 1000 });
    const navText = JSON.stringify(first.fields.nav?.value).length;
    for (const [label, binding] of [['a scroll', bound(ascending, 600)], ['a focus change', bound(ascending, 600, 'C0700')]] as const) {
      const step = counted(() => lane.paint({ label, binding, hour: 1, speed: 24, seek: 1, now: 1001 }));
      for (const name of ['nav', 'widths', 'meta']) assert.equal(step.result.fields[name], first.fields[name], `${label}: ${name} is not derived again`);
      assert.ok(step.serialised < navText / 10, `${label}: serialises ${step.serialised} characters, not the ${navText} of the navigation`);
    }
    lane.paint({ label: 'every row moves', binding: bound(reversed, 600, 'C0700'), hour: 2, speed: 24, seek: 1, now: 2000 });
    const frame = counted(() => lane.paint({ label: 'a frame of the glide', binding: bound(reversed, 600, 'C0700'), hour: 2, speed: 24, seek: 1, now: 2050 }));
    assert.equal(frame.result.motion, true, 'the glide is still running');
    assert.ok(frame.mapped < 100, `an animation frame copies ${frame.mapped} map entries, not one per mover`);
    assert.ok(frame.sorted < 100, `an animation frame sorts ${frame.sorted} entries, not every mover`);
  } finally {
    recording.restore();
  }
});

test('a glide frame reads only the movers whose path crosses the viewport, however far one other row travels', () => {
  const N = 1500;
  const rows = Array.from({ length: N }, (_, i): FlatTableRow => ({ kind: 'data', key: `R${i}`, name: `R${i}`, level: 1, rank: i + 1, row: { key: `R${i}`, name: `R${i}`, cells: {} } } as FlatTableRow));
  const rowIndex = new Map(rows.map((row, i) => [row.key, i] as const));
  const offsets = new Map(rows.map((row, i) => [row.key, { from: i === 0 ? (N - 1) * 20 : (i % 2 ? 20 : -20), to: 0, started: 0 }] as const));
  const glide = tableGlide(offsets, rows, rowIndex, null, 20);
  let read = 0;
  const movers = new Proxy(glide.movers as object[], { get: (target, key, receiver) => { if (typeof key === 'string' && /^\d+$/.test(key)) read++; return Reflect.get(target, key, receiver); } });
  const crossing = glideCrossing({ ...glide, movers } as typeof glide, 20000, 20300);
  const brute = rows.flatMap((_, i) => {
    const from = offsets.get(`R${i}`)!.from;
    return i * 20 + Math.max(from, 0) + 20 > 20000 && i * 20 + Math.min(from, 0) < 20300 ? [i] : [];
  });
  assert.deepEqual([...crossing].sort((a, b) => a - b), brute);
  assert.ok(read <= crossing.length, `a frame reads ${read} movers for ${crossing.length} on screen, not every mover above it`);
});

test('a scroll or focus step on a two-pane table costs the same at 150 rows as at 1,500, and the mirror names the focused row', () => {
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-panes-1500',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, columns, rows: [],
      panes: ['A', 'B'].map(side => ({
        key: side, title: { name: knownValue(side) },
        rows: Array.from({ length: (readView.panel('main') as { perPane: number }).perPane }, (_, i) => ({ key: `${side}${i}`, name: `${side}${i}`, cells: { name: { value: knownValue(`${side}${i}`) }, v: { value: knownValue(i + 1), format: 'integer' as const } } })),
      })),
    }),
  });
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'v', direction: 'ascending' } });
  const recording = installRecordingCanvas();
  const push = Array.prototype.push;
  const find = Array.prototype.find;
  const steps = (perPane: number) => {
    const staticPanels = { main: { perPane } };
    const bound = (scrollTop: number, focusedKey: string | null = null) => ({
      family: 'table' as const, theme: TEST_THEME, kind: 'test-panes-1500', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels,
      width: 600, height: 300, scrollLeft: 0, scrollTop, focusedKey, reducedMotion: false,
    });
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 600, height: 300, dpr: 1 });
    lane.paint({ label: 'first', binding: bound(0), hour: 1, now: 1000 });
    let now = 1000;
    return ([['a scroll', bound(1000)], ['a focus change', bound(1000, 'B40')]] as const).map(([label, binding]) => {
      let pushed = 0;
      Array.prototype.push = function (this: unknown[], ...items: unknown[]) { pushed += items.length; return push.apply(this, items); };
      Array.prototype.find = function (this: unknown[], ...args: Parameters<typeof find>) { pushed += this.length; return find.apply(this, args); } as typeof find;
      let step;
      try {
        step = lane.paint({ label, binding, hour: 1, now: now += 5000 });
      } finally {
        Array.prototype.push = push;
        Array.prototype.find = find;
      }
      const mirrored = (step.fields.mirror?.value as { rows: { key: string }[] } | undefined)?.rows.map(row => row.key) ?? [];
      return { label, pushed, mirrored };
    });
  };
  try {
    const small = steps(75);
    const large = steps(750);
    for (let at = 0; at < small.length; at++) {
      const [few, many] = [small[at]!, large[at]!];
      assert.ok(many.pushed - few.pushed < 20, `${many.label}: ${many.pushed} entries pushed or searched at 1,500 rows against ${few.pushed} at 150`);
      assert.deepEqual(many.mirrored, few.mirrored, `${many.label}: the mirror speaks the rows on screen, whatever the panes hold`);
    }
    assert.ok(large[1]!.mirrored.includes('B40'), 'the focused row is the one mirrored');
  } finally {
    Array.prototype.push = push;
    Array.prototype.find = find;
    recording.restore();
  }
});

test('an animation frame of a 1,500-row table with a flag per row joins no list that grows with the rows', () => {
  type Row = { id: string; v: number };
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-art-1500',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, rank: true, defaultRankShown: true, rankMarkers: true, columns,
      rows: (readView.panel('main') as { rows: Row[] }).rows.map(r => {
        readView.art({ kind: 'url', url: `asset://flag-${r.id}.png` });
        return { key: r.id, name: r.id, cells: { name: { value: knownValue(r.id) }, v: { value: knownValue(r.v), format: 'integer' } } };
      }),
    }),
  });
  const N = 1500;
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'v', direction: 'descending' } });
  const ascending = Array.from({ length: N }, (_, i): Row => ({ id: `C${String(i).padStart(4, '0')}`, v: i }));
  const reversed = ascending.map((row, i): Row => ({ ...row, v: N - i }));
  const bound = (rows: Row[]) => ({
    family: 'table' as const, theme: TEST_THEME, kind: 'test-art-1500', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: { main: { rows } },
    width: 600, height: 300, scrollLeft: 0, scrollTop: 600, focusedKey: null, reducedMotion: false,
  });
  const recording = installRecordingCanvas();
  const join = Array.prototype.join;
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 600, height: 300, dpr: 1 });
    lane.paint({ label: 'ascending', binding: bound(ascending), hour: 1, speed: 24, seek: 1, now: 1000 });
    const moving = bound(reversed);
    lane.paint({ label: 'every row moves', binding: moving, hour: 2, speed: 24, seek: 1, now: 2000 });
    let joined = 0;
    Array.prototype.join = function (this: unknown[], separator?: string) { joined += this.length; return join.call(this, separator); };
    let frame;
    try {
      frame = lane.paint({ label: 'a frame of the glide', binding: moving, hour: 2, speed: 24, seek: 1, now: 2050 });
    } finally {
      Array.prototype.join = join;
    }
    assert.equal(frame.motion, true, 'the glide is still running');
    assert.ok(joined < 200, `an animation frame joins ${joined} list entries, not one per row`);
  } finally {
    Array.prototype.join = join;
    recording.restore();
  }
});

test('after a refresh with the same flags, a one-row scroll hands the art cache the offscreen list it already holds', () => {
  type Row = { id: string; v: number };
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'flagName', offered: ['flagName'], priority: 0, text: true },
    { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-art-scroll-1500',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, columns,
      rows: (readView.panel('main') as { rows: Row[] }).rows.map(r => {
        const url = `asset://flag-${r.id}.png`;
        readView.art({ kind: 'url', url });
        return { key: r.id, name: r.id, cells: { name: { value: knownValue(r.id), iconSlot: true, art: readyArt(url) }, v: { value: knownValue(r.v), format: 'integer' } } };
      }),
    }),
  });
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  const rows = Array.from({ length: 1500 }, (_, i): Row => ({ id: `C${String(i).padStart(4, '0')}`, v: i }));
  const bound = (scrollTop: number) => ({
    family: 'table' as const, theme: TEST_THEME, kind: 'test-art-scroll-1500', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: { main: { rows } },
    width: 600, height: 300, scrollLeft: 0, scrollTop, focusedKey: null, reducedMotion: false,
  });
  const recording = installRecordingCanvas();
  const request = LaneArtCache.prototype.requestVisible;
  const offscreens: unknown[] = [];
  LaneArtCache.prototype.requestVisible = function (this: LaneArtCache, visible, offscreen) { offscreens.push(offscreen); return request.call(this, visible, offscreen); };
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 600, height: 300, dpr: 1 });
    lane.paint({ label: 'first', binding: bound(0), hour: 1, now: 1000 });
    lane.paint({ label: 'a refresh with the same flags', binding: bound(0), hour: 2, now: 2000 });
    lane.paint({ label: 'one row further', binding: bound(34), hour: 2, now: 3000 });
    assert.ok(offscreens.length >= 2, 'the scroll asks for the newly visible flag');
    assert.equal(offscreens.at(-1), offscreens[0], 'the offscreen list handed over is the one the cache already holds');
  } finally {
    LaneArtCache.prototype.requestVisible = request;
    recording.restore();
  }
});

test('a log scrolled deep into its rows numbers them from the top of the log, in its navigation and its mirror', () => {
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
  ];
  registerFamilyProjection({
    kind: 'test-deep-log',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, columns, unsorted: true, windowOffset: 600, totalRows: 2000,
      rows: Array.from({ length: 20 }, (_, i) => ({ key: `R${600 + i}`, name: `R${600 + i}`, cells: { name: { value: knownValue(`R${600 + i}`) } } })),
    }),
  });
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 400, height: 300, dpr: 1 });
    const step = lane.paint({
      label: 'deep',
      binding: { family: 'table' as const, theme: TEST_THEME, kind: 'test-deep-log', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: { main: {} }, width: 400, height: 300, scrollLeft: 0, scrollTop: 610 * ROW_HEIGHT.normal, focusedKey: null, reducedMotion: false },
      hour: 1, now: 1000,
    });
    const nav = step.fields.nav?.value as readonly (readonly [string, string, number])[];
    assert.deepEqual([nav[0]![0], nav[0]![2], nav.at(-1)![2]], ['R600', 600, 619], 'each row carries its ordinal in the whole log');
    const mirror = step.fields.mirror?.value as { readonly first: number; readonly total: number; readonly rows: readonly { readonly key: string; readonly rowIndex: number }[] };
    assert.equal(mirror.total, 2000, 'the mirror counts the whole log');
    assert.equal(mirror.first, 610 - TABLE_WINDOW_OVERSCAN, 'the mirror window starts at the row on screen, counted from the top of the log');
    assert.ok(mirror.rows.length > 0 && mirror.rows.every(row => row.rowIndex === Number(row.key.slice(1))), 'each mirrored row is numbered from the top of the log');
  } finally {
    recording.restore();
  }
});

test('glides, figures and bars outside the displayed range leave the table canvas untouched', () => {
  type Row = { id: string; v: number };
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true },
    { key: 'v', label: 'Val', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    kind: 'test-offscreen-glide',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour, columns,
      rows: (readView.panel('main') as { rows: Row[] }).rows.map(r => ({ key: r.id, name: r.id, cells: { name: { value: knownValue(r.id) }, v: { value: knownValue(r.v), format: 'integer' } } })),
    }),
  });
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'v', direction: 'descending' } });
  const rows = Array.from({ length: 200 }, (_, i): Row => ({ id: `C${String(i).padStart(3, '0')}`, v: 1000 - i }));
  const swapped = rows.map(row => (row.id === 'C150' ? { ...row, v: 1000 - 151 } : row.id === 'C151' ? { ...row, v: 1000 - 150 } : row));
  const panels = new Map<Row[], { readonly main: { readonly rows: Row[] } }>();
  const bound = (list: Row[], scrollTop: number) => ({
    family: 'table' as const, theme: TEST_THEME, kind: 'test-offscreen-glide', options: NO_OPTIONS, settings, sources: NO_SOURCES, staticPanels: panels.get(list) ?? panels.set(list, { main: { rows: list } }).get(list)!,
    width: 400, height: 300, scrollLeft: 0, scrollTop, focusedKey: null, reducedMotion: false,
  });
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 400, height: 300, dpr: 1 });
    const first = lane.paint({ label: 'first', binding: bound(rows, 0), hour: 1, speed: 24, seek: 1, now: 1000 });
    const frames = recording.canvases[0]!.frames.length;
    const below = lane.paint({ label: 'two rows far below swap', binding: bound(swapped, 0), hour: 2, speed: 24, seek: 1, now: 2000 });
    assert.equal(below.motion, false, 'nothing on screen moves, so no frame is asked for');
    assert.equal(recording.canvases[0]!.frames.length, frames, 'an off-screen reorder never touches the canvas');
    assert.equal(below.surfaces[0]?.content, first.surfaces[0]?.content, 'and the rows on screen are not drawn again');
    const scrolled = lane.paint({ label: 'scrolled to the swap while it glides', binding: bound(swapped, 150 * ROW_HEIGHT.normal), hour: 2, speed: 24, seek: 1, now: 2050 });
    assert.equal(scrolled.motion, true, 'a glide scrolled into view moves again');
    const rounded = rows.map((row, i) => i === 0 ? { ...row, v: row.v + 0.1 } : row);
    lane.paint({ label: 'back at the top', binding: bound(rows, 0), hour: 3, speed: 24, now: 3000 });
    const settled = recording.canvases[0]!.frames.length;
    lane.paint({ label: 'same displayed integer', binding: bound(rounded, 0), hour: 4, speed: 24, now: 4000 });
    assert.equal(recording.canvases[0]!.frames.length, settled, 'precision below the displayed figure costs no canvas work');

    const byName = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
    byName.columns.v!.as = 'faint';
    const largestBelow = (v: number) => ({ ...bound(rows.map((row, i) => i === rows.length - 1 ? { ...row, v } : row), 0), settings: byName });
    lane.paint({ label: 'bars scaled to an off-screen maximum', binding: largestBelow(2500.1), hour: 5, speed: 24 });
    const bars = recording.canvases[0]!.frames.length;
    lane.paint({ label: 'same bar widths', binding: largestBelow(2500.2), hour: 6, speed: 24 });
    assert.equal(recording.canvases[0]!.frames.length, bars);
    lane.paint({ label: 'shorter visible bars', binding: largestBelow(5000), hour: 7, speed: 24 });
    assert.ok(recording.canvases[0]!.frames.length > bars);
  } finally {
    recording.restore();
  }
});

test('a focused action still paints where its label extends left of an off-screen column', () => {
  const columns: TableColumnSpec[] = [
    { key: 'pinned', label: 'Pinned', as: 'plain', offered: ['plain'], priority: 0 },
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0 },
    { key: 'action', label: 'Action', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'action' },
  ];
  const kind = 'test-overhanging-action';
  registerFamilyProjection({
    kind, family: 'table', theme: TEST_THEME,
    project: view => ({
      columns, hour: view.hour, nextChangeAt: view.hour + 1, unsorted: true,
      rows: [{ key: 'r', name: 'Row', cells: { action: { value: knownValue('Open'), actionLabel: view.hour === 1 ? 'Show construction orders' : 'Show details' } } }],
    }),
  });
  const surface = { width: 145, height: 32, dpr: 1 };
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  settings.columns.pinned!.width = 40;
  settings.columns.name!.width = 100;
  settings.columns.action!.width = 40;
  const binding = { family: 'table', kind, theme: TEST_THEME, options: NO_OPTIONS, sources: NO_SOURCES, settings, ...surface, scrollTop: 0, scrollLeft: 0, focusedKey: 'r' };
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, surface);
    lane.paint({ label: 'long button', binding, hour: 1, speed: 24 });
    const frames = recording.canvases[0]!.frames;
    const first = frames.length;
    lane.paint({ label: 'shorter button', binding, hour: 2, speed: 24 });
    assert.ok(frames.length > first);
    assert.ok(frames.at(-1)!.includes('fillText("Show details")'));
  } finally {
    recording.restore();
  }
});

test('the movers a viewport evaluates are exactly those whose path crosses it', () => {
  const N = 1500;
  const rows = Array.from({ length: N }, (_, i): FlatTableRow => ({ kind: 'data', key: `R${i}`, name: `R${i}`, level: 1, rank: i + 1, row: { key: `R${i}`, name: `R${i}`, cells: {} } } as FlatTableRow));
  const rowIndex = new Map(rows.map((row, i) => [row.key, i] as const));
  const offsets = new Map(rows.map((row, i) => [row.key, { from: (N - 1 - 2 * i) * 20, to: 0, started: 0 }] as const));
  const glide = tableGlide(offsets, rows, rowIndex, null, 20);
  for (const [top, bottom] of [[-300, 0], [0, 300], [14000, 14300], [29700, 30000], [30000, 30300]] as const) {
    const brute = rows.flatMap((_, i) => {
      const from = offsets.get(`R${i}`)!.from;
      const lo = i * 20 + Math.min(from, 0);
      const hi = i * 20 + Math.max(from, 0) + 20;
      return hi > top && lo < bottom ? [i] : [];
    });
    assert.equal(hasGlideCrossing(glide, top, bottom), brute.length > 0);
    assert.deepEqual([...glideCrossing(glide, top, bottom)].sort((a, b) => a - b), brute, `viewport ${top}..${bottom}`);
  }
  assert.ok(glideCrossing(glide, 0, 300).length < 40, 'a viewport at the top meets only the rows passing through it');
});

test('playback keeps fitted columns until growth, pause, seek, resize or a setting changes them', () => {
  const cols: TableColumnSpec[] = [
    { key: 'item', label: 'Item', priority: 0, cellKind: 'item', as: 'plain', offered: ['plain'] },
    { key: 'detail', label: 'Detail', priority: 1, text: true, as: 'plain', offered: ['plain'] },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'normal', sort: { column: 'item', direction: 'ascending' } });
  const kind = 'test-playback-fitted-widths';
  registerFamilyProjection(declareTableProjection<{ text: string }, { text: string }, Record<string, never>>({
    kind,
    columns: () => cols.map(column => ({ ...column, value: row => knownValue(row.text) })),
    rows: reply => ({ rows: [reply] }),
    rowKey: () => 'event',
    rowName: () => 'Event',
  }));
  const recorded = installRecordingCanvas();
  try {
    const surface = { width: 620, height: 180, dpr: 1 };
    const lane = driveLane(createLaneView(tableLane, () => {}), recorded.canvases, surface);
    const bind = (text: string, tableSettings = settings) => ({
      family: 'table' as const, kind, options: NO_OPTIONS, settings: tableSettings, sources: NO_SOURCES,
      staticPanels: { main: { text } }, theme: TEST_THEME,
      width: surface.width, height: surface.height, scrollLeft: 0, scrollTop: 0, reducedMotion: true,
    });
    const short = bind('Short');
    const wide = bind('A significantly wider event detail');
    const wider = bind('The next recorded detail needs more horizontal space');
    const fit = (binding: ReturnType<typeof bind>, hour: number, speed = 24, seek = 0) => {
      const paint = lane.paint({ label: `Hour ${hour}`, binding, hour, speed, seek });
      return paint.fields.widths!.value as { widths: Record<string, number> };
    };
    const exact = fit(short, 10, 0);
    const grown = fit(wide, 11);
    assert.ok(grown.widths.detail! > exact.widths.detail!);
    assert.ok(grown.widths.item! < exact.widths.item!, 'the item column uses the remaining room');
    assert.deepEqual(fit(short, 12), grown, 'a shorter detail leaves both displayed widths in place');
    const bigger = fit(wider, 13);
    assert.ok(bigger.widths.detail! > grown.widths.detail!, 'a wider sample grows through the ordinary fit');
    assert.deepEqual(fit(short, 14), bigger);
    assert.deepEqual(fit(short, 14, 0), exact, 'pausing the same picture refits it');
    fit(wide, 15);
    assert.deepEqual(fit(short, 16), grown);
    assert.deepEqual(fit(short, 16, 24, 1), exact, 'seeking to the same hour refits it');
    fit(wide, 17, 24, 1);
    surface.height += 20;
    assert.deepEqual(fit(short, 18, 24, 1), exact, 'a height-only resize refits the columns');
    fit(wide, 19, 24, 1);
    assert.deepEqual(fit(bind('Short', { ...settings, density: 'compact' }), 20, 24, 1), exact, 'a settings edit refits the columns');
  } finally {
    recorded.restore();
  }
});

test('cell kinds for converted tables: an item column takes the spare width, two-way bars scale by magnitude, declared extras reach the cell, a blank cell draws nothing, and names that arrive re-measure the column', () => {
  const kinds: TableColumnSpec[] = [
    { key: 'pic', label: '', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'kind', sortable: false },
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
    { key: 'net', label: 'Net', as: 'plain', offered: ['plain'], priority: 1, numeric: true, cellKind: 'twoWay', widest: ['+99'] },
  ];
  const kindSettings = tableSettingsOf(undefined, kinds, { density: 'normal', sort: { column: 'net', direction: 'descending' } });
  const grown = layoutFamilyTable(buildColumnLayouts(kinds, kindSettings, { pic: 52, name: 128, net: 140 }), 600);
  assert.deepEqual(grown.columns.map(c => c.width), [58, 600 - 58 - 146, 146], 'the item column takes all the spare width; the kind and two-way columns keep theirs');

  const signedRows = resolveTableRows({ hour: 1, columns: kinds, rows: [
    { key: 'A', name: 'A', cells: { name: { value: knownValue('A') }, net: { value: knownValue(-30) } } },
    { key: 'B', name: 'B', cells: { name: { value: knownValue('B') }, net: { value: knownValue(10) } } },
  ] }, kindSettings);
  assert.equal(computeColumnMaxima(signedRows, grown.columns).get('net'), 30, 'a two-way column scales by the largest magnitude');
  const netColumn = grown.columns.find(col => col.key === 'net')!;
  const loss = signedRows.find(item => item.kind === 'data' && item.key === 'A');
  assert.equal(loss?.kind === 'data' ? resolveCellPaintModel(loss.row.cells.net, netColumn, new Map([['net', 30]])).bar.fraction : null, 1, 'a loss draws its bar at its magnitude');
  const measuredNet = measureFixedColumnWidths(buildColumnLayouts([kinds[2]!], kindSettings), kindSettings, TEST_THEME, text => text.length * 7).net!;
  assert.ok(measuredNet - 2 * 10 >= 72 + 8 + 7 * 3, `a two-way column leaves room for its 72 px slot beside the figure (${measuredNet})`);

  const partialModel = { hour: 1, rank: true, columns: kinds, populationPartial: 'Some countries were not read at this hour', rows: [
    { key: 'small', name: 'small', cells: { name: { value: knownValue('small') }, net: { value: knownValue(1) } } },
    { key: 'large', name: 'large', cells: { name: { value: knownValue('large') }, net: { value: knownValue(9) } } },
  ] };
  assert.deepEqual(resolveTableRows(partialModel, kindSettings).map(item => item.key), ['small', 'large'], 'a partly answered population keeps the product’s order, never re-ranked in the client');

  const shadedColumns: TableColumnSpec[] = [kinds[1]!, { key: 'oil', label: 'Oil', as: 'plain', offered: ['plain'], priority: 1, numeric: true, cellKind: 'shaded' }];
  const shadedSettings = tableSettingsOf(undefined, shadedColumns, { density: 'compact', sort: { column: 'oil', direction: 'descending' } });
  const shadedLayout = layoutFamilyTable(buildColumnLayouts(shadedColumns, shadedSettings, { name: 128, oil: 60 }), 400);
  const shadedRows = resolveTableRows({ hour: 1, columns: shadedColumns, rows: [
    { key: 'A', name: 'A', cells: { name: { value: knownValue('A') }, oil: { value: knownValue(-40) } } },
    { key: 'B', name: 'B', cells: { name: { value: knownValue('B') }, oil: { value: knownValue(12) } } },
  ] }, shadedSettings);
  assert.equal(computeColumnMaxima(shadedRows, shadedLayout.columns).get('oil'), 40, 'a shaded column scales imports and exports by the largest magnitude');

  const extraProjection = declareTableProjection<{ rows: { id: string; group: boolean }[] }, { id: string; group: boolean }, Record<string, never>>({
    kind: 'test-cell-extras',
    columns: [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, field: 'id', text: true, extra: row => ({ subtext: `${row.id} stored`, flags: [{ tag: 'GER', art: PENDING_ART }, { tag: 'SOV', art: PENDING_ART, dimmed: true }] }) },
      { key: 'total', label: 'Total', as: 'plain', offered: ['plain'], priority: 1, numeric: true, value: () => notApplicable('Group rows only'), extra: row => (row.group ? {} : { blank: true }) },
    ],
    rows: reply => ({ rows: reply.rows }),
    rowKey: r => r.id,
    rowName: r => r.id,
  });
  const extraModel = extraProjection.project(createBoundedReadView({ hour: 1, panels: new Map([['main', { rows: [{ id: 'G', group: true }, { id: 'C', group: false }] }]]) }), {});
  assert.equal(extraModel.rows[0]!.cells.name!.subtext, 'G stored');
  assert.deepEqual(extraModel.rows[0]!.cells.name!.flags?.map(f => [f.tag, f.dimmed === true]), [['GER', false], ['SOV', true]], 'two flags in one cell, the actor first and the other dimmed');
  assert.equal(extraModel.rows[1]!.cells.total!.blank, true);

  const grid = gridContext(400, 120);
  const extraSettings = tableSettingsOf(undefined, extraModel.columns, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  const extraLayout = layoutFamilyTable(buildColumnLayouts(extraModel.columns, extraSettings, { name: 200, total: 120 }), 400);
  drawFamilyTable(grid.ctx as unknown as CanvasRenderingContext2D, { layout: extraLayout, rows: resolveTableRows(extraModel, extraSettings), settings: extraSettings, hour: 1, width: 400, height: 120, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, markers: new Map(), dpr: 1 });
  const texts = [...grid.marks.values()];
  assert.equal(texts.filter(t => t === '—').length, 1, 'the group row shows its not-applicable dash; the blank child cell draws nothing');

  const names = new Map<string, string>([['A', 'A']]);
  registerFamilyProjection({
    kind: 'test-names-arrive',
    family: 'table', theme: TEST_THEME,
    project: readView => ({
      hour: readView.hour,
      columns: [{ key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true }],
      rows: [{ key: 'A', name: names.get('A')!, cells: { name: { value: knownValue(names.get('A')!) } } }],
    }),
  });
  const prevOffscreen = globalThis.OffscreenCanvas;
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return gridContext(this.width, this.height).ctx; }
    transferToImageBitmap() { return { close() {} }; }
  };
  try {
    const view = createLaneView(tableLane, () => {});
    const state = new Map<string, unknown>();
    let seq = 1;
    const settings = tableSettingsOf(undefined, [{ key: 'name', label: 'Country', as: 'plain', offered: ['plain'] }], { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
    const transport = new LaneTransport();
    const paint = () => view.paint({
      widget: 1, subs: [], pictures: [],
      model: { family: 'table' as const, kind: 'test-names-arrive', options: transport.carry({}), settings, sources: {}, staticPanels: { main: { at: seq } }, width: 120, height: 80, scrollLeft: 0, scrollTop: 0, theme: TEST_THEME, reducedMotion: true },
      hour: 1, seek: 0, speed: 0, binding: { generation: 1, surfaces: [{ width: 120, height: 80, dpr: 1 }], offset: 0, pin: null }, inspect: null, now: 1,
    }, state, () => `c${seq++}`);
    const widthOf = (painted: ReturnType<typeof paint>) => (painted.fields.widths?.value as { widths: Record<string, number> }).widths.name!;
    const before = widthOf(paint());
    names.set('A', 'A country whose recorded name arrived later and is long');
    const after = widthOf(paint());
    assert.ok(after > before, `a name that arrives after the first measurement widens its column (${before} -> ${after})`);
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }

  const withAction: TableColumnSpec[] = [
    kinds[0]!,
    { key: 'name', label: 'Organisation', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
    { key: 'size', label: 'Size', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
    { key: 'spare', label: '', as: 'plain', offered: ['plain'], priority: 2 },
    { key: 'view', label: '', as: 'plain', offered: ['plain'], priority: 3, cellKind: 'action', sortable: false },
  ];
  const named = 'Army Ordnance Department';
  const tipRows = resolveTableRows({ hour: 1, columns: withAction, rows: [{
    key: 'org', name: named, tooltipAction: 'Open its organisation window',
    cells: { pic: { value: knownValue(named), iconSlot: true }, name: { value: knownValue(named) }, size: { value: zeroValue(0) }, spare: { value: knownValue('stray') }, view: { value: knownValue('View'), actionLabel: 'View' } },
  }] }, tableSettingsOf(undefined, withAction, { density: 'normal', sort: { column: 'size', direction: 'descending' } }));
  const tip = deriveTableTooltip(tipRows, 'org', withAction)!;
  assert.equal(tip.title, named, 'the name column, not the picture before it, is the title');
  assert.deepEqual(tip.rows.map(row => row.label), ['Size'], 'no unlabelled row, no repeated name, no action word');
  assert.equal(tip.note, 'Open its organisation window', 'an action column contributes only its hint');
});

test('tree rows align whether or not they can open: a level with openable rows gives every row of that level the chevron lane, children start under their parent\'s name', () => {
  const tree: TableColumnSpec[] = [
    { key: 'pic', label: '', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'kind', sortable: false },
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
  ];
  const settings = tableSettingsOf(undefined, tree, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  const leaf = (key: string, children?: TableRow[]): TableRow => ({ key, name: key, cells: { name: { value: knownValue(key) } }, ...(children ? { children } : {}) });
  const rows = resolveTableRows({ hour: 1, columns: tree, rows: [leaf('A', [leaf('A1')]), leaf('B'), leaf('C', [leaf('C1', [leaf('C1a')]), leaf('C2')])] }, settings, { A: true, C: true, C1: true });
  const data = rows.filter((item): item is Extract<FlatTableRow, { kind: 'data' }> => item.kind === 'data');
  assert.deepEqual(data.map(item => [item.key, item.level, item.isTreeGroup]), [['A', 1, true], ['A1', 2, false], ['B', 1, false], ['C', 1, true], ['C1', 2, true], ['C1a', 3, false], ['C2', 2, false]]);
  const levels = treeLevelsOf(rows);
  assert.equal(treeLevelsOf(rows), levels, 'held per row list');
  const starts = Object.fromEntries(data.map(item => [item.key, treeTextStart(item.level, levels)]));
  assert.deepEqual(starts, { A: TREE_CHEVRON_LANE, A1: 2 * TREE_CHEVRON_LANE, B: TREE_CHEVRON_LANE, C: TREE_CHEVRON_LANE, C1: 2 * TREE_CHEVRON_LANE, C1a: 2 * TREE_CHEVRON_LANE, C2: 2 * TREE_CHEVRON_LANE });
  const flat = resolveTableRows({ hour: 1, columns: tree, rows: [leaf('A', [leaf('A1')]), leaf('B')] }, settings, { A: true });
  const flatLevels = treeLevelsOf(flat);
  assert.deepEqual(flat.map(item => item.kind === 'data' ? treeTextStart(item.level, flatLevels) : -1), [TREE_CHEVRON_LANE, TREE_CHEVRON_LANE, TREE_CHEVRON_LANE], 'a leaf, a group and a child of a two-level tree start at one x');
  assert.equal(treeTextStart(1, treeLevelsOf(resolveTableRows({ hour: 1, columns: tree, rows: [leaf('B')] }, settings))), 0, 'a flat table reserves no lane');
  const layout = layoutFamilyTable(buildColumnLayouts(tree, settings, { pic: 52, name: 200 }), 400);
  const nameX = layout.columns[0]!.width;
  assert.equal(hitFamilyTable(layout, flat, { x: nameX + 12, y: 10 }, 36, 0, 0, false, 0).toggle, true, 'the chevron opens its group');
  assert.equal(hitFamilyTable(layout, flat, { x: nameX + 12, y: 36 * 2 + 10 }, 36, 0, 0, false, 0).toggle, false, 'a leaf has no chevron to hit');
});

test('log capability: headerless widths, date-grouped boundaries, gap/refused/loading slots, in-place children, and fresh-paint equality after seek back, same-hour correction and reorder', () => {
  const DAY1 = 43_800_000 + 400 * 24;
  const DAY2 = DAY1 + 24;
  const DAY3 = DAY2 + 24;
  const logColumns: TableColumnSpec[] = [
    { key: 'date', label: 'Occurred On Date', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'date', sortable: true },
    { key: 'kind', label: 'Category', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'kind', sortable: false },
    { key: 'country', label: 'Origin Country', as: 'flag', offered: ['flag', 'flagName'], priority: 2, sortable: false },
    { key: 'item', label: 'Item', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
    { key: 'cost', label: 'Cost', as: 'plain', offered: ['plain'], priority: 3, numeric: true, format: 'integer' },
    { key: 'state', label: 'Outcome State', as: 'plain', offered: ['plain'], priority: 4, cellKind: 'status' },
  ];
  const baseSettings = tableSettingsOf(undefined, logColumns, { density: 'normal', sort: { column: 'date', direction: 'descending' } });
  const baseCols = buildColumnLayouts(logColumns, baseSettings);
  const measure = (text: string) => text.length * 7;
  const headedWidths = measureFixedColumnWidths(baseCols, baseSettings, TEST_THEME, measure);
  const headerlessWidths = measureFixedColumnWidths(baseCols, baseSettings, TEST_THEME, measure, undefined, { hideHeader: true });
  assert.ok(headerlessWidths.date! < headedWidths.date!, 'headerless mode ignores hidden header label width on date column');
  assert.ok(headerlessWidths.country! < headedWidths.country!, 'headerless mode keeps flag-only country column compact');
  const grown = layoutFamilyTable(buildColumnLayouts(logColumns, baseSettings, headerlessWidths), 720);
  assert.ok(grown.columns.find(c => c.key === 'item')!.width > 300, 'item column absorbs spare width while date/kind/flag/status stay fixed');

  const mkLogRow = (
    key: string,
    hour: number,
    cost: number,
    opts: {
      live?: boolean;
      statusKind?: 'running' | 'waiting' | 'succeeded' | 'finished' | 'ended';
      statusLabel?: string;
      inlinePrefix?: string;
      inlineSubtext?: string;
      title?: string;
      children?: readonly TableRow[];
    } = {},
  ): TableRow => ({
    key,
    name: opts.title ?? key,
    rowHour: hour,
    ...(opts.live ? { live: true } : {}),
    ...(opts.children ? { children: opts.children } : {}),
    cells: {
      date: { value: knownValue(hour) },
      kind: { value: knownValue('dec'), iconSlot: true, artShape: 'square', artName: 'Decision' },
      country: { value: knownValue('GER'), flags: [{ tag: 'GER', art: PENDING_ART }] },
      item: {
        value: knownValue(opts.title ?? key),
        ...(opts.inlinePrefix ? { inlinePrefix: opts.inlinePrefix } : {}),
        ...(opts.inlineSubtext ? { inlineSubtext: opts.inlineSubtext } : {}),
      },
      cost: { value: knownValue(cost), format: 'integer', unitSuffix: 'PP' },
      state: opts.statusKind
        ? { value: knownValue(opts.statusLabel ?? opts.statusKind), statusKind: opts.statusKind }
        : { value: notRecorded('Outcome not recorded'), missingFigure: 'Outcome' },
    },
  });

  const childRow = mkLogRow('c1', DAY2 + 2, 10, { title: 'Delivery 1', statusKind: 'finished', statusLabel: 'Delivered' });
  const slottedModel: TableModel = {
    columns: logColumns,
    hour: DAY3 + 12,
    hideHeader: true,
    unsorted: true,
    dateGrouped: true,
    rows: [],
    slots: [
      { kind: 'data', row: mkLogRow('live0', DAY3 + 8, 50, { live: true, title: 'First live same day', statusKind: 'running', statusLabel: 'Running' }) },
      { kind: 'data', row: mkLogRow('live1', DAY3 + 5, 50, { live: true, title: 'Stop the Strikes', inlineSubtext: 'Mission', statusKind: 'running', statusLabel: 'Running', children: [childRow] }) },
      { kind: 'data', row: mkLogRow('h1', DAY2 + 10, 25, { title: 'Event \uFFFC choice', inlinePrefix: 'A', statusKind: 'succeeded', statusLabel: 'Succeeded' }) },
      { kind: 'data', row: mkLogRow('h2', DAY2 + 4, 15, { title: 'Same day exact', statusKind: 'finished', statusLabel: 'Finished' }) },
      { kind: 'data', row: mkLogRow('h3', DAY2 + 2, 5, { title: 'Same day again', statusKind: 'ended', statusLabel: 'Ended' }) },
      { kind: 'gap', key: 'gap:1', firstHour: DAY1 + 6, lastHour: DAY2 - 1, reason: 'Collector detached' },
      { kind: 'data', row: mkLogRow('h4', DAY1 + 3, 100, { title: 'Before gap' }) },
      { kind: 'refused', key: 'ref:1', reason: 'Block damaged' },
      { kind: 'loading', key: 'load:1' },
    ],
  };

  const flatExpanded = resolveTableRows(slottedModel, baseSettings, { live1: true });
  assert.deepEqual(
    flatExpanded.map(item => (item.kind === 'data'
      ? [item.kind, item.key, item.level, item.showDate !== false, item.isDayEnd === true, item.isLiveEnd === true]
      : [item.kind, item.key])),
    [
      ['data', 'live0', 1, true, false, false],
      ['data', 'live1', 1, false, false, false],
      ['data', 'c1', 2, true, false, true],
      ['data', 'h1', 1, true, false, false],
      ['data', 'h2', 1, false, false, false],
      ['data', 'h3', 1, false, true, false],
      ['gap', 'gap:1'],
      ['data', 'h4', 1, true, true, false],
      ['refused', 'ref:1'],
      ['loading', 'load:1'],
    ],
    'same-day rows suppress repeat dates, the live block band follows its last open child, and gap/refused/loading slots preserve order',
  );

  const geom = computeRowLayout(flatExpanded, 'normal', 'standard');
  const queue = Array.from({ length: 120 }, (_, i) => ({ kind: 'data', key: `q${i}`, level: i % 4 === 0 ? 1 : 2, isLiveEnd: false, row: {} }) as unknown as FlatTableRow);
  const queueGeom = computeRowLayout(queue, 'comfortable', 'standard');
  const deep = tableRowWindow(queue.length, queueGeom, ROW_HEIGHT.comfortable, queueGeom.tops[100]!, 200);
  assert.equal(deep.first + TABLE_WINDOW_OVERSCAN, 100, 'deep in an expanded queue the published window starts at the row under the scroll position, not at scrollTop / 44');
  assert.ok(queueGeom.tops[deep.first + deep.count - TABLE_WINDOW_OVERSCAN - 1]! < queueGeom.tops[100]! + 200, 'and ends at the last row the viewport shows');
  const gapTop = geom.tops[6]!;
  assert.deepEqual(hitFamilyTable(grown, flatExpanded, { x: 40, y: gapTop + 10 }, 36, 0, 0, false, 0, undefined, undefined, undefined, geom), { kind: 'row', rowKey: 'gap:1' });
  const gapScreen = gridContext(720, 120);
  drawFamilyTable(gapScreen.ctx as unknown as CanvasRenderingContext2D, { layout: grown, rows: [{ kind: 'gap', key: 'gap:36', firstHour: DAY1, lastHour: DAY1 + 35, reason: 'Collector detached' }],
    settings: baseSettings, hour: DAY3, width: 720, height: 120, scrollLeft: 0, scrollTop: 0, rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 1, glideDuration: 0, glide: NO_GLIDE, dpr: 1 });
  assert.ok([...gapScreen.marks.values()].includes('1 day 12 hours'), 'a 36-hour gap is told as a day and hours, not rounded to two days');
  const gapTip = deriveTableTooltip(flatExpanded, 'gap:1', logColumns);
  assert.equal(gapTip?.title, 'Not recorded');
  assert.equal(gapTip?.detail, 'Collector detached');
  const audit = deriveTableAudit({ model: slottedModel, flatRows: flatExpanded, visibleColumns: grown.columns });
  assert.ok(audit.some(r => r.key === 'gap:1:gap'));
  assert.ok(audit.some(r => r.key === 'ref:1:refused'));
  assert.ok(audit.some(r => r.key === 'load:1:loading'));

  const gappedModel: TableModel = {
    columns: logColumns,
    hour: DAY2 + 5,
    hideHeader: true,
    unsorted: true,
    dateGrouped: true,
    rows: [
      mkLogRow('live0', DAY2 + 4, 20, { live: true, statusKind: 'waiting', statusLabel: 'Waiting' }),
      mkLogRow('after', DAY2 + 1, 10, { statusKind: 'finished', statusLabel: 'Finished' }),
      mkLogRow('before', DAY1 + 1, 10, { statusKind: 'finished', statusLabel: 'Finished' }),
    ],
    gaps: [{ firstHour: DAY1 + 12, lastHour: DAY3, reason: 'Unrecorded stretch' }],
  };
  const gappedFlat = resolveTableRows(gappedModel, baseSettings);
  assert.deepEqual(gappedFlat.map(r => r.kind === 'gap' ? `gap:${r.firstHour}-${r.lastHour}` : r.key), [
    'live0',
    `gap:${DAY1 + 12}-${DAY2 + 5}`,
    'after',
    'before',
  ], 'interleaveLogGaps clamps gaps to model.hour and never splits the live block');

  const sortByCost = tableSettingsOf(undefined, logColumns, { density: 'normal', sort: { column: 'cost', direction: 'descending' } });
  const costSorted = resolveTableRows({
    columns: logColumns,
    hour: DAY3,
    dateGrouped: true,
    rows: [
      mkLogRow('a', DAY2 + 8, 10, { statusKind: 'finished' }),
      mkLogRow('b', DAY2 + 4, 40, { statusKind: 'finished' }),
    ],
  }, sortByCost);
  assert.deepEqual(costSorted.map(r => r.kind === 'data' && r.showDate !== false), [true, true], 'sorting by a non-date figure shows every row date');

  type LogScene = {
    readonly rows: readonly TableRow[];
    readonly slots?: readonly TableModelSlot[];
    readonly unsorted?: boolean;
  };
  registerFamilyProjection({
    kind: 'test-log-fresh-equals-incremental',
    family: 'table', theme: TEST_THEME,
    project: readView => {
      const reply = readView.panel('main') as LogScene;
      return {
        hour: readView.hour,
        hideHeader: true,
        dateGrouped: true,
        unsorted: reply.unsorted,
        columns: logColumns,
        rows: reply.rows,
        ...(reply.slots ? { slots: reply.slots } : {}),
      };
    },
  });

  const makeRichGrid = (width: number, height: number) => {
    const base = gridContext(width, height);
    const hPixels = new Map<string, string>();
    const hatchBoxes = new Map<string, string>();
    const canvasBox: readonly [number, number, number, number] = [0, 0, width, height];
    let clipBox: readonly [number, number, number, number] = canvasBox;
    let pending: readonly [number, number, number, number] | null = null;
    const clipStack: (readonly [number, number, number, number])[] = [];
    let moveX = 0;
    let moveY = 0;
    let lineX = 0;
    let lineY = 0;
    let hasLine = false;
    const raw = base.ctx as Record<string, unknown>;
    const wrapped = new Proxy(raw, {
      get(target, prop) {
        if (prop === 'save') {
          const orig = target.save as () => void;
          return () => { clipStack.push(clipBox); orig(); };
        }
        if (prop === 'restore') {
          const orig = target.restore as () => void;
          return () => { clipBox = clipStack.pop() ?? canvasBox; orig(); };
        }
        if (prop === 'beginPath') {
          const orig = target.beginPath as () => void;
          return () => { pending = null; orig(); };
        }
        if (prop === 'rect' || prop === 'roundRect') {
          const orig = target.rect as (x: number, y: number, w: number, h: number) => void;
          return (x: number, y: number, w: number, h: number) => {
            pending = [x, y, x + w, y + h];
            if (prop === 'rect') orig(x, y, w, h);
          };
        }
        if (prop === 'clip') {
          const orig = target.clip as () => void;
          return () => {
            if (pending) {
              clipBox = [Math.max(clipBox[0], pending[0]), Math.max(clipBox[1], pending[1]), Math.min(clipBox[2], pending[2]), Math.min(clipBox[3], pending[3])];
            }
            orig();
          };
        }
        if (prop === 'moveTo') return (x: number, y: number) => { moveX = x; moveY = y; hasLine = false; };
        if (prop === 'lineTo') return (x: number, y: number) => { lineX = x; lineY = y; hasLine = true; };
        if (prop === 'stroke') {
          return () => {
            if (hasLine) {
              const y0 = Math.floor(moveY);
              const y1 = Math.floor(lineY);
              if (y0 === y1 && y0 >= clipBox[1] && y0 < clipBox[3]) {
                const xStart = Math.max(Math.ceil(clipBox[0]), Math.round(Math.min(moveX, lineX)));
                const xEnd = Math.min(Math.floor(clipBox[2]), Math.round(Math.max(moveX, lineX)));
                for (let px = xStart; px < xEnd; px++) hPixels.set(`${px},${y0}`, String(target.strokeStyle));
              } else if (y0 !== y1) {
                hatchBoxes.set(`${Math.round(clipBox[0])},${Math.round(clipBox[1])},${Math.round(clipBox[2])},${Math.round(clipBox[3])}`, String(target.strokeStyle));
              }
              hasLine = false;
            }
          };
        }
        if (prop === 'fillRect' || prop === 'clearRect') {
          const orig = target[prop] as (x: number, y: number, w: number, h: number) => void;
          return (x: number, y: number, w: number, h: number) => {
            orig(x, y, w, h);
            const x0 = Math.max(clipBox[0], x);
            const y0 = Math.max(clipBox[1], y);
            const x1 = Math.min(clipBox[2], x + w);
            const y1 = Math.min(clipBox[3], y + h);
            for (const key of [...hPixels.keys()]) {
              const [px, py] = key.split(',').map(Number) as [number, number];
              if (px >= x0 && px < x1 && py >= y0 && py < y1) hPixels.delete(key);
            }
            for (const key of [...hatchBoxes.keys()]) {
              const [bx0, by0, bx1, by1] = key.split(',').map(Number) as [number, number, number, number];
              if (bx1 > x0 && bx0 < x1 && by1 > y0 && by0 < y1) hatchBoxes.delete(key);
            }
          };
        }
        return target[prop as string];
      },
    });
    return { ctx: wrapped, snapshot: () => ({ marks: new Map(base.marks), hPixels: new Map(hPixels), hatchBoxes: new Map(hatchBoxes) }) };
  };

  const grids: ReturnType<typeof makeRichGrid>[] = [];
  const prevOffscreen = globalThis.OffscreenCanvas;
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { const g = makeRichGrid(this.width, this.height); grids.push(g); return g.ctx; }
    transferToImageBitmap() { return { close() {} }; }
  };
  try {
    const createLane = () => {
      const view = createLaneView(tableLane, () => {});
      const state = new Map<string, unknown>();
      let seq = 1;
      const idx = grids.length;
      const transport = new LaneTransport();
      const paint = (scene: LogScene, hour: number, settings: TableSettings, expanded?: Readonly<Record<string, boolean>>) => view.paint({
        widget: 1,
        subs: [],
        pictures: [],
        model: transport.carry({
          family: 'table' as const,
          kind: 'test-log-fresh-equals-incremental',
          options: NO_OPTIONS,
          settings,
          sources: NO_SOURCES,
          staticPanels: { main: scene },
          width: 640,
          height: 360,
          scrollLeft: 0,
          scrollTop: 0,
          expanded,
          theme: TEST_THEME,
          reducedMotion: true,
        }),
        hour,
        seek: 0, speed: 24,
        binding: { generation: 1, surfaces: [{ width: 640, height: 360, dpr: 1 }], offset: 0, pin: null },
        inspect: null,
        now: 5000,
      }, state, () => `c${seq++}`);
      return { paint, snap: () => grids[idx]!.snapshot() };
    };
    const freshSnap = (scene: LogScene, hour: number, settings: TableSettings, expanded?: Readonly<Record<string, boolean>>) => {
      const other = createLane();
      other.paint(scene, hour, settings, expanded);
      return other.snap();
    };

    const uniformAt1: LogScene = {
      unsorted: true,
      rows: [
        mkLogRow('e2', DAY2 + 8, 50, { title: 'Event Two', inlineSubtext: 'Political', statusKind: 'finished', statusLabel: 'Finished' }),
        mkLogRow('e1', DAY2 + 2, 25, { title: 'Event \uFFFC One', inlinePrefix: 'A', statusKind: 'succeeded', statusLabel: 'Succeeded' }),
        mkLogRow('e0', DAY1 + 6, 10, { title: 'Earlier Day', statusKind: 'ended', statusLabel: 'Ended' }),
      ],
    };
    const uniformAt2: LogScene = {
      unsorted: true,
      rows: [
        mkLogRow('e3', DAY2 + 12, 75, { title: 'Event Three', statusKind: 'finished', statusLabel: 'Finished' }),
        ...uniformAt1.rows,
      ],
    };
    const correctedAt1: LogScene = {
      unsorted: true,
      rows: [
        mkLogRow('e2', DAY2 + 8, 50, { title: 'Event Two', inlineSubtext: 'Revised', statusKind: 'succeeded', statusLabel: 'Succeeded' }),
        mkLogRow('e1', DAY2 + 2, 25, { title: 'Event \uFFFC One', inlinePrefix: 'B', statusKind: 'ended', statusLabel: 'Ended' }),
        mkLogRow('e0', DAY1 + 6, 10, { title: 'Earlier Day', statusKind: 'ended', statusLabel: 'Ended' }),
      ],
    };
    const reorderScene: LogScene = {
      unsorted: false,
      rows: correctedAt1.rows,
    };

    const played = createLane();
    played.paint(uniformAt1, DAY2 + 9, baseSettings);
    played.paint(uniformAt2, DAY2 + 13, baseSettings);
    assert.deepEqual(played.snap(), freshSnap(uniformAt2, DAY2 + 13, baseSettings), 'forward log step shifting repeat-date suppression equals fresh paint');

    played.paint(uniformAt1, DAY2 + 9, baseSettings);
    assert.deepEqual(played.snap(), freshSnap(uniformAt1, DAY2 + 9, baseSettings), 'log seek back restores dates and hairlines identically to fresh paint');

    played.paint(correctedAt1, DAY2 + 9, baseSettings);
    assert.deepEqual(played.snap(), freshSnap(correctedAt1, DAY2 + 9, baseSettings), 'same-hour log correction (inlinePrefix, inlineSubtext, statusKind) equals fresh paint');

    played.paint(reorderScene, DAY2 + 9, sortByCost);
    assert.deepEqual(played.snap(), freshSnap(reorderScene, DAY2 + 9, sortByCost), 'reordering log by figure restores every row date identically to fresh paint');

    const slottedScene: LogScene = { unsorted: true, rows: [], slots: slottedModel.slots };
    played.paint(slottedScene, DAY3 + 12, baseSettings, { live1: true });
    assert.deepEqual(played.snap(), freshSnap(slottedScene, DAY3 + 12, baseSettings, { live1: true }), 'slotted log with live block, expanded children, gap, refused, and loading rows equals fresh paint');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('the table host adapter stores and resolves table settings through its declaration policy and sizes media rows', () => {
  const columns = [{ key: 'a', label: 'A', priority: 1, as: 'number', offered: ['number'], width: 80 }, { key: 'b', label: 'B', priority: 2, as: 'number', offered: ['number'], width: 80 }] as never;
  const declaration = { family: 'table', kind: 'synthetic-settings', columns, settings: { kind: 'table', base: { density: 'normal', barsBehind: 'all', sort: { column: 'a', direction: 'descending' } } } };
  const policy = tableHostAdapter.settings!;
  const initial = policy.resolve(declaration, null, {});
  assert.equal(initial.sort.column, 'a');
  const stored: Record<string, unknown> = {};
  const next: TableSettings = { ...initial, density: 'compact', sort: { column: 'b', direction: 'ascending' } };
  policy.store!(declaration, next, initial, {} as never, (key, value) => { stored[key] = value; });
  const round = policy.resolve(declaration, null, stored);
  assert.deepEqual([round.density, round.sort.column, round.sort.direction], ['compact', 'b', 'ascending'], 'what the header stores is what the table resolves next');
  assert.equal(policy.editable!(declaration), true);
  assert.equal(policy.editable!({ ...declaration, settings: { kind: 'fixed', base: declaration.settings.base } }), false);

  assert.deepEqual(tableHostAdapter.getScrollExtent!({ widths: { widths: {}, totalWidth: 640 }, table: { count: 10, totalHeight: 280 } }), { width: 640, height: 280 });

  const ranked: TableModel = { columns: [{ key: 'a', label: 'A', as: 'number', offered: ['number'], numeric: true }] as never, hour: 5, rank: true, rows: [{ key: 'GER', name: 'Germany', cells: { a: { value: { kind: 'known', value: 3 } } } }] as never };
  const auditCtx = { viewport: { width: 320, height: 200, dpr: 1, scrollLeft: 0, scrollTop: 0, zoomWindow: null }, settings: round, expanded: {}, theme: undefined, fields: {} };
  const rankOf = (settings: TableSettings) => JSON.parse(tableHostAdapter.deriveAuditRows!(ranked, 5, { ...auditCtx, settings }).find(row => row.key === 'GER:_row')!.value!).rank;
  assert.equal(tableRank(ranked, { ...round, showRank: false }).shown, false);
  assert.equal(rankOf({ ...round, showRank: false }), null, 'ranks hidden on the canvas are absent from the exact audit too');
  assert.notEqual(rankOf({ ...round, showRank: true }), null, 'and present when shown');
  assert.deepEqual(tableHostAdapter.parseHit!('toggle:GER'), { key: 'GER', toggle: true, inspectToken: 'row:GER' }, 'hovering a tree chevron inspects its row, so the row tooltip shows');

  const subject = (taken: Value) => ({ hideHeader: true, subjectSummary: { name: knownValue('298. Division'), country: knownValue('Germany'), detail: knownValue('Infantry'),
    level: knownValue('Regular'), figures: [{ label: 'Casualties taken', value: taken }, { label: 'Enemy killed', value: notRecorded('Kills are not recorded') }] } });
  const widths = { widths: { a: 80 }, visible: ['a'], scrollable: false, totalWidth: 640 };
  const panes = { width: 640, titles: [] };
  const chrome = { columns, summaryText: '3 events', summaryPartial: 'Count from recorded hours' };
  const initialChrome = tableHostAdapter.chromeState!({ meta: chrome, widths, panes }, undefined);
  const withMoreRows = { ...chrome, columns: structuredClone(columns), summaryText: '4 events' };
  assert.equal(tableHostAdapter.chromeState!({ meta: withMoreRows, widths, panes }, initialChrome), initialChrome,
    'a changed title count leaves the header controls untouched');
  assert.notEqual(tableHostAdapter.chromeState!({ meta: { ...chrome, columns: [] }, widths, panes }, initialChrome), initialChrome);
  assert.equal(tableHostAdapter.chromeState!({ meta: chrome, widths: { ...widths, totalWidth: 700 }, panes }, initialChrome), initialChrome);
  assert.notEqual(tableHostAdapter.chromeState!({ meta: chrome, widths: { ...widths, widths: { a: 100 } }, panes }, initialChrome), initialChrome);
  assert.notEqual(tableHostAdapter.chromeState!({ meta: chrome, widths, panes: { ...panes, width: 700 } }, initialChrome), initialChrome);

  const first = tableHostAdapter.chromeState!({ meta: subject(knownValue(1904)), widths }, undefined) as TableChromeState;
  const written: string[][] = [];
  first.figures.attach(texts => { written.push([...texts]); });
  const foughtMeta = subject(knownValue(2536));
  const fought = tableHostAdapter.chromeState!({ meta: foughtMeta, widths }, first);
  assert.equal(fought, first, 'a summary figure that only changes its number keeps the chrome state, so playback commits nothing');
  assert.equal(tableHostAdapter.chromeState!({ meta: foughtMeta, widths }, first), first);
  assert.deepEqual(written, [['1,904'], ['2,536']], 'and the new number is written to the band once');
  assert.equal(tableHostAdapter.chromeState!({ meta: subject(knownValue(1904)), widths }, first), first);
  assert.deepEqual(written.at(-1), ['1,904'], 'a seek back restores the first figure without rebuilding chrome');
  assert.notEqual(tableHostAdapter.chromeState!({ meta: subject(notRecorded('Casualties not recorded')), widths }, first), first, 'a figure that stops being recorded rebuilds the band');
});

test('batch 1 log projections: market-ledger contract grouping, decisions running-first, event-choices awaiting-first and detail meta, and event-feed missing dated faction name', () => {
  const marketReply: MarketLedgerTailReply = {
    branchId: '1',
    offset: '0',
    total: '4',
    live: '1',
    counts: [{ key: 'active', total: '1' }],
    nextChange: null,
    rows: [
      {
        line: 'contract',
        key: '0000000000000012',
        hour: 100,
        observed: 100,
        exact: true,
        live: true,
        request: false,
        requestedHour: null,
        startedHour: 100,
        endedHour: null,
        status: 'active',
        seller: 'GER',
        sellerNotRecorded: false,
        buyer: 'FRA',
        buyerNotRecorded: false,
        items: [{
          variant: '6',
          name: 'Infantry Equipment I',
          nameMissing: null,
          icon: null,
          deliveredRaw: '25000000',
          contractedRaw: '50000000',
          deliveryUnavailable: null,
        }],
        deliveryMissing: null,
        paidRaw: '4800000',
        paidMissing: null,
        quotes: [{ variant: '6', name: 'Infantry Equipment I', nameMissing: null, priceRaw: '9600000' }],
        termsMissing: null,
        eventsTotal: '2',
      },
      { line: 'event', key: '3', contract: '0000000000000012', seller: 'GER', buyer: 'FRA', hour: 108, kind: 'payment', variant: null, equipment: null, equipmentMissing: null, amountRaw: '4800000', amountNotRecorded: false, reason: null },
      { line: 'event', key: '2', contract: '0000000000000012', seller: 'GER', buyer: 'FRA', hour: 105, kind: 'delivery', variant: '6', equipment: 'Infantry Equipment I', equipmentMissing: null, amountRaw: '25000000', amountNotRecorded: false, reason: null },
      {
        line: 'contract',
        key: '0000000000000099',
        hour: 100,
        observed: 100,
        exact: true,
        live: false,
        request: false,
        requestedHour: null,
        startedHour: 100,
        endedHour: 102,
        status: 'ended',
        seller: null,
        sellerNotRecorded: true,
        buyer: 'FRA',
        buyerNotRecorded: false,
        items: [{
          variant: '7',
          name: null,
          nameMissing: 'Design not named',
          icon: null,
          deliveredRaw: null,
          contractedRaw: null,
          deliveryUnavailable: 'Delivery amount unavailable: purchase manifest not acquired before removal',
        }],
        deliveryMissing: null,
        paidRaw: null,
        paidMissing: null,
        quotes: [],
        termsMissing: "The contract's terms were not recorded at its start",
        eventsTotal: '0',
      },
    ],
    missing: [],
    warnings: [],
  };
  const marketValidated = validateGenericTailReply(marketReply, 110);
  assert.equal(marketValidated.total, 4);
  const marketSource = buildRowSourceFromHead('mkt', marketReply, undefined, 110)!;
  const marketModel = marketLedgerProjection.project(createBoundedReadView({ hour: 110, rows: new Map([['main', marketSource]]) }), {
    perspective: 'FRA',
    selectionEmpty: null,
  });
  assert.equal(marketModel.rows.length, 4);
  assert.equal(marketModel.rows[0]!.cells.status.statusKind, 'running');
  assert.equal(marketModel.rows[0]!.cells.delivered.value.kind, 'known');
  assert.equal(marketModel.rows[0]!.cells.delivered.value.kind === 'known' ? marketModel.rows[0]!.cells.delivered.value.value : null, '250 / 500');
  assert.equal(marketModel.rows[0]!.cells.paid.value.kind === 'known' ? marketModel.rows[0]!.cells.paid.value.value : null, '\u221248');
  assert.deepEqual(marketModel.rows.slice(1, 3).map(row => row.pageParent), ['group:0000000000000012', 'group:0000000000000012'], 'deliveries and payments remain separate lines under their contract');
  assert.equal(marketModel.rows[1]!.cells.delivered.value.kind, 'notApplicable', 'payment child row never fabricates delivery count');
  assert.equal(marketModel.rows[2]!.cells.paid.value.kind, 'notApplicable', 'delivery child row never pairs payment');
  assert.equal(marketModel.rows[3]!.cells.delivered.value.kind, 'notRecorded', 'unavailable delivery stays notRecorded');
  assert.equal(marketModel.rows[3]!.cells.item.value.kind, 'notRecorded', 'unnamed design stays notRecorded');

  const decRows: ScriptPageRow[] = [
    {
      key: 'd-run',
      hour: 104,
      observed: 104,
      live: true,
      status: 'active',
      occurrence: {
        id: 'd-run',
        opened: true,
        edges: [{
          row: { id: 'd-run-1', hour: 104, readHour: 104, nameHour: 104, exact: true, category: 'politics', kind: 'decisionSelected', tags: ['GER'], participants: ['GER', null], item: { token: 'dec_two', name: 'Mobilize', description: null, icon: null }, amount: null },
          costs: null,
          lifecycle: null,
          days: 10,
          option: null,
        }],
      },
    },
    {
      key: 'd-done',
      hour: 108,
      observed: 108,
      live: false,
      status: 'taken',
      occurrence: {
        id: 'd-done',
        opened: true,
        edges: [{
          row: { id: 'd-done-1', hour: 108, readHour: 108, nameHour: 108, exact: true, category: 'politics', kind: 'decisionSelected', tags: ['GER'], participants: ['GER', null], item: { token: 'dec_one', name: 'War Bonds', description: null, icon: null }, amount: null },
          costs: [{ currency: 'politicalPower', amount: '-50000' }],
          lifecycle: null,
          days: null,
          option: null,
        }],
      },
    },
  ];
  const decSource = buildRowSourceFromHead('dec', { offset: '0', total: '2', rows: decRows, missing: [] }, undefined, 110)!;
  const decModel = decisionsProjection.project(createBoundedReadView({ hour: 110, rows: new Map([['main', decSource]]) }), {
    artwork: true,
    breaks: [],
  });
  assert.equal(decModel.rows.length, 2);
  assert.equal(decModel.rows[0]!.key, 'd-run', 'the product\'s order: the running decision first');
  assert.equal(decModel.rows[0]!.live, true);
  assert.equal(decModel.rows[0]!.cells.cost.value.kind, 'notRecorded');
  assert.equal(decModel.rows[1]!.key, 'd-done');
  assert.equal(decModel.rows[1]!.cells.cost.value.kind, 'known');

  const choiceRows: ScriptPageRow[] = [
    {
      key: 'ev-open',
      hour: 105,
      observed: 105,
      live: true,
      status: 'awaiting',
      occurrence: {
        id: 'ev-open',
        opened: true,
        edges: [{
          row: { id: 'ev-o-1', hour: 105, readHour: 105, nameHour: 105, exact: true, category: 'politics', kind: 'newsEventFired', tags: ['FRA'], participants: [null, 'FRA'], item: { token: 'news.2', name: 'Crisis', description: null, icon: null }, amount: null },
          costs: null,
          lifecycle: null,
          days: null,
          option: null,
        }],
      },
    },
    {
      key: 'ev-chosen',
      hour: 109,
      observed: 107,
      live: false,
      status: 'chosen',
      occurrence: {
        id: 'ev-chosen',
        opened: true,
        edges: [
          {
            row: { id: 'ev-c-1', hour: 107, readHour: 107, nameHour: 107, exact: true, category: 'politics', kind: 'scriptEventFired', tags: ['GER'], participants: [null, 'GER'], item: { token: 'ev.1', name: 'Rhineland', description: 'Text', icon: null, options: [{ ordinal: 0, name: 'Proceed' }] }, amount: null },
            costs: null,
            lifecycle: null,
            days: null,
            option: null,
          },
          {
            row: { id: 'ev-c-2', hour: 109, readHour: 109, nameHour: 109, exact: true, category: 'politics', kind: 'eventOptionSelected', tags: ['GER'], participants: ['GER', null], item: null, amount: null },
            costs: null,
            lifecycle: null,
            days: null,
            option: 0,
          },
        ],
      },
    },
  ];
  const choiceSource = buildRowSourceFromHead('evc', { offset: '0', total: '2', rows: choiceRows, missing: [] }, undefined, 110)!;
  const choiceModel = eventChoicesProjection.project(createBoundedReadView({ hour: 110, rows: new Map([['main', choiceSource]]) }), {
    events: ['country', 'news'],
    awaiting: false,
    artwork: true,
    breaks: [],
  });
  assert.equal(choiceModel.rows.length, 2);
  assert.equal(choiceModel.rows[0]!.key, 'ev-open', 'the product\'s order: the awaiting event first');
  assert.equal(choiceModel.rows[0]!.cells.choice.statusKind, 'waiting');
  assert.equal(choiceModel.rows[0]!.cells.action.actionLabel, 'View');
  assert.equal(choiceModel.rows[1]!.cells.choice.inlinePrefix, 'A');
  assert.ok(choiceModel.rows[1]!.meta, 'chosen event carries detail window subject');

  const feedRows = [
    {
      line: 'event' as const,
      key: 'f-1',
      hour: 106,
      observed: 106,
      live: false,
      choice: null,
      row: {
        id: 'f-1',
        hour: 106,
        readHour: 106,
        nameHour: 106,
        exact: false,
        category: 'diplomacy' as const,
        kind: 'factionJoined' as const,
        tags: ['GER'],
        item: { token: 'faction:1', name: null, nameMissing: 'Faction name not recorded at this hour', description: null, icon: null },
        amount: null,
      },
    },
  ];
  const feedSource = buildRowSourceFromHead('feed', { offset: '0', total: '1', rows: feedRows, missing: [] }, undefined, 110)!;
  const feedModel = eventFeedProjection.project(createBoundedReadView({ hour: 110, rows: new Map([['main', feedSource]]) }), {
    categories: ['diplomacy'],
    artwork: true,
  });
  assert.equal(feedModel.rows.length, 1);
  assert.equal(feedModel.rows[0]!.cells.detail.value.kind, 'notRecorded', 'missing dated faction name stays notRecorded rather than guessing');
});

test('the audit compares which art a cell shows, never whether its bytes have arrived', () => {
  const settings = tableSettingsOf(undefined, CLOCK_COLUMNS, CLOCK_TABLE_BASE);
  const layouts = buildColumnLayouts(CLOCK_COLUMNS, settings);
  const base = campaignClockProjection.project(createBoundedReadView({ hour: 50, panels: new Map([['main', {
    available: true, recordedSeconds: 3661, playingSeconds: 2400, pausedSeconds: 1261, complete: true, warnings: [],
  } as CampaignClockReply]]) }), {});
  const column = CLOCK_COLUMNS[0]!.key;
  const withArt = (art: ArtResult, artKey = JSON.stringify(['sprite', 'GFX_counter', 0, null, false])): TableModel => ({
    ...base,
    rows: base.rows.map(row => ({ ...row, cells: { ...row.cells, [column]: { ...row.cells[column]!, iconSlot: true, artKey, art } } })),
  });
  const audit = (model: TableModel) => deriveTableAudit({ model, flatRows: resolveTableRows(model, settings), visibleColumns: layouts });
  const arrived = audit(withArt(readyArt('http://artwork.localhost/0123456789abcdef0123456789abcdef.png')));
  const arriving = audit(withArt(PENDING_ART));
  assert.equal(pictureError('table', arrived, { values: new Map(arriving.map(row => [row.key, row.value!])), ordered: true }), null);
  const other = audit(withArt(PENDING_ART, JSON.stringify(['sprite', 'GFX_other', 0, null, false])));
  assert.notEqual(pictureError('table', arrived, { values: new Map(other.map(row => [row.key, row.value!])), ordered: true }), null, 'a different picture is still a difference');
});

test('cold table art and flag slots waiting behind the fetch concurrency limit paint as pending, not confirmed missing', () => {
  const artCache = new LaneArtCache({
    maxConcurrent: 6,
    fetchBitmap: () => new Promise<ImageBitmap>(() => {}),
  });
  const columns: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, widest: ['Country 7'] },
    { key: 'badge', label: 'Kind', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'kind', widest: ['Kind'] },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'compact', sort: { column: 'country', direction: 'ascending' } });
  const baseCols = buildColumnLayouts(columns, settings);
  const fixedWidths = measureFixedColumnWidths(baseCols, settings, TEST_THEME, text => text.length * 7);
  const layout = layoutFamilyTable(buildColumnLayouts(columns, settings, fixedWidths), 320, { rankShown: false, rankWidth: 0 });

  const flagUrls = Array.from({ length: 7 }, (_, i) => `art://flag-${i + 1}`);
  const iconUrls = Array.from({ length: 7 }, (_, i) => `art://icon-${i + 1}`);
  artCache.requestVisible([...flagUrls, ...iconUrls]);
  assert.equal(artCache.stateOfUrl(flagUrls[0]!), 'pending');
  assert.equal(artCache.stateOfUrl(flagUrls[6]!), 'unrequested');
  assert.equal(artCache.stateOfUrl(iconUrls[6]!), 'unrequested');

  const rows = resolveTableRows({
    columns,
    hour: 10,
    rows: Array.from({ length: 7 }, (_, i) => ({
      key: `R${i + 1}`,
      name: `Country ${i + 1}`,
      cells: {
        country: {
          value: knownValue(`Country ${i + 1}`),
          country: `T0${i + 1}`,
          iconSlot: true,
          art: readyArt(flagUrls[i]!),
        },
        badge: {
          value: knownValue(`Icon ${i + 1}`),
          iconSlot: true,
          art: readyArt(iconUrls[i]!),
          artName: `Icon ${i + 1}`,
        },
      },
    })),
  }, settings);

  const texts: string[] = [];
  let roundRectCalls = 0;
  const base = instrumentedContext().ctx;
  const recordingCtx = {
    ...base,
    measureText(text: string) {
      return { width: text.length * 5 };
    },
    fillText(text: string) {
      texts.push(text);
    },
    roundRect() {
      roundRectCalls++;
    },
  } as unknown as CanvasRenderingContext2D;

  drawFamilyTable(recordingCtx, {
    layout,
    rows,
    settings,
    hour: 10,
    width: 320,
    height: 400,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 1,
    glideDuration: 0,
    glide: NO_GLIDE,
    artCache,
    iconVersion: artCache.version,
    dpr: 1,
  });

  assert.equal(texts.includes('T07'), false, '7th queued flag waiting behind concurrency limit must not draw missing tag text');
  assert.equal(roundRectCalls, 0, 'queued unrequested art and flag slots must stay pending with no hatched missing box');

  const missingRows = resolveTableRows({
    columns,
    hour: 10,
    rows: [{
      key: 'M1',
      name: 'No Art',
      cells: {
        country: { value: knownValue('No Art'), country: 'MIS', iconSlot: true, art: productArt({ status: 'confirmedMissing', reason: 'no layer holds it' }) },
        badge: { value: knownValue('Missing'), iconSlot: true, artName: 'Missing', art: productArt({ status: 'confirmedMissing', reason: 'no layer holds it' }) },
      },
    }],
  }, settings);
  texts.length = 0;
  roundRectCalls = 0;
  drawFamilyTable(recordingCtx, {
    layout,
    rows: missingRows,
    settings,
    hour: 10,
    width: 320,
    height: 120,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 2,
    glideDuration: 0,
    glide: NO_GLIDE,
    artCache,
    iconVersion: artCache.version,
    dpr: 1,
  });
  assert.ok(roundRectCalls > 0, 'confirmed-missing table art and flag slots paint the hatched missing box');
  assert.ok(texts.includes('MIS'), 'a confirmed-missing flag slot paints the country tag');
  artCache.dispose();
});

test('table partial and withheld notices live in the header summary marker, never a body note row, while mirror description and audit retain them', () => {
  const partialProj = declareTableProjection<{ count: number; detail: string }, { id: string }, Record<string, never>>({
    kind: 'test-header-partial-marker',
    columns: [
      { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, path: 'id' },
    ],
    rows: reply => ({
      rows: [{ id: 'Row1' }],
      titleSummary: `${reply.count} not shown`,
      titleSummaryPartial: `${reply.count} leaders of these countries can’t be shown; Details lists why.\n${reply.detail}`,
      warnings: [reply.detail],
    }),
    rowKey: r => r.id,
    rowName: r => r.id,
  });
  registerFamilyProjection(partialProj);
  const prevOffscreen = globalThis.OffscreenCanvas;
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  }
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = FakeOffscreenCanvas;
  try {
    const laneView = createLaneView(tableLane, () => {});
    const laneState = new Map<string, unknown>();
    const transport = new LaneTransport();
    let seq = 1;
    const painted = laneView.paint({
      widget: 1,
      subs: [],
      pictures: [],
      model: transport.carry({
        family: 'table', theme: TEST_THEME,
        kind: 'test-header-partial-marker',
        options: NO_OPTIONS,
        settings: tableSettingsOf(undefined, [{ key: 'name', label: 'Name', as: 'plain', offered: ['plain'] }], { density: 'normal', sort: { column: 'name', direction: 'ascending' } }),
        sources: NO_SOURCES,
        staticPanels: { main: { count: 3, detail: 'Leader A: condition undecided' } },
        width: 320,
        height: 200,
        scrollLeft: 0,
        scrollTop: 0,
        audit: true,
      }),
      hour: 10,
      seek: 0, speed: 0,
      binding: { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: 100,
    }, laneState, () => `c${seq++}`);
    assert.equal(painted.mode, 'picture');
    const meta = painted.fields.meta!.value as { summaryText: string | null; summaryPartial: string | null; notes: readonly string[]; warnings: readonly string[] };
    assert.deepEqual(meta.notes, [], 'tables never emit a body note row');
    assert.equal(meta.summaryText, '3 not shown', 'the header marker carries the compact count');
    assert.equal(meta.summaryPartial, '3 leaders of these countries can’t be shown; Details lists why.\nLeader A: condition undecided', 'the header marker carries the full sentence and per-leader reasons');
    assert.deepEqual(meta.warnings, ['Leader A: condition undecided'], 'Details still lists the per-leader reasons');
    const nav = painted.fields.nav!.value as readonly (readonly unknown[])[];
    assert.equal(nav[0]![6], 0, 'the first row sits at y = 0 directly under the column headers');
    const desc = painted.fields.accessibleDescription?.value;
    assert.equal(typeof desc, 'string', 'accessible description is published for the partial summary');
    assert.ok((desc as string).includes('3 not shown') && (desc as string).includes('Leader A: condition undecided'));
    const audit = painted.fields.audit!.value as { id: number; rows: readonly { key: string; value?: string }[] };
    const metaAudit = audit.rows.find(row => row.key === '__table__:meta')?.value ?? '';
    assert.ok(metaAudit.includes('3 not shown') && metaAudit.includes('Leader A: condition undecided'), 'audit records the header marker and reasons');
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }

  const occPartial = occupiedStatesProjection.project(
    createBoundedReadView({
      hour: 50,
      panels: new Map([['main', {
        hour: 50,
        complete: false,
        missing: null,
        strengthMissing: 'Not recorded',
        garrisonRedRaw: null,
        laws: [],
        states: [{
          state: 1,
          name: 'Alsace',
          nameMissing: null,
          owner: 'FRA',
          occupier: 'GER',
          occupied: 'FRA',
          law: null,
          lawMissing: 'Not recorded',
          garrisonRequiredSign: null,
          assignedKnown: false,
          garrisonMissing: 'Not recorded',
          garrisonAlert: false,
        }],
        warnings: [],
      }]]),
    }),
    { filter: { passing: null, unrecorded: [], bounds: [] }, role: 'occupier', lawAs: 'dot', filterAll: true },
  );
  assert.equal(occPartial.titleSummary, 'Partial');
  assert.equal(occPartial.titleSummaryPartial, 'Some occupied states are missing.');
  assert.equal('notes' in occPartial, false);
});

test('buildDeclarativeRow resolves country identity and flag art once per row across numeric columns and rowName', () => {
  type ScoreRow = { readonly tag: string; readonly army: number; readonly navy: number; readonly air: number; readonly industry: number; readonly total: number };
  const proj = declareTableProjection<{ readonly rows: readonly ScoreRow[] }, ScoreRow, Record<string, never>>({
    kind: 'test-declarative-identity-reuse',
    validate: raw => raw as { readonly rows: readonly ScoreRow[] },
    columns: () => [
      { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, countryField: 'tag' },
      { key: 'army', label: 'Army', as: 'faint', offered: ['faint'], priority: 1, numeric: true, tone: 'country', country: 'tag', field: 'army' },
      { key: 'navy', label: 'Navy', as: 'faint', offered: ['faint'], priority: 2, numeric: true, tone: 'country', country: 'tag', field: 'navy' },
      { key: 'air', label: 'Air', as: 'faint', offered: ['faint'], priority: 3, numeric: true, tone: 'country', country: 'tag', field: 'air' },
      { key: 'industry', label: 'Industry', as: 'faint', offered: ['faint'], priority: 4, numeric: true, tone: 'country', country: 'tag', field: 'industry' },
      { key: 'total', label: 'Total', as: 'faint', offered: ['faint'], priority: 5, numeric: true, tone: 'country', country: 'tag', field: 'total' },
    ],
    rows: reply => ({ rows: reply.rows }),
    rowKey: r => r.tag,
    rowName: (r, hour, _o, who) => who(r.tag, hour).name,
  });
  let whoCalls = 0;
  let artCalls = 0;
  const view = createBoundedReadView({
    hour: 100,
    panels: new Map([['main', { rows: [{ tag: 'GER', army: 10, navy: 20, air: 30, industry: 40, total: 100 }] }]]),
    identity: {
      resolve(kind, key, atHour) {
        whoCalls++;
        return {
          kind,
          key,
          atHour,
          holder: { status: 'ready', value: key },
          tag: { status: 'ready', value: key },
          name: { status: 'ready', value: 'Germany' },
          colour: { status: 'ready', value: '#4a4a4a' },
          flagChoice: { status: 'ready', value: '["GER",null,null]' },
          validInterval: [0, 500],
          nextChangeAt: 501,
        };
      },
    },
    art: {
      resolve() {
        artCalls++;
        return { status: 'pending' };
      },
    },
  });
  const model = proj.project(view, {});
  assert.equal(model.rows.length, 1);
  assert.equal(model.rows[0]!.name, 'Germany');
  assert.equal(model.rows[0]!.cells.total!.countryColor, '#4a4a4a');
  assert.equal(whoCalls, 1, 'identity is resolved once per row at the same hour');
  assert.equal(artCalls, 1, 'flag art is resolved only for the country column, never numeric columns or rowName');
});

test('a tall row near the window edge keeps its neighbours on screen while scrolling a paused table, and a row whose page comes back as loading keeps its place and its height: the host window and the lane place rows from measured heights', () => {
  const columns: TableColumnSpec[] = [
    { key: 'date', label: 'Date', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'date', minWidth: 70, maxWidth: 70 },
    { key: 'item', label: 'Item', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'item', direction: 'descending' } });
  const plain = (key: string): TableRow => ({ key, name: key, cells: { date: { value: knownValue(10) }, item: { value: knownValue(key) } } });
  const chips = Array.from({ length: 60 }, (_, at) => ({ tag: 'AAA', art: readyArt(`art://${at}`), label: `Province ${at}` }));
  const group = (key: string): TableRow => ({ ...plain(key), pageGroup: { members: 1, open: true } });
  const terms = (key: string, parent: string): TableRow => ({ key, name: key, pageParent: parent, detailColumn: 'item', detailEnd: true,
    cells: { date: { value: knownValue(10) }, item: { value: knownValue(key), flags: chips } } });
  const all: TableRow[] = [
    plain('r0'), group('group'), terms('terms', 'group'),
    ...Array.from({ length: 10 }, (_, at) => plain(`r${at + 3}`)),
    group('group2'), terms('terms2', 'group2'),
    ...Array.from({ length: 30 }, (_, at) => plain(`r${at + 15}`)),
  ];
  const second = 14;
  const height = 200;
  const width = 640;
  const binding = (scrollTop: number) => ({ family: 'table', settings, width, height, scrollLeft: 0, scrollTop, options: NO_OPTIONS }) as never;
  const lay = (slots: TableModelSlot[], windowOffset: number, scrollTop: number, measure: object) =>
    tableLane.layout({ columns, rows: slots.flatMap(slot => slot.kind === 'data' ? [slot.row] : []), slots, hour: 10, unsorted: true, windowOffset, totalRows: all.length },
      binding(scrollTop), { width, height, dpr: 1 } as never, TEST_THEME, measure as OffscreenCanvasRenderingContext2D, 10, { previous: null, now: 0, speed: 0 } as never);
  const reference = lay(all.map(row => ({ kind: 'data' as const, row })), 0, 0, instrumentedContext().ctx);
  for (const ordinal of [2, second]) {
    assert.ok(reference.heights(ordinal) > ROW_HEIGHT.normal * 3, `the detail row ${ordinal} wraps its terms to several lines (${reference.heights(ordinal)}px)`);
  }
  const hostWindow = (scrollTop: number, fields: Readonly<Record<string, unknown>>) =>
    tableHostAdapter.rowWindow!({ width, height, dpr: 1, scrollLeft: 0, scrollTop, zoomWindow: null }, settings, fields)!;
  const fieldsOf = (layout: typeof reference, scrollTop: number) => tableLane.structure({ layout, binding: binding(scrollTop) } as never);
  const measure = instrumentedContext().ctx;
  const shown = (window: { first: number; count: number }, scrollTop: number, loading: number | null) => lay(all.slice(window.first, window.first + window.count)
    .map((row, at) => window.first + at === loading ? { kind: 'loading' as const, key: `loading:${loading}` } : { kind: 'data' as const, row }), window.first, scrollTop, measure);
  let layout = shown(hostWindow(0, {}), 0, null);
  const visit = (scrollTop: number, loading: number | null = null) => {
    const window = hostWindow(scrollTop, fieldsOf(layout, scrollTop));
    layout = shown(window, scrollTop, loading);
    const settled = hostWindow(scrollTop, fieldsOf(layout, scrollTop));
    if (settled.first !== window.first || settled.count !== window.count) layout = shown(settled, scrollTop, loading);
    return window;
  };
  const placed = (scrollTop: number, through: number) => {
    const offset = layout.shape.windowOffset;
    for (let index = 0; offset + index <= through && index < layout.shape.flatRows.length; index++) {
      assert.equal(layout.tops(index), reference.tops(offset + index), `row ${offset + index} keeps its place at ${scrollTop}px when the window starts at ${offset}`);
    }
  };
  let extent = 0;
  for (let scrollTop = 0; scrollTop <= reference.tops(second) + 40; scrollTop += 24) {
    const window = visit(scrollTop);
    for (let ordinal = 0; ordinal < all.length; ordinal++) {
      const onScreen = reference.tops(ordinal) < scrollTop + height && reference.tops(ordinal) + reference.heights(ordinal) > scrollTop;
      if (onScreen) assert.ok(ordinal >= window.first && ordinal < window.first + window.count, `row ${ordinal} is on screen at ${scrollTop}px and inside the window ${window.first}+${window.count}`);
    }
    placed(scrollTop, all.length);
    assert.ok(layout.shape.totalHeight >= extent, `the scroll extent never shrinks: ${layout.shape.totalHeight}px after ${extent}px at ${scrollTop}px`);
    extent = layout.shape.totalHeight;
  }
  assert.equal(extent, reference.shape.totalHeight, 'the scroll extent is whole once every row has been seen');
  const back = reference.tops(second - 1);
  const past = reference.tops(second + 6);
  for (const [scrollTop, loading] of [[back, second], [past, second], [back, null]] as const) {
    visit(scrollTop, loading);
    assert.ok(layout.shape.windowOffset > (scrollTop === past ? second : 2), `the tall rows before ${scrollTop === past ? 'and including the loading one ' : ''}are above the window that starts at ${layout.shape.windowOffset}`);
    placed(scrollTop, all.length);
    assert.equal(layout.shape.totalHeight, reference.shape.totalHeight, `the scroll extent stays whole at ${scrollTop}px with row ${loading} loading`);
  }
  assert.ok(layout.shape.windowOffset < second && second < layout.shape.windowOffset + layout.shape.flatRows.length, 'the page came back into the window');
});

test('keepsCanvas skips unchanged paints for bottom-clipped rows, unquantized behind-bar changes, cloned settings, and off-screen art settles while repainting every visible change', () => {
  const columns: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, pinned: true, text: true },
    { key: 'factories', label: 'Factories', as: 'faint', offered: ['faint'], priority: 1, numeric: true, barEligible: true, format: { decimals: 0 } },
    { key: 'army', label: 'Army', as: 'faint', offered: ['faint'], priority: 2, numeric: true, barEligible: true, format: { decimals: 0 } },
  ];
  const makeSettings = (barsBehind: 'all' | 'sorted' | 'off' = 'all') =>
    tableSettingsOf(undefined, columns, { density: 'normal', barsBehind, sort: { column: 'factories', direction: 'descending' } });
  const flag0 = readyArt('art://flag-0');
  const flag1 = readyArt('art://flag-1');
  const offscreenUrl = 'art://flag-offscreen';
  const makeRows = (r0Army: number, r1Factories: number, r1Army: number, r2Army: number): TableRow[] => [
    {
      key: 'AAA', name: 'Alpha',
      cells: {
        country: { value: knownValue('Alpha'), country: 'AAA', iconSlot: true, art: flag0 },
        factories: { value: knownValue(300), country: 'AAA' },
        army: { value: knownValue(r0Army), country: 'AAA' },
      },
    },
    {
      key: 'BBB', name: 'Beta',
      cells: {
        country: { value: knownValue('Beta'), country: 'BBB', iconSlot: true, art: flag1 },
        factories: { value: knownValue(r1Factories), country: 'BBB' },
        army: { value: knownValue(r1Army), country: 'BBB' },
      },
    },
    {
      key: 'CCC', name: 'Gamma',
      cells: {
        country: { value: knownValue('Gamma'), country: 'CCC', iconSlot: true, art: readyArt(offscreenUrl) },
        factories: { value: knownValue(100), country: 'CCC' },
        army: { value: knownValue(r2Army), country: 'CCC' },
      },
    },
  ];

  const pendingResolvers = new Map<string, (bmp: { width: number; height: number; close(): void }) => void>();
  const artCache = new LaneArtCache<{ width: number; height: number; close(): void }>({
    fetchBitmap: url => new Promise(resolve => { pendingResolvers.set(url, resolve); }),
  });
  const ctx = { ...instrumentedContext().ctx, canvas: { width: 354, height: 40 } } as unknown as OffscreenCanvasRenderingContext2D;
  const surface = { width: 354, height: 40, dpr: 1 };
  const makeFrame = (
    rows: TableRow[],
    hour: number,
    settings: TableSettings,
    height: number,
    previous: { layout: ReturnType<typeof tableLane.layout>; model: TableModel } | null,
    selectedKey: string | null = null,
  ) => {
    const model: TableModel = { columns, rows, hour, rowTimeBoundaries: false, rank: true };
    const binding = {
      family: 'table' as const,
      kind: 'scoreboard',
      settings,
      width: 354,
      height,
      scrollLeft: 0,
      scrollTop: 0,
      options: NO_OPTIONS,
      selectedKey,
      focusedKey: null,
      reducedMotion: true,
    };
    const surf = { ...surface, height };
    const layout = tableLane.layout(model, binding as never, surf as never, TEST_THEME, ctx, hour, { previous: previous as never, now: hour * 100, speed: 24 } as never);
    return { model, layout, binding, frame: { model, layout, binding, surface: surf, theme: TEST_THEME, art: artCache, now: hour * 100, speed: 24, inspect: null, continuity: 1, changedAt: hour * 100, before: previous } };
  };

  const s1 = makeSettings('all');
  const f1 = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 10, s1, 40, null);
  tableLane.paint(ctx, f1.frame as never);

  const f2 = makeFrame(makeRows(1_000_000, 295, 3_059_000, 3_198_000), 11, makeSettings('all'), 40, { layout: f1.layout, model: f1.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, f2.frame as never),
    true,
    'bottom-clipped row 1 value changes, sub-pixel behind-bar max shift on row 0, and cloned identical settings keep the canvas',
  );

  const f3 = makeFrame(makeRows(1_000_000, 295, 3_059_000, 6_400_000), 12, makeSettings('all'), 40, { layout: f1.layout, model: f1.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, f3.frame as never),
    false,
    'a column maximum shift that changes row 0 behind-bar width by whole CSS pixels forces a repaint',
  );

  const f3Off = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 13, makeSettings('off'), 40, null);
  tableLane.paint(ctx, f3Off.frame as never);
  const f3OffNext = makeFrame(makeRows(1_000_000, 295, 3_059_000, 6_400_000), 14, makeSettings('off'), 40, { layout: f3Off.layout, model: f3Off.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, f3OffNext.frame as never),
    true,
    'with barsBehind off, an off-screen maximum shift cannot change row 0 pixels',
  );

  const fTall = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 15, s1, 56, null);
  tableLane.paint(ctx, fTall.frame as never);
  const fTallNext = makeFrame(makeRows(1_000_000, 295, 3_060_000, 3_200_000), 16, s1, 56, { layout: fTall.layout, model: fTall.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, fTallNext.frame as never),
    false,
    'once height exposes row 1 ink, a change on row 1 forces a repaint',
  );

  const narrowCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, pinned: true, text: true, minWidth: 160, maxWidth: 160 },
    { key: 'factories', label: 'Factories', as: 'faint', offered: ['faint'], priority: 1, numeric: true, barEligible: true, format: { decimals: 0 }, minWidth: 90, maxWidth: 90 },
    { key: 'army', label: 'Army', as: 'faint', offered: ['faint'], priority: 2, numeric: true, barEligible: true, format: { decimals: 0 }, minWidth: 150, maxWidth: 150 },
  ];
  const narrowSettings = tableSettingsOf(undefined, narrowCols, { density: 'normal', barsBehind: 'all', sort: { column: 'factories', direction: 'descending' } });
  const makeNarrowFrame = (r0Army: number, hour: number, previous: { layout: ReturnType<typeof tableLane.layout>; model: TableModel } | null) => {
    const model: TableModel = { columns: narrowCols, rows: makeRows(r0Army, 294, 3_060_000, 3_200_000), hour, rowTimeBoundaries: false, rank: true };
    const binding = { family: 'table' as const, kind: 'scoreboard', settings: narrowSettings, width: 354, height: 40, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS, selectedKey: null, focusedKey: null, reducedMotion: true };
    const surf = { width: 354, height: 40, dpr: 1 };
    const layout = tableLane.layout(model, binding as never, surf as never, TEST_THEME, ctx, hour, { previous: previous as never, now: hour * 100, speed: 24 } as never);
    return { model, layout, binding, frame: { model, layout, binding, surface: surf, theme: TEST_THEME, art: artCache, now: hour * 100, speed: 24, inspect: null, continuity: 1, changedAt: hour * 100, before: previous } };
  };
  const fn1 = makeNarrowFrame(3_094_059, 20, null);
  tableLane.paint(ctx, fn1.frame as never);
  const fn2 = makeNarrowFrame(3_094_874, 21, { layout: fn1.layout, model: fn1.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, fn2.frame as never),
    true,
    'trailing digits clipped past the right viewport edge keep the canvas when behind-bar width is unchanged',
  );
  const fn3 = makeNarrowFrame(3_194_059, 22, { layout: fn1.layout, model: fn1.model });
  assert.equal(
    tableLane.keepsCanvas!(ctx, fn3.frame as never),
    false,
    'a visible leading digit change in a right-clipped column forces a repaint',
  );

  const fArt = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 17, s1, 40, null);
  artCache.requestVisible(['art://flag-0', offscreenUrl], []);
  tableLane.paint(ctx, fArt.frame as never);
  pendingResolvers.get(offscreenUrl)?.({ width: 20, height: 13, close() {} });
  return Promise.resolve().then(() => {
    const fAfterOffscreen = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 18, s1, 40, { layout: fArt.layout, model: fArt.model });
    assert.equal(
      tableLane.keepsCanvas!(ctx, fAfterOffscreen.frame as never),
      true,
      'an off-screen art settle leaves a table canvas untouched when visible art states did not change',
    );
    pendingResolvers.get('art://flag-0')?.({ width: 20, height: 13, close() {} });
    return Promise.resolve().then(() => {
      const fAfterVisible = makeFrame(makeRows(1_000_000, 294, 3_060_000, 3_200_000), 19, s1, 40, { layout: fArt.layout, model: fArt.model });
      assert.equal(
        tableLane.keepsCanvas!(ctx, fAfterVisible.frame as never),
        false,
        'a visible row art settle forces a repaint',
      );

      const inlineCols: TableColumnSpec[] = [
        { key: 'division', label: 'Division', as: 'plain', offered: ['plain'], priority: 0, text: true, pinned: true, narrow: 'primary', minWidth: 160 },
        { key: 'owner', label: 'Owner', as: 'flagName', offered: ['flagName'], priority: 1, text: true, narrow: 'flag', minWidth: 120 },
        { key: 'metric', label: 'Metric', as: 'plain', offered: ['plain'], priority: 2, numeric: true, minWidth: 120 },
      ];
      const inlineSettings = tableSettingsOf(undefined, inlineCols, { density: 'normal', barsBehind: 'off', sort: { column: 'metric', direction: 'descending' } });
      const inlineUrl = 'art://inline-flag';
      const inlineRows: TableRow[] = [{
        key: 'div-1',
        name: '1st Infantry',
        cells: {
          division: { value: knownValue('1st Infantry') },
          owner: { value: knownValue('Alpha'), iconSlot: true, country: 'AAA', art: readyArt(inlineUrl) },
          metric: { value: knownValue(5) },
        },
      }];
      const makeInlineFrame = (hour: number, previous: { layout: ReturnType<typeof tableLane.layout>; model: TableModel } | null) => {
        const model: TableModel = { columns: inlineCols, rows: inlineRows, hour, rowTimeBoundaries: false };
        const binding = { family: 'table' as const, kind: 'division-log', settings: inlineSettings, width: 280, height: 40, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS, selectedKey: null, focusedKey: null, reducedMotion: true };
        const surf = { width: 280, height: 40, dpr: 1 };
        const layout = tableLane.layout(model, binding as never, surf as never, TEST_THEME, ctx, hour, { previous: previous as never, now: hour * 100, speed: 24 } as never);
        return { model, layout, binding, frame: { model, layout, binding, surface: surf, theme: TEST_THEME, art: artCache, now: hour * 100, speed: 24, inspect: null, continuity: 1, changedAt: hour * 100, before: previous } };
      };
      const fi1 = makeInlineFrame(30, null);
      assert.ok(fi1.layout.shape.columns.columns.find(c => c.key === 'division')?.inlineFlags?.includes('owner'));
      assert.equal(fi1.layout.shape.columns.columns.find(c => c.key === 'owner')?.width, 0);
      artCache.requestVisible([inlineUrl], []);
      tableLane.paint(ctx, fi1.frame as never);
      pendingResolvers.get(inlineUrl)?.({ width: 20, height: 13, close() {} });
      return Promise.resolve().then(() => {
        const fi2 = makeInlineFrame(31, { layout: fi1.layout, model: fi1.model });
        assert.equal(
          tableLane.keepsCanvas!(ctx, fi2.frame as never),
          false,
          'an inlineFlags flag settle from a zero-width column forces a repaint',
        );

        const makeDeepFrame = (val: number, hour: number, previous: { layout: ReturnType<typeof tableLane.layout>; model: TableModel } | null) => {
          const rows: TableRow[] = [{ key: 'r10', name: 'Row 10', cells: { country: { value: knownValue('Row 10') }, factories: { value: knownValue(val) }, army: { value: knownValue(100) } } }];
          const model: TableModel = { columns, rows, hour, rowTimeBoundaries: false, windowOffset: 10, totalRows: 20 };
          const binding = { family: 'table' as const, kind: 'scoreboard', settings: s1, width: 354, height: 40, scrollLeft: 0, scrollTop: 10 * ROW_HEIGHT.normal, options: NO_OPTIONS, selectedKey: null, focusedKey: null, reducedMotion: true };
          const surf = { width: 354, height: 40, dpr: 1 };
          const layout = tableLane.layout(model, binding as never, surf as never, TEST_THEME, ctx, hour, { previous: previous as never, now: hour * 100, speed: 24 } as never);
          return { model, layout, binding, frame: { model, layout, binding, surface: surf, theme: TEST_THEME, art: artCache, now: hour * 100, speed: 24, inspect: null, continuity: 1, changedAt: hour * 100, before: previous } };
        };
        const fd1 = makeDeepFrame(10, 40, null);
        tableLane.paint(ctx, fd1.frame as never);
        const fd2 = makeDeepFrame(20, 41, { layout: fd1.layout, model: fd1.model });
        assert.equal(
          tableLane.keepsCanvas!(ctx, fd2.frame as never),
          false,
          'a visible row in a paged window with nonzero windowOffset repaints on value change',
        );
        artCache.dispose();
      });
    });
  });
});

test('declareTableProjection incremental cache keeps unchanged rows across ticks and invalidates when country identity changes', () => {
  type LiveRow = { readonly tag: string };
  const proj = declareTableProjection<{ readonly rows: readonly LiveRow[] }, LiveRow, { readonly live: boolean }>({
    kind: 'test-decl-identity-snapshot',
    columns: [
      { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, countryField: 'tag' },
      { key: 'score', label: 'Score', as: 'faint', offered: ['faint'], priority: 1, numeric: true, liveField: 'score' },
    ],
    rows: reply => ({ rows: reply.rows }),
    rowKey: r => r.tag,
    rowName: (r, hour, _o, who) => who(r.tag, hour).name,
    incrementalLive: () => true,
  });
  const reply = { rows: [{ tag: 'AAA' }, { tag: 'BBB' }] };
  const options = { live: true };
  let countryName = 'Alpha';
  const live = new Map<string, number>([['AAA\u0000score', 10], ['BBB\u0000score', 20]]);
  const makeView = (hour: number) => createBoundedReadView({
    hour,
    panels: new Map([['main', reply]]),
    live: new Map([['main', live as never]]),
    identity: {
      resolve(kind, key, atHour) {
        return {
          kind, key, atHour,
          holder: { status: 'ready', value: key },
          tag: { status: 'ready', value: key },
          name: { status: 'ready', value: key === 'AAA' ? countryName : 'Beta' },
          colour: { status: 'ready', value: '#4a4a4a' },
          flagChoice: { status: 'ready', value: `["${key}",null,null]` },
          validInterval: [0, 500],
          nextChangeAt: 501,
        };
      },
    },
    art: { resolve: handle => readyArt(`art://${handle.kind === 'flag' ? handle.choice : 'x'}`) },
  });

  const m1 = proj.project(makeView(10), options);
  const m2 = proj.project(makeView(11), options);
  assert.equal(m2.rows[0], m1.rows[0], 'unchanged row stays reused on incremental live tick');
  assert.equal(m2.rows[1], m1.rows[1], 'second unchanged row stays reused on incremental live tick');
  assert.equal(m2.changedKeys?.size, 0);

  countryName = 'Alpha Renamed';
  const m3 = proj.project(makeView(12), options);
  assert.equal(m3.rows[0]!.name, 'Alpha Renamed', 'country identity change invalidates the incremental snapshot');
});

test('flag columns widen for multi-country coalitions past row 40, preserve two-flag narrow columns, and reserve inline subtext beside missing or long names in compact density', () => {
  const columns: TableColumnSpec[] = [
    { key: 'country', label: '', as: 'flag', cellKind: 'country', offered: ['flagName', 'name', 'flag'], priority: 0, minWidth: 52, grow: 0 },
    { key: 'name', label: 'Port', as: 'plain', cellKind: 'item', offered: ['plain'], priority: 0, text: true, minWidth: 160, maxWidth: 220 },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'compact', sort: { column: 'name', direction: 'ascending' } });
  const baseCols = buildColumnLayouts(columns, settings);
  const twoWidths = measureFixedColumnWidths(baseCols, settings, TEST_THEME, text => text.length * 7, undefined, { flags: { country: 2 } });
  const fourWidths = measureFixedColumnWidths(baseCols, settings, TEST_THEME, text => text.length * 7, undefined, { flags: { country: 4 } });
  assert.equal(twoWidths.country, 52);
  assert.ok(fourWidths.country! > twoWidths.country!, 'four coalition flags widen a flag-only column');
  const missingFlag = productArt({ status: 'confirmedMissing', reason: 'missing' });
  const model: TableModel = {
    hour: 1,
    rhythm: 'log',
    columns,
    rows: [
      {
        key: 'unnamed',
        name: 'Port name unavailable',
        cells: {
          country: {
            value: knownValue('Alpha, Charlie · Bravo, Delta'),
            flags: [
              { tag: 'AA', art: missingFlag },
              { tag: 'CC', art: missingFlag },
              { tag: 'BB', art: missingFlag, separator: true },
              { tag: 'DD', art: missingFlag },
            ],
          },
          name: { value: notRecorded('Port name unavailable'), inlineSubtext: 'No ships waiting' },
        },
      },
      {
        key: 'long-named',
        name: 'Very Long Naval Repair Anchorage Name Filling The Column',
        cells: {
          country: { value: knownValue('Alpha · Bravo'), flags: [{ tag: 'AA', art: missingFlag }, { tag: 'BB', art: missingFlag }] },
          name: { value: knownValue('Very Long Naval Repair Anchorage Name Filling The Column'), inlineSubtext: 'No ships waiting' },
        },
      },
    ],
  };
  const layout = layoutFamilyTable(buildColumnLayouts(columns, settings, twoWidths), 272, { rhythm: 'log' });
  const twoFlagGrid = gridContext(272, 120);
  drawFamilyTable(twoFlagGrid.ctx as unknown as CanvasRenderingContext2D, {
    layout,
    rows: resolveTableRows({ ...model, rows: [model.rows[1]!] }, settings),
    settings,
    hour: 1,
    width: 272,
    height: 120,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 1,
    glideDuration: 0,
    glide: NO_GLIDE,
    markers: new Map(),
    dpr: 1,
    rhythm: 'log',
  });
  const texts = [...twoFlagGrid.marks.values()];
  assert.ok(texts.includes('AA') && texts.includes('BB'), 'a 52px two-flag column still draws both flags');
  assert.ok(texts.includes('No ships waiting'), 'inline status remains visible beside a long recorded port name in compact density');

  const wideLayout = layoutFamilyTable(buildColumnLayouts(columns, settings, fourWidths), 400);
  const wideGrid = gridContext(400, 120);
  drawFamilyTable(wideGrid.ctx as unknown as CanvasRenderingContext2D, {
    layout: wideLayout,
    rows: resolveTableRows(model, settings),
    settings,
    hour: 1,
    width: 400,
    height: 120,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 1,
    glideDuration: 0,
    glide: NO_GLIDE,
    markers: new Map(),
    dpr: 1,
  });
  const wideTexts = [...wideGrid.marks.values()];
  assert.ok(['AA', 'CC', 'BB', 'DD'].every(code => wideTexts.includes(code)), 'all four coalition flags paint in the widened flag column');
  assert.equal(wideTexts.filter(text => text === 'No ships waiting').length, 2, 'inline status paints beside both a missing-name hatch and a long named row');

  const resizedLayout = layoutFamilyTable(buildColumnLayouts(columns, settings, { country: 68, name: 220 }), 300, { rhythm: 'log' });
  const resizedGrid = gridContext(300, 120);
  drawFamilyTable(resizedGrid.ctx as unknown as CanvasRenderingContext2D, {
    layout: resizedLayout,
    rows: resolveTableRows({ ...model, rows: [model.rows[0]!] }, settings),
    settings,
    hour: 1,
    width: 300,
    height: 120,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 1,
    glideDuration: 0,
    glide: NO_GLIDE,
    markers: new Map(),
    dpr: 1,
    rhythm: 'log',
  });
  const resizedTexts = [...resizedGrid.marks.values()];
  assert.ok(resizedTexts.includes('AA') && resizedTexts.includes('CC'), 'leading coalition flags fit inside a resized 68px column');
  assert.equal(resizedTexts.includes('BB') || resizedTexts.includes('DD'), false, 'coalition flags stop at the resized column edge instead of overlapping the next column');

  const namedSettings = tableSettingsOf(undefined, columns, { density: 'compact', sort: { column: 'name', direction: 'ascending' } });
  namedSettings.columns.country = { ...namedSettings.columns.country!, as: 'flagName', showNames: true };
  const resizedNamedLayout = layoutFamilyTable(buildColumnLayouts(columns, namedSettings, { country: 68, name: 220 }), 300, { rhythm: 'log' });
  const resizedNamedGrid = gridContext(300, 120);
  drawFamilyTable(resizedNamedGrid.ctx as unknown as CanvasRenderingContext2D, {
    layout: resizedNamedLayout,
    rows: resolveTableRows({ ...model, rows: [model.rows[0]!] }, namedSettings),
    settings: namedSettings,
    hour: 1,
    width: 300,
    height: 120,
    scrollLeft: 0,
    scrollTop: 0,
    rankShown: false,
    rankWidth: 0,
    theme: TEST_THEME,
    now: 1,
    glideDuration: 0,
    glide: NO_GLIDE,
    markers: new Map(),
    dpr: 1,
    rhythm: 'log',
  });
  const resizedNamedTexts = [...resizedNamedGrid.marks.values()];
  assert.ok(resizedNamedTexts.includes('AA') && resizedNamedTexts.includes('CC'), 'leading coalition flags fit inside a resized 68px flagName column');
  assert.equal(resizedNamedTexts.includes('BB') || resizedNamedTexts.includes('DD'), false, 'coalition flags stop at the resized flagName column edge');

  const rows45 = Array.from({ length: 45 }, (_, idx) => ({
    key: `r${idx}`,
    name: `Battle ${idx}`,
    cells: {
      country: {
        value: knownValue('Alpha · Bravo'),
        flags: idx === 42
          ? [{ tag: 'AAA', art: PENDING_ART }, { tag: 'CCC', art: PENDING_ART }, { tag: 'BBB', art: PENDING_ART, separator: true }]
          : [{ tag: 'AAA', art: PENDING_ART }, { tag: 'BBB', art: PENDING_ART }],
      },
      name: { value: knownValue(`Battle ${idx}`) },
    },
  }));
  const lateKind = 'test-late-coalition-flags';
  registerFamilyProjection({
    kind: lateKind,
    family: 'table',
    theme: TEST_THEME,
    project: readView => ({ hour: readView.hour, rhythm: 'log', columns, rows: rows45 }),
  });
  const prevOffscreen = globalThis.OffscreenCanvas;
  (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return gridContext(this.width, this.height).ctx; }
    transferToImageBitmap() { return { close() {} }; }
  };
  try {
    const view = createLaneView(tableLane, () => {});
    const state = new Map<string, unknown>();
    const transport = new LaneTransport();
    const painted = view.paint({
      widget: 1, subs: [], pictures: [],
      model: { family: 'table' as const, kind: lateKind, options: transport.carry({}), settings, sources: {}, staticPanels: { main: { at: 1 } }, width: 480, height: 200, scrollLeft: 0, scrollTop: 0, theme: TEST_THEME, reducedMotion: true },
      hour: 1, seek: 0, speed: 0, binding: { generation: 1, surfaces: [{ width: 480, height: 200, dpr: 1 }], offset: 0, pin: null }, inspect: null, now: 1,
    }, state, () => 'c1');
    const twoFlagLaidOut = layout.columns.find(col => col.key === 'country')!.width;
    const countryW = (painted.fields.widths?.value as { widths: Record<string, number> }).widths.country!;
    assert.ok(countryW > twoFlagLaidOut, `row 42 coalition widens the country column beyond the two-flag laid-out width ${twoFlagLaidOut}px (got ${countryW}px)`);
  } finally {
    (globalThis as unknown as { OffscreenCanvas: unknown }).OffscreenCanvas = prevOffscreen;
  }
});

test('deriveTableAudit records tooltip countryFlagKey across pending and ready states and detects flag identity changes in Aces', async () => {
  const { acesProjection } = await import('../../widgets/aces/acesProjection.ts');
  const { TABLE_BASE: ACES_BASE } = await import('../../widgets/aces/definition.ts');
  const { createSnapshotIdentityProvider } = await import('../identity/provider.ts');
  const hour = 61320100;
  const reply = {
    readHour: hour,
    missing: null,
    warnings: [],
    aces: [{
      id: '1',
      tag: 'GER',
      name: 'Ace Pilot',
      callsign: 'Falcon',
      portrait: null,
      portraitMissing: 'Not recorded',
      wing: null,
      wingName: null,
      aircraft: [],
      firstSeen: hour - 50,
      alive: true,
      death: null,
    }],
  };
  const auditAce = (flagKey: string, flagArt: ArtResult) => {
    const index = new IdentityIndex({ sourceId: 's', revision: '1', branchId: 'b1', content: 1 }, [hour - 100, hour + 100]);
    index.ingestStyle({
      tag: 'GER',
      name: 'Germany',
      color: [74, 74, 74],
      valid: [hour - 100, hour + 100],
      flagChoice: { keys: [flagKey, null, null], reasons: [null, null, null] },
    }, hour);
    const rv = createBoundedReadView({
      hour,
      panels: new Map([['main', reply]]),
      identity: createSnapshotIdentityProvider(index),
      art: { resolve: () => flagArt },
    });
    const model = acesProjection.project(rv, { filter: { passing: null, unrecorded: [], bounds: [] } });
    const settings = tableSettingsOf(undefined, model.columns, ACES_BASE);
    const flatRows = resolveTableRows(model, settings);
    const visibleColumns = buildColumnLayouts(model.columns, settings);
    return deriveTableAudit({ model, flatRows, visibleColumns }).find(entry => entry.key === '1:_tooltip')!.value!;
  };
  const pendingA = auditAce('GER_fascism', PENDING_ART);
  const readyA = auditAce('GER_fascism', readyArt('flag://GER_fascism'));
  const readyB = auditAce('GER_democratic', readyArt('flag://GER_democratic'));
  assert.equal(pendingA, readyA, 'tooltip audit stays invariant while the same flag loads');
  assert.notEqual(readyA, readyB, 'tooltip audit detects a changed country flag identity in Aces');
  assert.ok(readyA.includes('GER_fascism'), 'tooltip audit names the flag handle key');
  assert.ok(!readyA.includes('"countryFlagKey":null'), 'Aces carries the flag handle key onto its country cell');

  const { warContributionSidesProjection } = await import('../../widgets/war-contribution/sidesProjection.ts');
  const warsReply = {
    branchId: 'b1',
    available: true,
    viewpoints: [{ firstHour: hour - 50, lastHour: hour + 50, tag: 'GER' }],
    warnings: [],
    unobserved: [],
    capitulationsUnobserved: [],
    countryNames: [],
    wars: [{
      id: 'w1',
      firstHour: hour - 50,
      lastHour: hour + 50,
      startedBy: false,
      endedBy: false,
      observations: [{
        firstHour: hour - 50,
        lastHour: hour + 50,
        viewpointTag: 'GER',
        viewpointSide: ['GER'],
        opposingSide: ['FRA'],
        attackersAreViewpoint: true,
        representative: ['GER', 'FRA'] as [string, string],
        name: { text: 'Test War', known: true, reason: null, key: null },
      }],
      participants: [
        { firstHour: hour - 50, lastHour: hour + 50, tag: 'GER', side: 'attacker' as const, joinedBy: false, leftBy: false },
        { firstHour: hour - 50, lastHour: hour + 50, tag: 'FRA', side: 'defender' as const, joinedBy: false, leftBy: false },
      ],
      capitulations: [],
    }],
  };
  const zeroStats = {
    equipmentDamage: '0', occupation: '0', airDamage: '0', strategicBombing: '0', shipDamage: '0',
    convoyDamage: '0', casualties: '0', unknownCasualties: '0', lendLeaseSent: '0', lendLeaseReceived: '0',
    score: '100', participation: '100',
  };
  const warReply = {
    branchId: 'b1',
    hour,
    available: true,
    missing: null,
    warnings: [],
    contributions: [{
      viewpointSide: ['GER'],
      opposingSide: ['FRA'],
      reason: null,
      participants: [
        { tag: 'GER', side: 'attacker' as const, ...zeroStats },
        { tag: 'FRA', side: 'defender' as const, ...zeroStats },
      ],
      measures: [{
        key: 'participation' as const,
        viewpoint: { total: '100', partial: false, shareOfWar: 5000, members: [{ tag: 'GER', value: '100', partial: false, shareOfSide: 10000 }] },
        opposing: { total: '100', partial: false, shareOfWar: 5000, members: [{ tag: 'FRA', value: '100', partial: false, shareOfSide: 10000 }] },
      }],
    }],
  };
  const auditSide = (flagKey: string, flagArt: ArtResult) => {
    const index = new IdentityIndex({ sourceId: 's', revision: '1', branchId: 'b1', content: 1 }, [hour - 100, hour + 100]);
    index.ingestStyle({ tag: 'GER', name: 'Germany', color: [74, 74, 74], valid: [hour - 100, hour + 100], flagChoice: { keys: [flagKey, null, null], reasons: [null, null, null] } }, hour);
    index.ingestStyle({ tag: 'FRA', name: 'France', color: [50, 80, 180], valid: [hour - 100, hour + 100], flagChoice: { keys: ['FRA_democratic', null, null], reasons: [null, null, null] } }, hour);
    const rv = createBoundedReadView({
      hour,
      panels: new Map([['live', warReply]]),
      identity: createSnapshotIdentityProvider(index),
      art: { resolve: () => flagArt },
    });
    const model = warContributionSidesProjection.project(rv, { wars: warsReply, saved: 'w1' });
    const pane = model.panes![0]!;
    const settings = tableSettingsOf(undefined, model.columns, { density: 'compact', barsBehind: 'off', sort: { column: 'participation', direction: 'descending' } });
    const flatRows: FlatTableRow[] = pane.rows.map((row, idx) => ({ kind: 'data', key: row.key, name: row.name, row, level: 1, rank: idx + 1, parent: null, childCount: 0, isTreeGroup: false, isOpen: false }));
    const visibleColumns = buildColumnLayouts(model.columns, settings);
    return deriveTableAudit({ model, flatRows, visibleColumns }).find(entry => entry.key === 'viewpoint:GER:_tooltip')!.value!;
  };
  const sideA = auditSide('GER_fascism', readyArt('flag://GER_fascism'));
  const sideB = auditSide('GER_democratic', readyArt('flag://GER_democratic'));
  assert.notEqual(sideA, sideB, 'War Contribution Sides tooltip audit detects a changed small flag choice');
  assert.ok(sideA.includes('GER_fascism') && !sideA.includes('"countryFlagKey":null'), 'War Contribution Sides carries the small flag handle key');

  const gsCell = (raw: string) => ({ raw, missing: null });
  const gsReply = {
    branchId: 'b1',
    hour,
    missing: null,
    hints: {},
    multipliers: [],
    rows: [{ tag: 'GER', total: gsCell('100'), army: gsCell('50'), navy: gsCell('25'), air: gsCell('25'), industry: gsCell('10'), land: gsCell('10') }],
    warnings: [],
  };
  const gsOpts = { all: true, membership: null, top: null, by: 'total' as const, factions: false, factionRuns: null, rankMarkers: false };
  const gsAudit = (flagKey: string) => {
    const idx = new IdentityIndex({ sourceId: 's-gs', revision: '1', branchId: 'b1', content: 1 }, [hour - 100, hour + 100]);
    idx.ingestStyle({ tag: 'GER', name: 'Germany', color: [74, 74, 74], valid: [hour - 100, hour + 100], flagChoice: { keys: [flagKey, null, null], reasons: [null, null, null] } }, hour);
    const rv = createBoundedReadView({ hour, panels: new Map([['main', gsReply]]), identity: createSnapshotIdentityProvider(idx), art: { resolve: () => PENDING_ART } });
    const model = gameScoreProjection.project(rv, gsOpts as never);
    return model.rows[0]!.cells.country!.artKey;
  };
  const gsKeyA = gsAudit('GER_fascism');
  const gsKeyB = gsAudit('GER_democratic');
  assert.notEqual(gsKeyA, gsKeyB, 'Game Score incremental cache updates country artKey when flagChoice changes while pending');

  const sbCols: ScoreboardColumn[] = [{ key: 'factories', label: 'Factories', unit: 'factories', decimals: 0, available: true, reason: null, family: { kind: 'f', measure: 'm' }, partial: null, noneAtZero: false }];
  const sbOpts = { shown: sbCols, candidates: ['GER'], top: 4, rankMarkers: false };
  const sbAnswer = answer({ keys: [1n], columns: 1, names: ['GER'], values: [100n], states: [CELL.value] });
  const sbAudit = (flagKey: string) => {
    const idx = new IdentityIndex({ sourceId: 's-sb', revision: '1', branchId: 'b1', content: 1 }, [hour - 100, hour + 100]);
    idx.ingestStyle({ tag: 'GER', name: 'Germany', color: [74, 74, 74], valid: [hour - 100, hour + 100], flagChoice: { keys: [flagKey, null, null], reasons: [null, null, null] } }, hour);
    const rv = createBoundedReadView({ hour, answers: new Map([[RANK_SOURCE, sbAnswer], ['factories', sbAnswer]]), identity: createSnapshotIdentityProvider(idx), art: { resolve: () => PENDING_ART } });
    const model = scoreboardProjection.project(rv, sbOpts);
    return model.rows[0]!.cells.country!.artKey;
  };
  const sbKeyA = sbAudit('GER_fascism');
  const sbKeyB = sbAudit('GER_democratic');
  assert.notEqual(sbKeyA, sbKeyB, 'Scoreboard incremental cache updates country artKey when flagChoice changes while pending');
});

test('tableLane.visibleArt follows frame-specific gliding rows and never pins all 1,500 flags on a rank reversal', async () => {
  const columns: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const makeModel = (scores: readonly number[], hour: number): TableModel => ({
    columns,
    hour,
    rows: scores.map((score, i) => ({
      key: `r${i}`,
      name: `Row ${i}`,
      cells: {
        country: { value: knownValue(`Row ${i}`), country: `C${i}`, iconSlot: true, art: readyArt(`flag://r${i}`), artKey: `flag:panel:r${i}` },
        score: { value: knownValue(score) },
      },
    })),
  });
  const surface = { width: 420, height: 110, dpr: 1 };
  const measure = instrumentedContext().ctx as unknown as OffscreenCanvasRenderingContext2D;

  const plainCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
  ];
  const plainSettings = tableSettingsOf(undefined, plainCols, { density: 'normal', sort: { column: 'country', direction: 'descending' } });
  const plainModel: TableModel = {
    columns: plainCols,
    hour: 100,
    rows: [{ key: 'plain-0', name: 'Plain', cells: { country: { value: knownValue('Plain'), country: 'GER', iconSlot: true, art: readyArt('flag://plain-hidden') } } }],
  };
  const plainBinding = { family: 'table', settings: plainSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const plainLayout = tableLane.layout(plainModel, plainBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 900, speed: 24 } as never);
  const plainUrls = [...tableLane.visibleArt({ model: plainModel, layout: plainLayout, binding: plainBinding, surface, theme: TEST_THEME, hour: 100, now: 900, changedAt: 900, continuity: 0, speed: 24, before: null } as never)];
  assert.deepEqual(plainUrls, [], 'plain top-level cells without artShape contribute zero icon URLs');

  const topBinding = { family: 'table', settings, width: 420, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const firstModel = makeModel([100, 90, 80, 70, 60, 50, 40, 30, 20, 10, 5, 1], 100);
  const firstLayout = tableLane.layout(firstModel, topBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 1000, speed: 24 } as never);
  const prevFrame = { model: firstModel, layout: firstLayout, binding: topBinding, surface, theme: TEST_THEME, hour: 100, now: 1000, changedAt: 1000, continuity: 1, speed: 24, before: null } as never;
  const movedModel = makeModel([5, 90, 80, 70, 60, 50, 40, 30, 20, 10, 1, 100], 101);
  const movedLayout = tableLane.layout(movedModel, topBinding, surface as never, TEST_THEME, measure, 101, { previous: prevFrame, now: 1020, speed: 24 } as never);
  const earlyFrame = { model: movedModel, layout: movedLayout, binding: topBinding, surface, theme: TEST_THEME, hour: 101, now: 1020, changedAt: 1020, continuity: 1, speed: 24, before: prevFrame } as never;
  const earlyUrls = [...tableLane.visibleArt(earlyFrame)];
  assert.ok(earlyUrls.includes('flag://r0'), 'at the start of its glide out of index 0, r0 is still inside the viewport and stays in visibleArt');
  assert.ok(!earlyUrls.includes('flag://r11'), 'at the start of its glide from index 11, r11 is still below the viewport and is not pinned early');

  const count = 1500;
  const midScroll = Math.floor(count / 2) * ROW_HEIGHT.normal;
  const midBinding = { family: 'table', settings, width: 420, height: 110, scrollLeft: 0, scrollTop: midScroll, options: NO_OPTIONS } as never;
  const asc = makeModel(Array.from({ length: count }, (_, i) => count - i), 100);
  const rev = makeModel(Array.from({ length: count }, (_, i) => i + 1), 101);
  const ascLayout = tableLane.layout(asc, midBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 2000, speed: 24 } as never);
  const ascFrame = { model: asc, layout: ascLayout, binding: midBinding, surface, theme: TEST_THEME, hour: 100, now: 2000, changedAt: 2000, continuity: 2, speed: 24, before: null } as never;
  const revLayout = tableLane.layout(rev, midBinding, surface as never, TEST_THEME, measure, 101, { previous: ascFrame, now: 2000, speed: 24 } as never);
  const revFrame = (now: number) => ({ model: rev, layout: revLayout, binding: midBinding, surface, theme: TEST_THEME, hour: 101, now, changedAt: 2000, continuity: 2, speed: 24, before: ascFrame } as never);
  const startUrls = [...tableLane.visibleArt(revFrame(2000))];
  const nearMidUrls = [...tableLane.visibleArt(revFrame(2089))];
  const midUrls = [...tableLane.visibleArt(revFrame(2090))];
  assert.ok(startUrls.length > 0 && startUrls.length <= 10, `at 0 ms a 1,500-row rank reversal pins only on-screen flags (got ${startUrls.length})`);
  assert.ok(nearMidUrls.length <= 40, `at 89 ms occluded rows do not flood visibleArt (got ${nearMidUrls.length})`);
  assert.ok(midUrls.length > 0 && midUrls.length <= 4, `at the 90 ms midpoint occluded rows are culled instead of pinning 1,500 flags (got ${midUrls.length})`);

  const portraitCols: TableColumnSpec[] = [
    { key: 'portrait', label: '', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'kind', sortable: false },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  const portraitSettings = tableSettingsOf(undefined, portraitCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const portraitBinding = { family: 'table', settings: portraitSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const makePortraitModel = (scores: readonly [number, number], hour: number): TableModel => ({
    columns: portraitCols,
    hour,
    rows: scores.map((score, i) => ({
      key: `p${i}`,
      name: `Portrait ${i}`,
      cells: {
        portrait: { value: knownValue(`Portrait ${i}`), iconSlot: true, artShape: 'portrait', art: readyArt(`portrait://p${i}`), artKey: `portrait:p${i}` },
        score: { value: knownValue(score) },
      },
    })),
  });
  const pFirst = makePortraitModel([100, 50], 100);
  const pFirstLayout = tableLane.layout(pFirst, portraitBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 3000, speed: 24 } as never);
  const pPrevFrame = { model: pFirst, layout: pFirstLayout, binding: portraitBinding, surface, theme: TEST_THEME, hour: 100, now: 3000, changedAt: 3000, continuity: 3, speed: 24, before: null } as never;
  const pSwap = makePortraitModel([50, 100], 101);
  const pSwapLayout = tableLane.layout(pSwap, portraitBinding, surface as never, TEST_THEME, measure, 101, { previous: pPrevFrame, now: 3000, speed: 24 } as never);
  const pMidUrls = [...tableLane.visibleArt({ model: pSwap, layout: pSwapLayout, binding: portraitBinding, surface, theme: TEST_THEME, hour: 101, now: 3093, changedAt: 3000, continuity: 3, speed: 24, before: pPrevFrame } as never)];
  assert.ok(pMidUrls.includes('portrait://p0') && pMidUrls.includes('portrait://p1'), `exposed 40px portrait above swapping row stays in visibleArt (got ${JSON.stringify(pMidUrls)})`);

  const liveCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  const liveSettings = tableSettingsOf(undefined, liveCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const liveBinding = { family: 'table', settings: liveSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const makeLiveModel = (scores: readonly [number, number], hour: number): TableModel => ({
    columns: liveCols,
    hour,
    dateGrouped: true,
    rows: [
      { key: 'l0', name: 'Live', live: true, cells: { country: { value: knownValue('Live'), country: 'GER', iconSlot: true, art: readyArt('flag://live'), artKey: 'flag:panel:live' }, score: { value: knownValue(scores[0]) } } },
      { key: 'l1', name: 'Hist', cells: { country: { value: knownValue('Hist'), country: 'FRA', iconSlot: true, art: readyArt('flag://hist'), artKey: 'flag:panel:hist' }, score: { value: knownValue(scores[1]) } } },
    ],
  });
  const lFirst = makeLiveModel([50, 100], 100);
  const lFirstLayout = tableLane.layout(lFirst, liveBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 4000, speed: 24 } as never);
  const lPrevFrame = { model: lFirst, layout: lFirstLayout, binding: liveBinding, surface, theme: TEST_THEME, hour: 100, now: 4000, changedAt: 4000, continuity: 4, speed: 24, before: null } as never;
  const lSwap = makeLiveModel([100, 50], 101);
  const lSwapLayout = tableLane.layout(lSwap, liveBinding, surface as never, TEST_THEME, measure, 101, { previous: lPrevFrame, now: 4000, speed: 24 } as never);
  const lMidUrls = [...tableLane.visibleArt({ model: lSwap, layout: lSwapLayout, binding: liveBinding, surface, theme: TEST_THEME, hour: 101, now: 4112, changedAt: 4000, continuity: 4, speed: 24, before: lPrevFrame } as never)];
  assert.ok(lMidUrls.includes('flag://live') && lMidUrls.includes('flag://hist'), `flag centered in live-end content height stays visible at 112 ms (got ${JSON.stringify(lMidUrls)})`);

  const pinnedCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 160 },
    ...Array.from({ length: 6 }, (_, i) => ({ key: `c${i}`, label: `Col ${i}`, as: 'plain' as const, offered: ['plain' as const], priority: i + 1, numeric: true, hideable: false, minWidth: 90 })),
  ];
  const pinnedSettings = tableSettingsOf(undefined, pinnedCols, { density: 'normal', sort: { column: 'c0', direction: 'descending' } });
  const pinnedBinding = { family: 'table', settings: pinnedSettings, width: 320, height: 110, scrollLeft: 40, scrollTop: 0, options: NO_OPTIONS } as never;
  const pinnedSurface = { width: 320, height: 110, dpr: 1 };
  const rowA: TableRow = { key: 'a', name: 'Alpha', cells: { country: { value: knownValue('Alpha'), country: 'GER', iconSlot: true, art: readyArt('flag://pinned-alpha'), artKey: 'flag:panel:alpha' }, c0: { value: knownValue(10) } } };
  const gapSlot: TableModelSlot = { kind: 'gap', key: 'gap:1', firstHour: 10, lastHour: 20, reason: 'Collector detached' };
  const gFirst: TableModel = { columns: pinnedCols, hour: 100, unsorted: true, rows: [rowA], slots: [gapSlot, { kind: 'data', row: rowA }] };
  const gFirstLayout = tableLane.layout(gFirst, pinnedBinding, pinnedSurface as never, TEST_THEME, measure, 100, { previous: null, now: 5000, speed: 24 } as never);
  const gPrevFrame = { model: gFirst, layout: gFirstLayout, binding: pinnedBinding, surface: pinnedSurface, theme: TEST_THEME, hour: 100, now: 5000, changedAt: 5000, continuity: 5, speed: 24, before: null } as never;
  const gMoved: TableModel = { columns: pinnedCols, hour: 101, unsorted: true, rows: [rowA], slots: [{ kind: 'data', row: rowA }, gapSlot] };
  const gMovedLayout = tableLane.layout(gMoved, pinnedBinding, pinnedSurface as never, TEST_THEME, measure, 101, { previous: gPrevFrame, now: 5000, speed: 24 } as never);
  const gMidUrls = [...tableLane.visibleArt({ model: gMoved, layout: gMovedLayout, binding: pinnedBinding, surface: pinnedSurface, theme: TEST_THEME, hour: 101, now: 5020, changedAt: 5000, continuity: 5, speed: 24, before: gPrevFrame } as never)];
  assert.ok(gMidUrls.includes('flag://pinned-alpha'), `a pinned-column flag gliding across a recording gap stays in visibleArt at 20 ms (got ${JSON.stringify(gMidUrls)})`);

  const flagCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  const flagSettings = tableSettingsOf(undefined, flagCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const makeFlagModel = (scores: readonly number[], hour: number, withTraits = false): TableModel => ({
    columns: withTraits
      ? [...flagCols, { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 2, cellKind: 'icons', sortable: false }]
      : flagCols,
    hour,
    rows: scores.map((score, i) => ({
      key: `f${i}`,
      name: `Flag ${i}`,
      cells: {
        country: { value: knownValue(`Flag ${i}`), country: `C${i}`, iconSlot: true, art: readyArt(`flag://f${i}`), artKey: `flag:panel:f${i}` },
        score: { value: knownValue(score) },
        ...(withTraits ? { traits: { value: knownValue('1'), icons: [{ name: 'Trait', art: readyArt('trait://shared') }] } } : {}),
      },
    })),
  });
  const offscreenBinding = { family: 'table', settings: flagSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: 750 * ROW_HEIGHT.normal + 12, options: NO_OPTIONS } as never;
  const fAsc = makeFlagModel(Array.from({ length: count }, (_, i) => count - i), 100);
  const fRev = makeFlagModel(Array.from({ length: count }, (_, i) => i + 1), 101);
  const fAscLayout = tableLane.layout(fAsc, offscreenBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 6000, speed: 24 } as never);
  const fAscFrame = { model: fAsc, layout: fAscLayout, binding: offscreenBinding, surface, theme: TEST_THEME, hour: 100, now: 6000, changedAt: 6000, continuity: 6, speed: 24, before: null } as never;
  const fRevLayout = tableLane.layout(fRev, offscreenBinding, surface as never, TEST_THEME, measure, 101, { previous: fAscFrame, now: 6000, speed: 24 } as never);
  const offscreenUrls = [...tableLane.visibleArt({ model: fRev, layout: fRevLayout, binding: offscreenBinding, surface, theme: TEST_THEME, hour: 101, now: 6089.99, changedAt: 6000, continuity: 6, speed: 24, before: fAscFrame } as never)];
  assert.equal(offscreenUrls.length, 0, `gliding rows whose flags are clipped outside the viewport pin zero URLs (got ${offscreenUrls.length})`);

  const traitCols: TableColumnSpec[] = [
    ...flagCols,
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 2, cellKind: 'icons', sortable: false },
  ];
  const traitSettings = tableSettingsOf(undefined, traitCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const traitBinding = { family: 'table', settings: traitSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: 750 * ROW_HEIGHT.normal - 36, options: NO_OPTIONS } as never;
  const tAsc = makeFlagModel(Array.from({ length: count }, (_, i) => count - i), 100, true);
  const tRev = makeFlagModel(Array.from({ length: count }, (_, i) => i + 1), 101, true);
  const tAscLayout = tableLane.layout(tAsc, traitBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 7000, speed: 24 } as never);
  const tAscFrame = { model: tAsc, layout: tAscLayout, binding: traitBinding, surface, theme: TEST_THEME, hour: 100, now: 7000, changedAt: 7000, continuity: 7, speed: 24, before: null } as never;
  const tRevLayout = tableLane.layout(tRev, traitBinding, surface as never, TEST_THEME, measure, 101, { previous: tAscFrame, now: 7000, speed: 24 } as never);
  const traitUrls = [...tableLane.visibleArt({ model: tRev, layout: tRevLayout, binding: traitBinding, surface, theme: TEST_THEME, hour: 101, now: 7089.99, changedAt: 7000, continuity: 7, speed: 24, before: tAscFrame } as never)];
  assert.ok(traitUrls.length > 0 && traitUrls.length <= 10, `a trait icon strip uses its 23px painted bounds instead of keeping 760 covered rows (got ${traitUrls.length})`);

  const mixedCols: TableColumnSpec[] = [
    { key: 'portrait', label: '', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'kind', sortable: false },
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 1, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 2, numeric: true },
  ];
  const mixedSettings = tableSettingsOf(undefined, mixedCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const mixedBinding = { family: 'table', settings: mixedSettings, width: 420, height: 110, scrollLeft: 0, scrollTop: midScroll, options: NO_OPTIONS } as never;
  const makeMixedModel = (scores: readonly number[], hour: number, portrait: ArtResult): TableModel => ({
    columns: mixedCols,
    hour,
    rows: scores.map((score, i) => ({
      key: `m${i}`,
      name: `Mixed ${i}`,
      cells: {
        portrait: { value: knownValue(`Mixed ${i}`), iconSlot: true, artShape: 'portrait', art: portrait, artKey: `portrait:m${i}` },
        country: { value: knownValue(`Mixed ${i}`), country: `C${i}`, iconSlot: true, art: readyArt(`flag://m${i}`), artKey: `flag:panel:m${i}` },
        score: { value: knownValue(score) },
      },
    })),
  });
  for (const [label, portrait, continuity] of [
    ['unavailable portrait', unavailableArt('Missing portrait'), 8],
    ['shared portrait', readyArt('portrait://shared'), 9],
  ] as const) {
    const mAsc = makeMixedModel(Array.from({ length: count }, (_, i) => count - i), 100, portrait);
    const mRev = makeMixedModel(Array.from({ length: count }, (_, i) => i + 1), 101, portrait);
    const mAscLayout = tableLane.layout(mAsc, mixedBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 8000, speed: 24 } as never);
    const mAscFrame = { model: mAsc, layout: mAscLayout, binding: mixedBinding, surface, theme: TEST_THEME, hour: 100, now: 8000, changedAt: 8000, continuity, speed: 24, before: null } as never;
    const mRevLayout = tableLane.layout(mRev, mixedBinding, surface as never, TEST_THEME, measure, 101, { previous: mAscFrame, now: 8000, speed: 24 } as never);
    const mixedUrls = [...tableLane.visibleArt({ model: mRev, layout: mRevLayout, binding: mixedBinding, surface, theme: TEST_THEME, hour: 101, now: 8090, changedAt: 8000, continuity, speed: 24, before: mAscFrame } as never)];
    const flagUrls = mixedUrls.filter(url => url.startsWith('flag://'));
    assert.equal(flagUrls.length, 1, `at 90 ms with ${label}, only the single uncovered country flag is pinned (got ${flagUrls.length})`);
  }

  const detailCols: TableColumnSpec[] = [
    { key: 'terms', label: 'Terms', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
  ];
  const detailSettings = tableSettingsOf(undefined, detailCols, { density: 'normal', sort: { column: 'terms', direction: 'descending' } });
  const detailFlags = Array.from({ length: 256 }, (_, i) => ({
    tag: `T${i}`,
    art: readyArt(`flag://term-${i}`),
    label: `Country term ${i}`,
  }));
  const detailModel: TableModel = {
    columns: detailCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'deal-parent',
      name: 'Peace Conference',
      cells: { terms: { value: knownValue('Peace Conference') } },
      children: [{
        key: 'deal-detail',
        name: 'Deal',
        detailColumn: 'terms',
        detailEnd: true,
        cells: {
          terms: {
            value: knownValue('Terms'),
            heading: { flag: { tag: 'GER', art: readyArt('flag://deal-heading') }, text: knownValue('Peace conference') },
            flags: detailFlags,
          },
        },
      }],
    }],
  };
  const detailBinding = { family: 'table', settings: detailSettings, width: 320, height: 110, scrollLeft: 0, scrollTop: 300, expanded: { 'deal-parent': true }, options: NO_OPTIONS } as never;
  const detailLayout = tableLane.layout(detailModel, detailBinding, { width: 320, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 9000, speed: 24 } as never);
  const detailUrls = [...tableLane.visibleArt({ model: detailModel, layout: detailLayout, binding: detailBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 9000, changedAt: 9000, continuity: 10, speed: 24, before: null } as never)];
  assert.ok(!detailUrls.includes('flag://deal-heading'), 'scrolled-off detail heading flag is not pinned');
  assert.ok(detailUrls.length > 0 && detailUrls.length <= 16, `a 256-term detail row pins only flags on visible wrapped lines (got ${detailUrls.length})`);

  const medalCols: TableColumnSpec[] = [
    { key: 'medals', label: 'Medals', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'icons', sortable: false, minWidth: 240, maxWidth: 240 },
  ];
  const medalSettings = tableSettingsOf(undefined, medalCols, { density: 'normal', sort: { column: 'medals', direction: 'descending' } });
  const medalModel: TableModel = {
    columns: medalCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'div-1',
      name: 'Division 1',
      cells: {
        medals: {
          value: knownValue('1500'),
          iconStripKind: 'medals',
          icons: Array.from({ length: 1500 }, (_, i) => ({ name: `Medal ${i}`, art: readyArt(`medal://m${i}`) })),
        },
      },
    }],
  };
  const medalBinding = { family: 'table', settings: medalSettings, width: 240, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const medalLayout = tableLane.layout(medalModel, medalBinding, { width: 240, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 10000, speed: 24 } as never);
  const medalUrls = [...tableLane.visibleArt({ model: medalModel, layout: medalLayout, binding: medalBinding, surface: { width: 240, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 10000, changedAt: 10000, continuity: 11, speed: 24, before: null } as never)];
  assert.ok(medalUrls.length > 0 && medalUrls.length <= 10, `a 240 px medal strip with 1,500 icons pins only fitted icons (got ${medalUrls.length})`);

  const { LaneArtCache } = await import('../art/laneCache.ts');
  const { drawIconStrip, iconStripSlotWidth } = await import('../../draw/kit.ts');
  const { resolveCachedArt, toArtSlotState } = await import('../art/adapters.ts');
  const narrowTraitCols: TableColumnSpec[] = [
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'icons', sortable: false, minWidth: 240, maxWidth: 240 },
  ];
  const narrowTraitSettings = tableSettingsOf(undefined, narrowTraitCols, { density: 'normal', sort: { column: 'traits', direction: 'descending' } });
  const narrowTraits = Array.from({ length: 1500 }, (_, i) => ({ name: `Trait ${i}`, art: readyArt(`trait://narrow-${i}`) }));
  const narrowTraitModel: TableModel = {
    columns: narrowTraitCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-1', name: 'General', cells: { traits: { value: knownValue('1500'), iconStripKind: 'traits', icons: narrowTraits } } }],
  };
  const narrowTraitBinding = { family: 'table', settings: narrowTraitSettings, width: 240, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const narrowTraitLayout = tableLane.layout(narrowTraitModel, narrowTraitBinding, { width: 240, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 11000, speed: 24 } as never);
  const narrowCache = new LaneArtCache({
    maxEntries: 8,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 8, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  const traitCol = narrowTraitLayout.shape.columns.columns[0]!;
  const drawnUrls = () => {
    const stripCtx = instrumentedContext().ctx as unknown as OffscreenCanvasRenderingContext2D;
    const res = drawIconStrip(
      stripCtx,
      10 + (traitCol.leadInset ?? 0),
      traitCol.width - 10 - (traitCol.trailInset ?? 0),
      18,
      narrowTraits.map(item => ({ state: toArtSlotState(resolveCachedArt(item.art, narrowCache), item.name), name: item.name })),
      TEST_THEME,
      { kind: 'traits' },
    );
    return narrowTraits.slice(0, res.drawnCount).map(item => item.art.url);
  };
  const pinnedUrls = () => [...tableLane.visibleArt({ model: narrowTraitModel, layout: narrowTraitLayout, binding: narrowTraitBinding, surface: { width: 240, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 11000, changedAt: 11000, continuity: 12, speed: 24, before: null, art: narrowCache } as never)];
  const beforeDecode = pinnedUrls();
  assert.deepEqual(beforeDecode, drawnUrls(), 'before decode, visibleArt matches drawIconStrip exactly');
  for (let round = 0; round < 10; round++) {
    const shown = pinnedUrls();
    if (shown.every(url => narrowCache.stateOfUrl(url) === 'ready')) break;
    narrowCache.requestVisible(shown, []);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const afterDecode = pinnedUrls();
  assert.deepEqual(afterDecode, drawnUrls(), 'after decoding 8x32 bitmaps, visibleArt expands to match drawIconStrip exactly');
  assert.ok(afterDecode.length > beforeDecode.length, `decoding narrower 8x32 bitmaps increases pinned icons (${beforeDecode.length} -> ${afterDecode.length})`);
  assert.ok(afterDecode.every(url => narrowCache.stateOfUrl(url) === 'ready'), 'every drawn icon is decoded and ready in cache');
  narrowCache.dispose();

  const hScrollCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 160 },
    ...Array.from({ length: 3 }, (_, i) => ({ key: `pad${i}`, label: `Pad ${i}`, as: 'plain' as const, offered: ['plain' as const], priority: i + 1, numeric: true, hideable: false, minWidth: 92 })),
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 4, cellKind: 'icons', sortable: false, hideable: false, minWidth: 240, maxWidth: 240 },
  ];
  const hScrollSettings = tableSettingsOf(undefined, hScrollCols, { density: 'normal', sort: { column: 'pad0', direction: 'descending' } });
  const hScrollModel: TableModel = {
    columns: hScrollCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'h0',
      name: 'Alpha',
      cells: {
        country: { value: knownValue('Alpha'), country: 'GER', iconSlot: true, art: readyArt('flag://h-alpha') },
        pad0: { value: knownValue(1) },
        traits: { value: knownValue('12'), iconStripKind: 'traits', icons: Array.from({ length: 12 }, (_, i) => ({ name: `T${i}`, art: readyArt(`trait://h-${i}`) })) },
      },
    }],
  };
  const hSurface = { width: 320, height: 110, dpr: 1 };
  const hLeftBinding = { family: 'table', settings: hScrollSettings, width: 320, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const hLeftLayout = tableLane.layout(hScrollModel, hLeftBinding, hSurface as never, TEST_THEME, measure, 100, { previous: null, now: 12000, speed: 24 } as never);
  const hLeftUrls = [...tableLane.visibleArt({ model: hScrollModel, layout: hLeftLayout, binding: hLeftBinding, surface: hSurface, theme: TEST_THEME, hour: 100, now: 12000, changedAt: 12000, continuity: 13, speed: 24, before: null } as never)];
  assert.deepEqual(hLeftUrls, ['flag://h-alpha'], 'horizontally offscreen trait column pins zero trait URLs');
  const hRightBinding = { family: 'table', settings: hScrollSettings, width: 320, height: 110, scrollLeft: 320, scrollTop: 0, options: NO_OPTIONS } as never;
  const hRightLayout = tableLane.layout(hScrollModel, hRightBinding, hSurface as never, TEST_THEME, measure, 100, { previous: null, now: 12000, speed: 24 } as never);
  const hRightUrls = [...tableLane.visibleArt({ model: hScrollModel, layout: hRightLayout, binding: hRightBinding, surface: hSurface, theme: TEST_THEME, hour: 100, now: 12000, changedAt: 12000, continuity: 14, speed: 24, before: null } as never)];
  assert.ok(hRightUrls.includes('flag://h-alpha') && hRightUrls.some(url => url.startsWith('trait://h-')), `scrolling the trait column into view pins its visible icons (got ${JSON.stringify(hRightUrls)})`);

  const skillCols: TableColumnSpec[] = [
    { key: 'skill', label: 'Skill', as: 'plain', offered: ['plain'], priority: 0, numeric: true, cellKind: 'skill', skillIcon: 'badge://skill-default', minWidth: 68 },
  ];
  const skillSettings = tableSettingsOf(undefined, skillCols, { density: 'normal', sort: { column: 'skill', direction: 'descending' } });
  const skillModel: TableModel = {
    columns: skillCols,
    hour: 100,
    unsorted: true,
    rows: [
      { key: 'sk-missing', name: 'Missing', cells: { skill: { value: notRecorded('Skill not recorded'), art: readyArt('badge://skill-missing') } } },
      { key: 'sk-known', name: 'Known', cells: { skill: { value: knownValue(5), art: readyArt('badge://skill-known') } } },
    ],
  };
  const skillBinding = { family: 'table', settings: skillSettings, width: 200, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const skillLayout = tableLane.layout(skillModel, skillBinding, { width: 200, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 13000, speed: 24 } as never);
  const skillUrls = [...tableLane.visibleArt({ model: skillModel, layout: skillLayout, binding: skillBinding, surface: { width: 200, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 13000, changedAt: 13000, continuity: 15, speed: 24, before: null } as never)];
  assert.deepEqual(skillUrls, ['badge://skill-known'], 'a notRecorded skill cell does not pin its undrawn badge URL');

  const distinctPortraitModel = (scores: readonly number[], hour: number): TableModel => ({
    columns: mixedCols,
    hour,
    rows: scores.map((score, i) => ({
      key: `dp${i}`,
      name: `Portrait ${i}`,
      cells: {
        portrait: {
          value: knownValue(`Portrait ${i}`),
          iconSlot: true,
          artShape: 'portrait',
          art: readyArt(`portrait://distinct-${i}`, { frame: { frameIndex: 0, frameCount: 1, frameWidth: 32, frameHeight: 32, textureWidth: 32, textureHeight: 32, outputWidth: 32, outputHeight: 32 } }),
        },
        country: { value: knownValue(`Portrait ${i}`), country: `C${i}`, iconSlot: true, art: readyArt(`flag://dp-${i}`) },
        score: { value: knownValue(score) },
      },
    })),
  });
  const dpAsc = distinctPortraitModel(Array.from({ length: count }, (_, i) => count - i), 100);
  const dpRev = distinctPortraitModel(Array.from({ length: count }, (_, i) => i + 1), 101);
  const dpAscLayout = tableLane.layout(dpAsc, mixedBinding, surface as never, TEST_THEME, measure, 100, { previous: null, now: 14000, speed: 24 } as never);
  const dpAscFrame = { model: dpAsc, layout: dpAscLayout, binding: mixedBinding, surface, theme: TEST_THEME, hour: 100, now: 14000, changedAt: 14000, continuity: 16, speed: 24, before: null } as never;
  const dpRevLayout = tableLane.layout(dpRev, mixedBinding, surface as never, TEST_THEME, measure, 101, { previous: dpAscFrame, now: 14000, speed: 24 } as never);
  const dpMidUrls = [...tableLane.visibleArt({ model: dpRev, layout: dpRevLayout, binding: mixedBinding, surface, theme: TEST_THEME, hour: 101, now: 14090, changedAt: 14000, continuity: 16, speed: 24, before: dpAscFrame } as never)];
  const dpPortraits = dpMidUrls.filter(url => url.startsWith('portrait://'));
  assert.equal(dpPortraits.length, 1, `at 90 ms with 1,500 distinct 32x32 portraits fitted to 30x30, only the topmost portrait is pinned (got ${dpPortraits.length})`);

  const genCols: TableColumnSpec[] = [
    { key: 'portrait', label: '', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'kind', pinned: true, hideable: false, minWidth: 58, maxWidth: 58, sortable: false },
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 1, text: true, cellKind: 'item', hideable: false, minWidth: 160 },
    ...Array.from({ length: 4 }, (_, i) => ({ key: `stat${i}`, label: `Stat ${i}`, as: 'plain' as const, offered: ['plain' as const], priority: i + 2, numeric: true, hideable: false, minWidth: 90 })),
  ];
  const genSettings = tableSettingsOf(undefined, genCols, { density: 'normal', sort: { column: 'stat0', direction: 'descending' } });
  const genModel: TableModel = {
    columns: genCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'g0',
      name: 'General',
      cells: {
        portrait: { value: knownValue('General'), iconSlot: true, artShape: 'portrait', art: readyArt('portrait://gen-0') },
        country: { value: knownValue('Germany'), country: 'GER', iconSlot: true, art: readyArt('flag://gen-hidden-behind-pinned') },
        stat0: { value: knownValue(5) },
      },
    }],
  };
  const genBinding = { family: 'table', settings: genSettings, width: 320, height: 110, scrollLeft: 40, scrollTop: 0, options: NO_OPTIONS } as never;
  const genLayout = tableLane.layout(genModel, genBinding, { width: 320, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 15000, speed: 24 } as never);
  const genUrls = [...tableLane.visibleArt({ model: genModel, layout: genLayout, binding: genBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 15000, changedAt: 15000, continuity: 17, speed: 24, before: null } as never)];
  assert.deepEqual(genUrls, ['portrait://gen-0'], 'a country flag scrolled behind a pinned portrait column is not pinned');

  const trimmedSpriteCols: TableColumnSpec[] = [
    { key: 'kind', label: '', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'kind', sortable: false, minWidth: 44 },
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 1, text: true, cellKind: 'item' },
  ];
  const trimmedSpriteSettings = tableSettingsOf(undefined, trimmedSpriteCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const trimmedModel: TableModel = {
    columns: trimmedSpriteCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'drawn-0',
      name: 'Division',
      cells: {
        kind: { value: knownValue('Division'), iconSlot: true, artShape: 'drawn', artTile: 'flat', art: readyArt('sprite://padded-64x8') },
        name: { value: knownValue('Division') },
      },
    }],
  };
  const trimmedBinding = { family: 'table', settings: trimmedSpriteSettings, width: 320, height: 110, scrollLeft: 0, scrollTop: 22, options: NO_OPTIONS } as never;
  const trimmedLayout = tableLane.layout(trimmedModel, trimmedBinding, { width: 320, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 16000, speed: 24 } as never);
  const OrigOffscreen = globalThis.OffscreenCanvas;
  const paddedData = new Uint8ClampedArray(64 * 8 * 4);
  for (let py = 0; py < 8; py++) {
    for (let px = 28; px < 36; px++) paddedData[(py * 64 + px) * 4 + 3] = 255;
  }
  const paddedBitmap = { width: 64, height: 8, close() {} } as unknown as ImageBitmap;
  (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(width: number, height: number) {
      this.width = width;
      this.height = height;
    }
    getContext() {
      return {
        drawImage() {},
        getImageData: () => ({ data: paddedData }),
      };
    }
  };
  const trimmedCache = new LaneArtCache({ fetchBitmap: async () => paddedBitmap });
  try {
    const beforeTrimUrls = [...tableLane.visibleArt({ model: trimmedModel, layout: trimmedLayout, binding: trimmedBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 16000, changedAt: 16000, continuity: 18, speed: 24, before: null, art: trimmedCache } as never)];
    assert.deepEqual(beforeTrimUrls, ['sprite://padded-64x8'], 'before decode, drawn sprite uses conservative slot bounds');
    trimmedCache.requestVisible(beforeTrimUrls, []);
    await new Promise<void>(resolve => setImmediate(resolve));
    const afterTrimUrls = [...tableLane.visibleArt({ model: trimmedModel, layout: trimmedLayout, binding: trimmedBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 16000, changedAt: 16000, continuity: 18, speed: 24, before: null, art: trimmedCache } as never)];
    assert.deepEqual(afterTrimUrls, ['sprite://padded-64x8'], 'after decode, 64x8 sprite with 8x8 opaque center stays pinned via trimmed bounds at scrollTop=22');
  } finally {
    trimmedCache.dispose();
    (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas = OrigOffscreen;
  }

  const decCols: TableColumnSpec[] = [
    { key: 'title', label: 'Decision', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 338, maxWidth: 338 },
    ...Array.from({ length: 3 }, (_, i) => ({ key: `d${i}`, label: `D${i}`, as: 'plain' as const, offered: ['plain' as const], priority: i + 1, numeric: true, hideable: false, minWidth: 90, maxWidth: 90 })),
    { key: 'cost', label: 'Cost', as: 'plain', offered: ['plain'], priority: 4, numeric: true, hideable: false, minWidth: 110, maxWidth: 110 },
    ...Array.from({ length: 2 }, (_, i) => ({ key: `t${i}`, label: `T${i}`, as: 'plain' as const, offered: ['plain' as const], priority: i + 5, numeric: true, hideable: false, minWidth: 150, maxWidth: 150 })),
  ];
  const decSettings = tableSettingsOf(undefined, decCols, { density: 'normal', sort: { column: 'cost', direction: 'descending' } });
  const decModel: TableModel = {
    columns: decCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'dec-0',
      name: 'War Bonds',
      cells: {
        title: { value: knownValue('War Bonds') },
        cost: { value: knownValue(50), unitArt: readyArt('icon://pp-cost'), unitSuffix: 'PP' },
      },
    }],
  };
  const decSurface = { width: 520, height: 110, dpr: 1 };
  const decHiddenBinding = { family: 'table', settings: decSettings, width: 520, height: 110, scrollLeft: 345, scrollTop: 0, options: NO_OPTIONS } as never;
  const decHiddenLayout = tableLane.layout(decModel, decHiddenBinding, decSurface as never, TEST_THEME, measure, 100, { previous: null, now: 17000, speed: 24 } as never);
  const decHiddenUrls = [...tableLane.visibleArt({ model: decModel, layout: decHiddenLayout, binding: decHiddenBinding, surface: decSurface, theme: TEST_THEME, hour: 100, now: 17000, changedAt: 17000, continuity: 19, speed: 24, before: null } as never)];
  assert.deepEqual(decHiddenUrls, [], 'a numeric unitArt icon scrolled behind pinned columns is not pinned even when the figure text remains exposed');
  const decVisibleBinding = { family: 'table', settings: decSettings, width: 520, height: 110, scrollLeft: 220, scrollTop: 0, options: NO_OPTIONS } as never;
  const decVisibleLayout = tableLane.layout(decModel, decVisibleBinding, decSurface as never, TEST_THEME, measure, 100, { previous: null, now: 17000, speed: 24 } as never);
  const decVisibleUrls = [...tableLane.visibleArt({ model: decModel, layout: decVisibleLayout, binding: decVisibleBinding, surface: decSurface, theme: TEST_THEME, hour: 100, now: 17000, changedAt: 17000, continuity: 20, speed: 24, before: null } as never)];
  assert.deepEqual(decVisibleUrls, ['icon://pp-cost'], 'a numeric unitArt icon exposed past pinned columns is pinned');

  const peaceCols: TableColumnSpec[] = [
    { key: 'date', label: 'Date', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'date', minWidth: 320, maxWidth: 320 },
    { key: 'terms', label: 'Terms', as: 'plain', offered: ['plain'], priority: 1, text: true, cellKind: 'item' },
  ];
  const peaceSettings = tableSettingsOf(undefined, peaceCols, { density: 'normal', sort: { column: 'date', direction: 'descending' } });
  const peaceModel: TableModel = {
    columns: peaceCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'peace-parent',
      name: 'Peace',
      cells: { date: { value: knownValue(100) }, terms: { value: knownValue('Peace') } },
      children: [{
        key: 'peace-detail',
        name: 'Terms',
        detailColumn: 'terms',
        detailEnd: true,
        cells: {
          terms: {
            value: knownValue('Terms'),
            heading: { flag: { tag: 'GER', art: readyArt('flag://peace-offscreen-head') }, text: knownValue('Germany') },
            flags: [
              { tag: 'FRA', art: readyArt('flag://peace-offscreen-1'), label: 'France' },
              { tag: 'POL', art: readyArt('flag://peace-offscreen-2'), label: 'Poland' },
            ],
          },
        },
      }],
    }],
  };
  const peaceBinding = { family: 'table', settings: peaceSettings, width: 320, height: 110, scrollLeft: 0, scrollTop: 0, expanded: { 'peace-parent': true }, options: NO_OPTIONS } as never;
  const peaceLayout = tableLane.layout(peaceModel, peaceBinding, { width: 320, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 18000, speed: 24 } as never);
  const peaceUrls = [...tableLane.visibleArt({ model: peaceModel, layout: peaceLayout, binding: peaceBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 18000, changedAt: 18000, continuity: 21, speed: 24, before: null } as never)];
  assert.deepEqual(peaceUrls, [], 'Peace Deals detail flags pushed past the 320 px viewport edge by a 320 px Date column are not pinned');

  const tightTraitCols: TableColumnSpec[] = [
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'icons', sortable: false, minWidth: 40, maxWidth: 40 },
  ];
  const tightTraitSettings = tableSettingsOf(undefined, tightTraitCols, { density: 'normal', sort: { column: 'traits', direction: 'descending' } });
  const tightTraitModel: TableModel = {
    columns: tightTraitCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-tight', name: 'General', cells: { traits: { value: knownValue('1'), iconStripKind: 'traits', icons: [{ name: 'Narrow', art: readyArt('trait://tight-8x32') }] } } }],
  };
  const tightCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 8, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  tightCache.requestVisible([], Array.from({ length: 128 }, (_, i) => `offscreen://fill-${i}`));
  await new Promise<void>(resolve => setImmediate(resolve));
  const tightBinding = { family: 'table', settings: tightTraitSettings, width: 40, height: 110, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const tightLayout = tableLane.layout(tightTraitModel, tightBinding, { width: 40, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 19000, speed: 24 } as never);
  const tightBefore = [...tableLane.visibleArt({ model: tightTraitModel, layout: tightLayout, binding: tightBinding, surface: { width: 40, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 19000, changedAt: 19000, continuity: 22, speed: 24, before: null, art: tightCache } as never)];
  assert.deepEqual(tightBefore, ['trait://tight-8x32'], '40 px Traits column requests its undecoded narrow trait icon even when offscreen cache is full');
  tightCache.requestVisible(tightBefore, []);
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.equal(tightCache.stateOfUrl('trait://tight-8x32'), 'ready', 'tight trait icon decodes once requested via visibleArt');
  const tightAfter = [...tableLane.visibleArt({ model: tightTraitModel, layout: tightLayout, binding: tightBinding, surface: { width: 40, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 19000, changedAt: 19000, continuity: 23, speed: 24, before: null, art: tightCache } as never)];
  assert.deepEqual(tightAfter, ['trait://tight-8x32'], 'decoded 8x32 trait icon stays in visibleArt');
  tightCache.dispose();

  const fullPinCols: TableColumnSpec[] = [
    { key: 'portrait', label: 'Portrait', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'kind', pinned: true, hideable: false, minWidth: 320, maxWidth: 320 },
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 1, text: true, cellKind: 'item', hideable: false, minWidth: 140 },
  ];
  const fullPinSettings = tableSettingsOf(undefined, fullPinCols, { density: 'normal', sort: { column: 'country', direction: 'descending' } });
  const fullPinModel: TableModel = {
    columns: fullPinCols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'fp-1',
      name: 'Alpha',
      cells: {
        portrait: { value: knownValue('Portrait'), art: readyArt('portrait://full-pin'), artShape: 'portrait', iconSlot: true },
        country: { value: knownValue('Alpha'), country: 'GER', iconSlot: true, art: readyArt('flag://fully-covered-by-pin') },
      },
    }],
  };
  const fullPinBinding = { family: 'table', settings: fullPinSettings, width: 320, height: 110, scrollLeft: 20, scrollTop: 0, options: NO_OPTIONS } as never;
  const fullPinLayout = tableLane.layout(fullPinModel, fullPinBinding, { width: 320, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 20000, speed: 24 } as never);
  const fullPinUrls = [...tableLane.visibleArt({ model: fullPinModel, layout: fullPinLayout, binding: fullPinBinding, surface: { width: 320, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 20000, changedAt: 20000, continuity: 24, speed: 24, before: null } as never)];
  assert.deepEqual(fullPinUrls, ['portrait://full-pin'], 'unpinned country flag completely covered when pinnedWidth >= viewport.width is excluded from visibleArt');

  const clippedTraitCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 100, maxWidth: 100 },
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'icons', sortable: false, hideable: false, minWidth: 96, maxWidth: 96 },
    { key: 'pad', label: 'Pad', as: 'plain', offered: ['plain'], priority: 2, numeric: true, hideable: false, minWidth: 120 },
  ];
  const clippedTraitSettings = tableSettingsOf(undefined, clippedTraitCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const clippedTraits = Array.from({ length: 4 }, (_, i) => ({ name: `Trait ${i}`, art: readyArt(`trait://clipped-${i}`) }));
  const clippedTraitModel: TableModel = {
    columns: clippedTraitCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-clip', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('4'), iconStripKind: 'traits', icons: clippedTraits } } }],
  };
  const clippedCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 21, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  clippedCache.requestVisible([], Array.from({ length: 128 }, (_, i) => `offscreen://clip-${i}`));
  await new Promise<void>(resolve => setImmediate(resolve));
  const clippedBinding = { family: 'table', settings: clippedTraitSettings, width: 140, height: 110, scrollLeft: 64, scrollTop: 0, options: NO_OPTIONS } as never;
  const clippedLayout = tableLane.layout(clippedTraitModel, clippedBinding, { width: 140, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 21000, speed: 24 } as never);
  for (let round = 0; round < 6; round++) {
    const urls = [...tableLane.visibleArt({ model: clippedTraitModel, layout: clippedLayout, binding: clippedBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 21000, changedAt: 21000, continuity: 25 + round, speed: 24, before: null, art: clippedCache } as never)];
    if (urls.length === 0 || urls.every(url => clippedCache.stateOfUrl(url) === 'ready')) break;
    clippedCache.requestVisible(urls, []);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const clippedFinal = [...tableLane.visibleArt({ model: clippedTraitModel, layout: clippedLayout, binding: clippedBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 21000, changedAt: 21000, continuity: 31, speed: 24, before: null, art: clippedCache } as never)];
  assert.deepEqual(clippedFinal, ['trait://clipped-2', 'trait://clipped-3'], `probing clipped undecoded traits exposes traits 2 and 3 and drops clipped predecessors (got ${JSON.stringify(clippedFinal)})`);
  assert.equal(clippedCache.stateOfUrl('trait://clipped-2'), 'ready');
  assert.equal(clippedCache.stateOfUrl('trait://clipped-3'), 'ready');
  clippedCache.dispose();

  const { defaultArtFrame } = await import('../art/types.ts');
  const medalPinCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 100, maxWidth: 100 },
    { key: 'medals', label: 'Medals', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'icons', sortable: false, hideable: false, minWidth: 40, maxWidth: 40 },
    { key: 'pad', label: 'Pad', as: 'plain', offered: ['plain'], priority: 2, numeric: true, hideable: false, minWidth: 120 },
  ];
  const medalPinSettings = tableSettingsOf(undefined, medalPinCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const narrowMedalFrame = defaultArtFrame(6, 28);
  const recordedMedals = [
    { name: 'Medal 0', art: readyArt('medal://rec-0', { frame: narrowMedalFrame }) },
    { name: 'Medal 1', art: readyArt('medal://rec-1', { frame: narrowMedalFrame }) },
  ];
  const medalPinModel: TableModel = {
    columns: medalPinCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'div-m', name: 'Division', cells: { name: { value: knownValue('Division') }, medals: { value: knownValue('2'), iconStripKind: 'medals', icons: recordedMedals } } }],
  };
  const medalPinCache = new LaneArtCache({
    maxEntries: 8,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 6, height: 28, close() {} }) as unknown as ImageBitmap,
  });
  const medalPinBinding = { family: 'table', settings: medalPinSettings, width: 140, height: 110, scrollLeft: 18, scrollTop: 0, options: NO_OPTIONS } as never;
  const medalPinLayout = tableLane.layout(medalPinModel, medalPinBinding, { width: 140, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 22000, speed: 24 } as never);
  const medalPinUrls = [...tableLane.visibleArt({ model: medalPinModel, layout: medalPinLayout, binding: medalPinBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 22000, changedAt: 22000, continuity: 32, speed: 24, before: null, art: medalPinCache } as never)];
  assert.deepEqual(medalPinUrls, ['medal://rec-1'], 'recorded 6x28 medals only request the visible second medal when the first is behind the pinned column');
  medalPinCache.requestVisible(medalPinUrls, []);
  await new Promise<void>(resolve => setImmediate(resolve));
  const medalPinCtx = instrumentedContext().ctx as unknown as OffscreenCanvasRenderingContext2D;
  const medalCol = medalPinLayout.shape.columns.columns[1]!;
  const medalDrawn = drawIconStrip(
    medalPinCtx,
    10 + (medalCol.leadInset ?? 0),
    medalCol.width - 10 - (medalCol.trailInset ?? 0),
    18,
    recordedMedals.map(item => ({ state: toArtSlotState(resolveCachedArt(item.art, medalPinCache), item.name), name: item.name })),
    TEST_THEME,
    { kind: 'medals' },
  );
  assert.equal(medalDrawn.drawnCount, 2, 'drawIconStrip uses recorded slot width for the pending first medal and draws the decoded second medal');
  medalPinCache.dispose();

  const nonOverflowCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 100, maxWidth: 100 },
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'icons', sortable: false, hideable: false, minWidth: 200, maxWidth: 200 },
  ];
  const nonOverflowSettings = tableSettingsOf(undefined, nonOverflowCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const fiveTraits = Array.from({ length: 5 }, (_, i) => ({ name: `Trait ${i}`, art: readyArt(`trait://five-${i}`) }));
  const nonOverflowModel: TableModel = {
    columns: nonOverflowCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-five', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('5'), iconStripKind: 'traits', icons: fiveTraits } } }],
  };
  const offscreenFill = Array.from({ length: 128 }, (_, i) => `offscreen://five-${i}`);
  const fiveCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 20, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  fiveCache.requestVisible([], offscreenFill);
  await new Promise<void>(resolve => setImmediate(resolve));
  const fiveBinding = { family: 'table', settings: nonOverflowSettings, width: 120, height: 110, scrollLeft: 75, scrollTop: 0, options: NO_OPTIONS } as never;
  const fiveLayout = tableLane.layout(nonOverflowModel, fiveBinding, { width: 120, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 23000, speed: 24 } as never);
  for (let round = 0; round < 8; round++) {
    const urls = [...tableLane.visibleArt({ model: nonOverflowModel, layout: fiveLayout, binding: fiveBinding, surface: { width: 120, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 23000, changedAt: 23000, continuity: 40 + round, speed: 24, before: null, art: fiveCache } as never)];
    fiveCache.requestVisible(urls, offscreenFill);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const fiveFinal = [...tableLane.visibleArt({ model: nonOverflowModel, layout: fiveLayout, binding: fiveBinding, surface: { width: 120, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 23000, changedAt: 23000, continuity: 50, speed: 24, before: null, art: fiveCache } as never)];
  assert.ok(fiveFinal.includes('trait://five-4'), `non-overflowing 200 px trait strip probes clipped predecessors so trait 4 loads without scrolling (got ${JSON.stringify(fiveFinal)})`);
  assert.equal(fiveCache.stateOfUrl('trait://five-4'), 'ready', 'trait 4 is decoded and ready with offscreen demand saturated');
  fiveCache.dispose();

  let evictFetches = 0;
  const evictCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async url => {
      evictFetches++;
      const wide = url.endsWith('-0') || url.endsWith('-1');
      return { width: wide ? 32 : 20, height: 32, close() {} } as unknown as ImageBitmap;
    },
  });
  const evictTraits = Array.from({ length: 5 }, (_, i) => ({ name: `Mixed ${i}`, art: readyArt(`trait://evict-${i}`) }));
  const evictModel: TableModel = {
    columns: nonOverflowCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-evict', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('5'), iconStripKind: 'traits', icons: evictTraits } } }],
  };
  const evictOffscreen = Array.from({ length: 128 }, (_, i) => `offscreen://evict-${i}`);
  evictCache.requestVisible([], evictOffscreen);
  await new Promise<void>(resolve => setImmediate(resolve));
  const evictBinding = { family: 'table', settings: nonOverflowSettings, width: 120, height: 110, scrollLeft: 95, scrollTop: 0, options: NO_OPTIONS } as never;
  const evictLayout = tableLane.layout(evictModel, evictBinding, { width: 120, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 24000, speed: 24 } as never);
  for (let round = 0; round < 10; round++) {
    const urls = [...tableLane.visibleArt({ model: evictModel, layout: evictLayout, binding: evictBinding, surface: { width: 120, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 24000, changedAt: 24000, continuity: 60 + round, speed: 24, before: null, art: evictCache } as never)];
    evictCache.requestVisible(urls, evictOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const evictFinal = [...tableLane.visibleArt({ model: evictModel, layout: evictLayout, binding: evictBinding, surface: { width: 120, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 24000, changedAt: 24000, continuity: 75, speed: 24, before: null, art: evictCache } as never)];
  assert.ok(evictFinal.includes('trait://evict-4'), `remembered widths across eviction let fifth trait load at scrollLeft=95 (got ${JSON.stringify(evictFinal)})`);
  assert.equal(evictCache.stateOfUrl('trait://evict-4'), 'ready', 'fifth trait is ready in cache');
  const fetchesAfterSettle = evictFetches;
  for (let extra = 0; extra < 5; extra++) {
    const urls = [...tableLane.visibleArt({ model: evictModel, layout: evictLayout, binding: evictBinding, surface: { width: 120, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 24000, changedAt: 24000, continuity: 80 + extra, speed: 24, before: null, art: evictCache } as never)];
    evictCache.requestVisible(urls, evictOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  assert.equal(evictFetches, fetchesAfterSettle, 'repeated probes stop once predecessor widths are measured');
  evictCache.dispose();

  const leftEdgeCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 8, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  const leftEdgeTraits = [
    { name: 'Narrow 0', art: readyArt('trait://left-edge-0') },
    { name: 'Narrow 1', art: readyArt('trait://left-edge-1') },
  ];
  const leftEdgeModel: TableModel = {
    columns: nonOverflowCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-left', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('2'), iconStripKind: 'traits', icons: leftEdgeTraits } } }],
  };
  const leftEdgeOffscreen = Array.from({ length: 128 }, (_, i) => `offscreen://left-edge-${i}`);
  leftEdgeCache.requestVisible([], leftEdgeOffscreen);
  await new Promise<void>(resolve => setImmediate(resolve));
  const leftEdgeBinding = { family: 'table', settings: nonOverflowSettings, width: 140, height: 110, scrollLeft: 35, scrollTop: 0, options: NO_OPTIONS } as never;
  const leftEdgeLayout = tableLane.layout(leftEdgeModel, leftEdgeBinding, { width: 140, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 25000, speed: 24 } as never);
  for (let round = 0; round < 6; round++) {
    const urls = [...tableLane.visibleArt({ model: leftEdgeModel, layout: leftEdgeLayout, binding: leftEdgeBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 25000, changedAt: 25000, continuity: 90 + round, speed: 24, before: null, art: leftEdgeCache } as never)];
    leftEdgeCache.requestVisible(urls, leftEdgeOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const leftEdgeFinal = [...tableLane.visibleArt({ model: leftEdgeModel, layout: leftEdgeLayout, binding: leftEdgeBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 25000, changedAt: 25000, continuity: 100, speed: 24, before: null, art: leftEdgeCache } as never)];
  assert.deepEqual(leftEdgeFinal, [], 'probing clipped predecessor moves both 8x32 traits behind the pinned edge identically on cold and warm caches');
  leftEdgeCache.dispose();

  const shiftTraits = [
    { name: 'Narrow 0', art: readyArt('trait://shift-0') },
    { name: 'Wide 1', art: readyArt('trait://shift-1') },
  ];
  const shiftModel: TableModel = {
    columns: nonOverflowCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-shift', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('2'), iconStripKind: 'traits', icons: shiftTraits } } }],
  };
  const makeShiftCache = () => new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async url => ({ width: url.endsWith('-0') ? 8 : 32, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  const shiftCold = makeShiftCache();
  const shiftWarm = makeShiftCache();
  shiftCold.requestVisible([], leftEdgeOffscreen);
  shiftWarm.requestVisible(['trait://shift-0', 'trait://shift-1'], leftEdgeOffscreen);
  await new Promise<void>(resolve => setImmediate(resolve));
  const shiftLayout = tableLane.layout(shiftModel, leftEdgeBinding, { width: 140, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 26000, speed: 24 } as never);
  for (let round = 0; round < 6; round++) {
    const urls = [...tableLane.visibleArt({ model: shiftModel, layout: shiftLayout, binding: leftEdgeBinding, surface: { width: 140, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 26000, changedAt: 26000, continuity: 110 + round, speed: 24, before: null, art: shiftCold } as never)];
    shiftCold.requestVisible(urls, leftEdgeOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const traitStartX = (cache: LaneArtCache) => {
    const col = shiftLayout.shape.columns.columns[1]!;
    const leftX = 100 - 35 + 10 + (col.leadInset ?? 0);
    const w0 = iconStripSlotWidth(toArtSlotState(resolveCachedArt(shiftTraits[0]!.art, cache), shiftTraits[0]!.name), 23);
    return leftX + w0 + 5;
  };
  assert.equal(traitStartX(shiftCold), traitStartX(shiftWarm), 'cold cache probes clipped 8x32 predecessor so second 32x32 trait paints at the exact warm-cache x coordinate');
  shiftCold.dispose();
  shiftWarm.dispose();

  const gapPeaceCols: TableColumnSpec[] = [
    { key: 'date', label: 'Date', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'date', pinned: true, hideable: false, minWidth: 160, maxWidth: 160 },
    { key: 'terms', label: 'Terms', as: 'plain', offered: ['plain'], priority: 1, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 120, maxWidth: 120 },
    { key: 'extra', label: 'Extra', as: 'plain', offered: ['plain'], priority: 2, numeric: true, hideable: false, minWidth: 180 },
  ];
  const gapPeaceSettings = tableSettingsOf(undefined, gapPeaceCols, { density: 'normal', sort: { column: 'date', direction: 'descending' } });
  const gapPeaceChild = {
    key: 'gp-detail',
    name: 'Terms',
    detailColumn: 'terms',
    detailEnd: true,
    cells: {
      terms: {
        value: knownValue('Terms'),
        heading: { flag: { tag: 'GER', art: readyArt('flag://gp-head') }, text: knownValue('Germany') },
        flags: [{ tag: 'FRA', art: readyArt('flag://gp-recip'), label: 'France' }],
      },
    },
  };
  const gpTopRow: TableRow = { key: 'gp-top', name: 'Top', cells: { date: { value: knownValue(100) }, terms: { value: knownValue('Top') }, extra: { value: knownValue(1) } }, children: [gapPeaceChild] };
  const gpGapSlot: TableModelSlot = { kind: 'gap', key: 'gp-gap', firstHour: 101, lastHour: 105, reason: 'Collector detached' };
  const gapPeaceBefore: TableModel = {
    columns: gapPeaceCols,
    hour: 100,
    unsorted: true,
    rows: [gpTopRow],
    slots: [gpGapSlot, { kind: 'data', row: gpTopRow }],
  };
  const gapPeaceAfter: TableModel = {
    columns: gapPeaceCols,
    hour: 102,
    unsorted: true,
    rows: [gpTopRow],
    slots: [{ kind: 'data', row: gpTopRow }, gpGapSlot],
  };
  const gpBinding = { family: 'table', settings: gapPeaceSettings, width: 320, height: 180, scrollLeft: 0, scrollTop: 0, expanded: { 'gp-top': true }, options: NO_OPTIONS } as never;
  const gpSurface = { width: 320, height: 180, dpr: 1 };
  const gpPrev = tableLane.layout(gapPeaceBefore, gpBinding, gpSurface as never, TEST_THEME, measure, 100, { previous: null, now: 30000, speed: 24 } as never);
  const gpPrevFrame = { model: gapPeaceBefore, layout: gpPrev, binding: gpBinding, surface: gpSurface, theme: TEST_THEME, hour: 100, now: 30000, changedAt: 30000, continuity: 120, speed: 24, before: null } as never;
  const gpNext = tableLane.layout(gapPeaceAfter, gpBinding, gpSurface as never, TEST_THEME, measure, 102, { previous: gpPrevFrame, now: 30000, speed: 24 } as never);
  const gpUrls = [...tableLane.visibleArt({ model: gapPeaceAfter, layout: gpNext, binding: gpBinding, surface: gpSurface, theme: TEST_THEME, hour: 102, now: 30020, changedAt: 30000, continuity: 120, speed: 24, before: gpPrevFrame } as never)];
  assert.ok(gpUrls.includes('flag://gp-head') && gpUrls.includes('flag://gp-recip'), `Peace Deals detail flags inside pinned region stay in visibleArt while gliding across a recording gap (got ${JSON.stringify(gpUrls)})`);

  const overflowLabelCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 106, maxWidth: 106 },
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'icons', sortable: false, hideable: false, minWidth: 90, maxWidth: 90 },
    { key: 'pad', label: 'Pad', as: 'plain', offered: ['plain'], priority: 2, numeric: true, hideable: false, minWidth: 120 },
  ];
  const overflowLabelSettings = tableSettingsOf(undefined, overflowLabelCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const overflowTraits = [
    { name: 'O0', art: readyArt('trait://ovf-0') },
    { name: 'O1', art: readyArt('trait://ovf-1', { frame: defaultArtFrame(32, 32) }) },
    { name: 'O2', art: readyArt('trait://ovf-2', { frame: defaultArtFrame(32, 32) }) },
    { name: 'O3', art: readyArt('trait://ovf-3', { frame: defaultArtFrame(32, 32) }) },
  ];
  const overflowLabelModel: TableModel = {
    columns: overflowLabelCols,
    hour: 100,
    unsorted: true,
    rows: [{ key: 'gen-ovf', name: 'General', cells: { name: { value: knownValue('General') }, traits: { value: knownValue('4'), iconStripKind: 'traits', icons: overflowTraits } } }],
  };
  const makeOvfCache = () => new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async url => ({ width: url.endsWith('-0') ? 8 : 32, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  const ovfCold = makeOvfCache();
  const ovfWarm = makeOvfCache();
  ovfCold.requestVisible([], leftEdgeOffscreen);
  ovfWarm.requestVisible(overflowTraits.map(t => t.art.url), leftEdgeOffscreen);
  await new Promise<void>(resolve => setImmediate(resolve));
  const ovfBinding = { family: 'table', settings: overflowLabelSettings, width: 150, height: 110, scrollLeft: 63, scrollTop: 0, options: NO_OPTIONS } as never;
  const ovfLayout = tableLane.layout(overflowLabelModel, ovfBinding, { width: 150, height: 110, dpr: 1 } as never, TEST_THEME, measure, 100, { previous: null, now: 31000, speed: 24 } as never);
  for (let round = 0; round < 6; round++) {
    const urls = [...tableLane.visibleArt({ model: overflowLabelModel, layout: ovfLayout, binding: ovfBinding, surface: { width: 150, height: 110, dpr: 1 }, theme: TEST_THEME, hour: 100, now: 31000, changedAt: 31000, continuity: 130 + round, speed: 24, before: null, art: ovfCold } as never)];
    ovfCold.requestVisible(urls, leftEdgeOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const ovfLabelX = (cache: LaneArtCache) => {
    const col = ovfLayout.shape.columns.columns[1]!;
    const leftX = ovfLayout.shape.columns.pinnedWidth - 63 + 10 + (col.leadInset ?? 0);
    const rightX = ovfLayout.shape.columns.pinnedWidth - 63 + col.width - 10 - (col.trailInset ?? 0);
    let labelX: number | null = null;
    const rawCtx = instrumentedContext().ctx;
    const captureCtx = Object.assign(Object.create(rawCtx), {
      fillText(text: string, x: number, y: number) {
        if (text === '+2') labelX = x;
        return rawCtx.fillText(text, x, y);
      },
    }) as unknown as OffscreenCanvasRenderingContext2D;
    drawIconStrip(
      captureCtx,
      leftX,
      rightX,
      18,
      overflowTraits.map(item => ({ state: toArtSlotState(resolveCachedArt(item.art, cache), item.name), name: item.name })),
      TEST_THEME,
      { kind: 'traits' },
    );
    return labelX;
  };
  assert.equal(ovfLabelX(ovfCold), ovfLabelX(ovfWarm), 'clipped predecessor is probed so +2 overflow label paints at the exact warm-cache x coordinate');
  assert.notEqual(ovfLabelX(ovfCold), null, '+2 overflow label was painted by drawIconStrip');
  ovfCold.dispose();
  ovfWarm.dispose();

  const incDetailCols: TableColumnSpec[] = [
    { key: 'terms', label: 'Terms', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
  ];
  const incDetailSettings = tableSettingsOf(undefined, incDetailCols, { density: 'normal', sort: { column: 'terms', direction: 'descending' } });
  const incDetailRow: TableRow = {
    key: 'top-detail',
    name: 'Terms',
    detailColumn: 'terms',
    detailEnd: true,
    cells: {
      terms: {
        value: knownValue('Terms'),
        heading: { flag: { tag: 'GER', art: readyArt('flag://inc-head') }, text: knownValue('Germany') },
        flags: Array.from({ length: 18 }, (_, i) => ({ tag: `C${i}`, art: readyArt(`flag://inc-term-${i}`), label: `Territory ${i}` })),
      },
    },
  };
  const incDetailBase: TableModel = { columns: incDetailCols, hour: 100, unsorted: true, rows: [incDetailRow] };
  const incDetailNext: TableModel = { columns: incDetailCols, hour: 101, unsorted: true, rows: [incDetailRow], changedKeys: new Set() };
  const incDetailBinding = { family: 'table', settings: incDetailSettings, width: 320, height: 110, scrollLeft: 0, scrollTop: 40, options: NO_OPTIONS } as never;
  const incDetailSurface = { width: 320, height: 110, dpr: 1 };
  const incFresh = tableLane.layout(incDetailBase, incDetailBinding, incDetailSurface as never, TEST_THEME, measure, 100, { previous: null, now: 32000, speed: 24 } as never);
  const incPrevFrame = { model: incDetailBase, layout: incFresh, binding: incDetailBinding, surface: incDetailSurface, theme: TEST_THEME, hour: 100, now: 32000, changedAt: 32000, continuity: 140, speed: 24, before: null } as never;
  const incRefreshed = tableLane.layout(incDetailNext, incDetailBinding, incDetailSurface as never, TEST_THEME, measure, 101, { previous: incPrevFrame, now: 32000, speed: 24 } as never);
  assert.equal(incRefreshed.heights(0), incFresh.heights(0), 'incremental refresh preserves top-level detail row height');
  const incFreshUrls = [...tableLane.visibleArt({ model: incDetailBase, layout: incFresh, binding: incDetailBinding, surface: incDetailSurface, theme: TEST_THEME, hour: 100, now: 32000, changedAt: 32000, continuity: 140, speed: 24, before: null } as never)];
  const incRefreshedUrls = [...tableLane.visibleArt({ model: incDetailNext, layout: incRefreshed, binding: incDetailBinding, surface: incDetailSurface, theme: TEST_THEME, hour: 101, now: 32000, changedAt: 32000, continuity: 140, speed: 24, before: incPrevFrame } as never)];
  assert.ok(incFreshUrls.length > 0, 'scrolled top-level detail row exposes flags');
  assert.deepEqual(incRefreshedUrls, incFreshUrls, 'incremental refresh preserves visible flags on a scrolled top-level detail row');

  const reorderDetailCols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  const reorderDetailSettings = tableSettingsOf(undefined, reorderDetailCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const tallDetailRow = (score: number): TableRow => ({
    key: 'tall-detail',
    name: 'Tall Detail',
    detailColumn: 'country',
    detailEnd: true,
    cells: {
      country: {
        value: knownValue('Tall Detail'),
        heading: { flag: { tag: 'GER', art: readyArt('flag://tall-head') }, text: knownValue('Germany') },
        flags: Array.from({ length: 48 }, (_, i) => ({ tag: `T${i}`, art: readyArt(`flag://tall-term-${i}`), label: `Long Territory Name ${i}` })),
      },
      score: { value: knownValue(score) },
    },
  });
  const plainRow = (score: number): TableRow => ({
    key: 'plain-row',
    name: 'Plain',
    cells: {
      country: { value: knownValue('Plain'), country: 'FRA', iconSlot: true, art: readyArt('flag://plain-mover') },
      score: { value: knownValue(score) },
    },
  });
  const rdSurface = { width: 320, height: 100, dpr: 1 };
  const rdBinding = { family: 'table', settings: reorderDetailSettings, width: 320, height: 100, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const rdBefore: TableModel = { columns: reorderDetailCols, hour: 100, rows: [plainRow(100), tallDetailRow(50)] };
  const rdAfter: TableModel = { columns: reorderDetailCols, hour: 101, rows: [plainRow(10), tallDetailRow(200)] };
  const rdPrev = tableLane.layout(rdBefore, rdBinding, rdSurface as never, TEST_THEME, measure, 100, { previous: null, now: 33000, speed: 24 } as never);
  const rdPrevFrame = { model: rdBefore, layout: rdPrev, binding: rdBinding, surface: rdSurface, theme: TEST_THEME, hour: 100, now: 33000, changedAt: 33000, continuity: 150, speed: 24, before: null } as never;
  const rdNext = tableLane.layout(rdAfter, rdBinding, rdSurface as never, TEST_THEME, measure, 101, { previous: rdPrevFrame, now: 33000, speed: 24 } as never);
  const rdStartUrls = [...tableLane.visibleArt({ model: rdAfter, layout: rdNext, binding: rdBinding, surface: rdSurface, theme: TEST_THEME, hour: 101, now: 33001, changedAt: 33000, continuity: 150, speed: 24, before: rdPrevFrame } as never)];
  assert.ok(rdStartUrls.includes('flag://plain-mover'), `ordinary row gliding below a tall detail row starts at its prior top y=0 and keeps its flag in visibleArt (got ${JSON.stringify(rdStartUrls)})`);

  const flatRowsBase: TableRow[] = Array.from({ length: 1500 }, (_, i) =>
    i === 0
      ? tallDetailRow(1500)
      : { key: `r-${i}`, name: `Row ${i}`, cells: { country: { value: knownValue(`Row ${i}`) }, score: { value: knownValue(1500 - i) } } },
  );
  const flatRowsAfter: TableRow[] = [
    { key: 'tall-detail', name: 'Row 0', cells: { country: { value: knownValue('Row 0') }, score: { value: knownValue(1500) } } },
    ...flatRowsBase.slice(1),
  ];
  const flatModel0: TableModel = { columns: reorderDetailCols, hour: 100, rows: flatRowsBase };
  const flatModel1: TableModel = { columns: reorderDetailCols, hour: 101, rows: flatRowsAfter, changedKeys: new Set(['tall-detail']) };
  const flatLayout0 = tableLane.layout(flatModel0, rdBinding, rdSurface as never, TEST_THEME, measure, 100, { previous: null, now: 34000, speed: 24 } as never);
  assert.notEqual(flatLayout0.shape.rowGeom, null, 'table with a detail row uses variable rowGeom');
  const flatPrev0 = { model: flatModel0, layout: flatLayout0, binding: rdBinding, surface: rdSurface, theme: TEST_THEME, hour: 100, now: 34000, changedAt: 34000, continuity: 160, speed: 24, before: null } as never;
  const flatLayout1 = tableLane.layout(flatModel1, rdBinding, rdSurface as never, TEST_THEME, measure, 101, { previous: flatPrev0, now: 34000, speed: 24 } as never);
  assert.equal(flatLayout1.shape.rowGeom, null, 'removing the last detail row incrementally returns the 1,500-row table to flat rowGeom === null');

  const growDetailRow = (termCount: number): TableRow => ({
    key: 'grow-detail',
    name: 'Terms',
    detailColumn: 'country',
    detailEnd: true,
    cells: {
      country: {
        value: knownValue('Terms'),
        flags: Array.from({ length: termCount }, (_, i) => ({ tag: `G${i}`, art: readyArt(`flag://grow-t-${i}`), label: `Long Territory Name ${i}` })),
      },
      score: { value: knownValue(200) },
    },
  });
  const growFollow: TableRow = {
    key: 'grow-follow',
    name: 'Follow',
    cells: {
      country: { value: knownValue('Follow'), country: 'FRA', iconSlot: true, art: readyArt('flag://grow-follow') },
      score: { value: knownValue(100) },
    },
  };
  const growModel0: TableModel = { columns: reorderDetailCols, hour: 100, unsorted: true, rows: [growDetailRow(1), growFollow] };
  const growModel1: TableModel = { columns: reorderDetailCols, hour: 101, unsorted: true, rows: [growDetailRow(2), growFollow] };
  const growModel2: TableModel = { columns: reorderDetailCols, hour: 102, unsorted: true, rows: [growDetailRow(3), growFollow] };
  const growSurface = { width: 320, height: 80, dpr: 1 };
  const growBinding = { family: 'table', settings: reorderDetailSettings, width: 320, height: 80, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const growL0 = tableLane.layout(growModel0, growBinding, growSurface as never, TEST_THEME, measure, 100, { previous: null, now: 35000, speed: 24 } as never);
  const growF0 = { model: growModel0, layout: growL0, binding: growBinding, surface: growSurface, theme: TEST_THEME, hour: 100, now: 35000, changedAt: 35000, continuity: 170, speed: 24, before: null } as never;
  const growL1 = tableLane.layout(growModel1, growBinding, growSurface as never, TEST_THEME, measure, 101, { previous: growF0, now: 35000, speed: 24 } as never);
  const growF1At30 = { model: growModel1, layout: growL1, binding: growBinding, surface: growSurface, theme: TEST_THEME, hour: 101, now: 35030, changedAt: 35000, continuity: 170, speed: 24, before: growF0 } as never;
  const probeY = growL1.tops(1) - 1;
  const growHitBefore = tableLane.hit(growF1At30, [40, probeY]);
  const growUrlsBefore = [...tableLane.visibleArt(growF1At30)];
  const growL2 = tableLane.layout(growModel2, growBinding, growSurface as never, TEST_THEME, measure, 102, { previous: growF1At30, now: 35030, speed: 24 } as never);
  const growF2At30 = { model: growModel2, layout: growL2, binding: growBinding, surface: growSurface, theme: TEST_THEME, hour: 102, now: 35030, changedAt: 35030, continuity: 170, speed: 24, before: growF1At30 } as never;
  const growHitAfter = tableLane.hit(growF2At30, [40, probeY]);
  const growUrlsAfter = [...tableLane.visibleArt(growF2At30)];
  assert.equal(growHitBefore, 'row:grow-follow', 'pre-refresh mid-glide hit lands on the following country row');
  assert.equal(growHitAfter, 'row:grow-follow', 'refreshing mid-glide preserves the painted position so the same pointer still hits the following country row');
  assert.ok(growUrlsBefore.includes('flag://grow-follow') && growUrlsAfter.includes('flag://grow-follow'), 'refreshing mid-glide keeps the following country flag visible without jumping');

  const pagedDetail = growDetailRow(2);
  const pagedStationary: TableRow = {
    key: 'paged-stay',
    name: 'Stay',
    cells: {
      country: { value: knownValue('Stay'), country: 'USA', iconSlot: true, art: readyArt('flag://paged-stay') },
      score: { value: knownValue(50) },
    },
  };
  const pagedMid: TableRow = {
    key: 'paged-mid',
    name: 'Mid',
    cells: { country: { value: knownValue('Mid') }, score: { value: knownValue(60) } },
  };
  const pagedBinding = { family: 'table', settings: reorderDetailSettings, width: 320, height: 140, scrollLeft: 0, scrollTop: 70, options: NO_OPTIONS } as never;
  const pagedSurface = { width: 320, height: 140, dpr: 1 };
  const pagedModel0: TableModel = { columns: reorderDetailCols, hour: 100, unsorted: true, windowOffset: 0, totalRows: 3, rows: [pagedDetail, pagedMid, pagedStationary] };
  const pagedModel1: TableModel = { columns: reorderDetailCols, hour: 101, unsorted: true, windowOffset: 1, totalRows: 3, rows: [pagedMid, pagedStationary] };
  const pagedL0 = tableLane.layout(pagedModel0, pagedBinding, pagedSurface as never, TEST_THEME, measure, 100, { previous: null, now: 36000, speed: 24 } as never);
  const pagedF0 = { model: pagedModel0, layout: pagedL0, binding: pagedBinding, surface: pagedSurface, theme: TEST_THEME, hour: 100, now: 36000, changedAt: 36000, continuity: 180, speed: 24, before: null } as never;
  const pagedL1 = tableLane.layout(pagedModel1, pagedBinding, pagedSurface as never, TEST_THEME, measure, 101, { previous: pagedF0, now: 36000, speed: 24 } as never);
  assert.equal(pagedL0.tops(2), pagedL1.tops(1), 'stationary row keeps identical absolute top across paged window shift past a detail row');
  const pagedF1 = { model: pagedModel1, layout: pagedL1, binding: pagedBinding, surface: pagedSurface, theme: TEST_THEME, hour: 101, now: 36005, changedAt: 36000, continuity: 180, speed: 24, before: pagedF0 } as never;
  const pagedHit = tableLane.hit(pagedF1, [40, pagedL1.tops(1) - pagedL1.tops(0) - pagedL1.localScrollTop + 18]);
  const pagedUrls = [...tableLane.visibleArt(pagedF1)];
  assert.equal(pagedHit, 'row:paged-stay', 'shifting paged window past a detail row does not animate stationary rows off their hit target');
  assert.ok(pagedUrls.includes('flag://paged-stay'), `stationary row flag stays in visibleArt across paged window shift (got ${JSON.stringify(pagedUrls)})`);

  const speedModel0: TableModel = { columns: reorderDetailCols, hour: 100, unsorted: true, rows: [growDetailRow(1), growFollow] };
  const speedModel1: TableModel = { columns: reorderDetailCols, hour: 101, unsorted: true, rows: [growDetailRow(6), growFollow] };
  const speedModel2: TableModel = { columns: reorderDetailCols, hour: 102, unsorted: true, rows: [growDetailRow(9), growFollow] };
  const speedSurface = { width: 320, height: 240, dpr: 1 };
  const speedBinding = { family: 'table', settings: reorderDetailSettings, width: 320, height: 240, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const speedL0 = tableLane.layout(speedModel0, speedBinding, speedSurface as never, TEST_THEME, measure, 100, { previous: null, now: 37000, speed: 24 } as never);
  const speedF0 = { model: speedModel0, layout: speedL0, binding: speedBinding, surface: speedSurface, theme: TEST_THEME, hour: 100, now: 37000, changedAt: 37000, continuity: 190, speed: 24, before: null } as never;
  const speedL1 = tableLane.layout(speedModel1, speedBinding, speedSurface as never, TEST_THEME, measure, 101, { previous: speedF0, now: 37000, speed: 24 } as never);
  const speedF1At90 = { model: speedModel1, layout: speedL1, binding: speedBinding, surface: speedSurface, theme: TEST_THEME, hour: 101, now: 37090, changedAt: 37000, continuity: 190, speed: 24, before: speedF0 } as never;
  const speedL2 = tableLane.layout(speedModel2, speedBinding, speedSurface as never, TEST_THEME, measure, 102, { previous: speedF1At90, now: 37090, speed: 168 } as never);
  const speedF2At90 = { model: speedModel2, layout: speedL2, binding: speedBinding, surface: speedSurface, theme: TEST_THEME, hour: 102, now: 37090, changedAt: 37090, continuity: 190, speed: 168, before: speedF1At90 } as never;
  const hitYs = (f: never) => {
    const ys: number[] = [];
    for (let y = 0; y < 240; y++) if (tableLane.hit(f, [40, y]) === 'row:grow-follow') ys.push(y);
    return ys;
  };
  const beforeSpeedYs = hitYs(speedF1At90);
  const afterSpeedYs = hitYs(speedF2At90);
  assert.ok(beforeSpeedYs.length > 0, 'following row has a hit range at 90 ms before speed change');
  assert.deepEqual(afterSpeedYs, beforeSpeedYs, 'changing playback speed from 24 to 168 h/s mid-glide keeps the following row hit range continuous');

  const snapRow = (key: string, flag: string, score: number): TableRow => ({
    key,
    name: key,
    cells: {
      country: { value: knownValue(key), country: key, iconSlot: true, art: readyArt(flag) },
      score: { value: knownValue(score) },
    },
  });
  const snapSurface = { width: 320, height: 36, dpr: 1 };
  const snapBinding = { family: 'table', settings: reorderDetailSettings, width: 320, height: 36, scrollLeft: 0, scrollTop: 0, options: NO_OPTIONS } as never;
  const snapM0: TableModel = { columns: reorderDetailCols, hour: 100, rows: [snapRow('r0', 'flag://snap-r0', 200), snapRow('r1', 'flag://snap-r1', 100)] };
  const snapM1: TableModel = { columns: reorderDetailCols, hour: 101, rows: [snapRow('r0', 'flag://snap-r0', 50), snapRow('r1', 'flag://snap-r1', 250)] };
  const snapL0 = tableLane.layout(snapM0, snapBinding, snapSurface as never, TEST_THEME, measure, 100, { previous: null, now: 37500, speed: 24 } as never);
  const snapF0 = { model: snapM0, layout: snapL0, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 100, now: 37500, changedAt: 37500, continuity: 195, speed: 24, before: null } as never;
  const snapL1 = tableLane.layout(snapM1, snapBinding, snapSurface as never, TEST_THEME, measure, 101, { previous: snapF0, now: 37500, speed: 24 } as never);
  const snapF1Slow = { model: snapM1, layout: snapL1, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 101, now: 37502, changedAt: 37500, continuity: 195, speed: 24, before: snapF0 } as never;
  assert.equal(tableLane.hit(snapF1Slow, [40, 18]), 'row:r0', 'at 24 h/s, mid-glide hit still lands on outgoing row r0');
  assert.deepEqual([...tableLane.visibleArt(snapF1Slow)], ['flag://snap-r0'], 'at 24 h/s, mid-glide visibleArt includes outgoing row r0');
  const snapF1Fast = { model: snapM1, layout: snapL1, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 101, now: 37502, changedAt: 37500, continuity: 195, speed: 720, before: snapF0 } as never;
  assert.equal(tableLane.hit(snapF1Fast, [40, 18]), 'row:r1', 'jumping playback speed to 720 h/s snaps hit testing to r1');
  assert.deepEqual([...tableLane.visibleArt(snapF1Fast)], ['flag://snap-r1'], 'jumping playback speed to 720 h/s snaps visibleArt to r1 in lockstep with paint and hit');
  const snapF1BackSlow = { model: snapM1, layout: snapL1, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 101, now: 37505, changedAt: 37500, continuity: 195, speed: 24, before: snapF0 } as never;
  assert.equal(tableLane.hit(snapF1BackSlow, [40, 18]), 'row:r1', 'slowing back to 24 h/s on the same snapped frame never revives the discarded glide');
  assert.deepEqual([...tableLane.visibleArt(snapF1BackSlow)], ['flag://snap-r1'], 'slowing back to 24 h/s on the same snapped frame keeps visibleArt on r1');
  const snapM2: TableModel = { columns: reorderDetailCols, hour: 102, rows: [snapRow('r0', 'flag://snap-r0', 50), snapRow('r1', 'flag://snap-r1', 250)] };
  const snapL2 = tableLane.layout(snapM2, snapBinding, snapSurface as never, TEST_THEME, measure, 102, { previous: snapF1Fast, now: 37504, speed: 720 } as never);
  const snapF2Fast = { model: snapM2, layout: snapL2, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 102, now: 37504, changedAt: 37504, continuity: 195, speed: 720, before: snapF1Fast } as never;
  assert.equal(tableLane.hit(snapF2Fast, [40, 18]), 'row:r1', 'projected at 720 h/s stays snapped on r1');
  const snapF2BackSlow = { model: snapM2, layout: snapL2, binding: snapBinding, surface: snapSurface, theme: TEST_THEME, hour: 102, now: 37505, changedAt: 37504, continuity: 195, speed: 24, before: snapF1Fast } as never;
  assert.equal(tableLane.hit(snapF2BackSlow, [40, 18]), 'row:r1', 'slowing back to 24 h/s after snapping at 720 h/s keeps hit testing on r1');
  assert.deepEqual([...tableLane.visibleArt(snapF2BackSlow)], ['flag://snap-r1'], 'slowing back to 24 h/s after snapping at 720 h/s keeps visibleArt on r1');

  const trimCols: TableColumnSpec[] = [
    { key: 'kind', label: 'Kind', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'kind', minWidth: 36, maxWidth: 36 },
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 1, text: true, cellKind: 'item' },
  ];
  const trimSettings = tableSettingsOf(undefined, trimCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const makeTrimBitmap = () => {
    const buf = new Uint8ClampedArray(64 * 64 * 4);
    for (let y = 31; y < 33; y++) {
      for (let x = 0; x < 64; x++) buf[(y * 64 + x) * 4 + 3] = 255;
    }
    return { width: 64, height: 64, data: buf, close() {} } as unknown as ImageBitmap;
  };
  let trimFetches = 0;
  const trimCache = new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async () => {
      trimFetches++;
      return makeTrimBitmap();
    },
  });
  const savedOffscreen = globalThis.OffscreenCanvas;
  globalThis.OffscreenCanvas = class {
    width: number;
    height: number;
    private buf: Uint8ClampedArray;
    constructor(w: number, h: number) { this.width = w; this.height = h; this.buf = new Uint8ClampedArray(w * h * 4); }
    getContext() {
      return {
        drawImage: (src: { data?: Uint8ClampedArray }) => { if (src.data) this.buf.set(src.data); },
        getImageData: () => ({ data: this.buf }),
      };
    }
  } as unknown as typeof OffscreenCanvas;
  try {
    const trimSurface = { width: 200, height: 36, dpr: 1 };
    const trimBinding = { family: 'table', settings: trimSettings, width: 200, height: 36, scrollLeft: 0, scrollTop: 25, options: NO_OPTIONS } as never;
    const makeTrimModel = (id: number): TableModel => ({
      columns: trimCols,
      hour: 100,
      unsorted: true,
      rows: [
        { key: `tr-${id}`, name: `Trim ${id}`, cells: { kind: { value: knownValue('K'), art: readyArt(`sprite://trim-evict-${id}`), artShape: 'drawn', iconSlot: true }, name: { value: knownValue(`Trim ${id}`) } } },
        { key: 'tr-pad', name: 'Pad', cells: { kind: { value: knownValue('P') }, name: { value: knownValue('Pad') } } },
      ],
    });
    for (let i = 0; i < 129; i++) {
      const m = makeTrimModel(i);
      const l = tableLane.layout(m, trimBinding, trimSurface as never, TEST_THEME, measure, 100, { previous: null, now: 38000, speed: 24 } as never);
      const urls = [...tableLane.visibleArt({ model: m, layout: l, binding: trimBinding, surface: trimSurface, theme: TEST_THEME, hour: 100, now: 38000, changedAt: 38000, continuity: 200 + i, speed: 24, before: null, art: trimCache } as never)];
      trimCache.requestVisible(urls, []);
      await new Promise<void>(resolve => setImmediate(resolve));
      tableLane.visibleArt({ model: m, layout: l, binding: trimBinding, surface: trimSurface, theme: TEST_THEME, hour: 100, now: 38000, changedAt: 38000, continuity: 400 + i, speed: 24, before: null, art: trimCache } as never);
    }
    trimCache.requestVisible([], []);
    assert.equal(trimFetches, 129, 'all 129 clipped drawn sprites are initially fetched once before trimming reveals they sit above the viewport');
    assert.equal(trimCache.stateOfUrl('sprite://trim-evict-0'), 'unrequested', 'first trimmed sprite bitmap is evicted once 129 sprites exceed the 128-entry cache');
    const m0 = makeTrimModel(0);
    const l0 = tableLane.layout(m0, trimBinding, trimSurface as never, TEST_THEME, measure, 100, { previous: null, now: 38000, speed: 24 } as never);
    const evictedUrls = [...tableLane.visibleArt({ model: m0, layout: l0, binding: trimBinding, surface: trimSurface, theme: TEST_THEME, hour: 100, now: 38000, changedAt: 38000, continuity: 600, speed: 24, before: null, art: trimCache } as never)];
    assert.deepEqual(evictedUrls, [], 'evicted trimmed sprite retains measured bounds and is not re-fetched when clipped');
    trimCache.requestVisible(evictedUrls, []);
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(trimFetches, 129, 'stationary clipped trimmed sprite causes zero additional fetches after eviction');
  } finally {
    globalThis.OffscreenCanvas = savedOffscreen;
    trimCache.dispose();
  }

  const wideTraitCols: TableColumnSpec[] = [
    { key: 'traits', label: 'Traits', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'icons', sortable: false, minWidth: 80, maxWidth: 80 },
  ];
  const wideTraitSettings = tableSettingsOf(undefined, wideTraitCols, { density: 'normal', sort: { column: 'traits', direction: 'descending' } });
  const wideTraitModel: TableModel = {
    columns: wideTraitCols,
    hour: 100,
    unsorted: true,
    rows: [
      { key: 'wt-0', name: 'General', cells: { traits: { value: knownValue('1'), iconStripKind: 'traits', icons: [{ name: 'Wide', art: readyArt('trait://wide-64x8') }] } } },
      { key: 'wt-1', name: 'Pad', cells: { traits: { value: knownValue('0') } } },
    ],
  };
  const wideTraitCache = new LaneArtCache({
    maxEntries: 8,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 64, height: 8, close() {} }) as unknown as ImageBitmap,
  });
  const wtSurface = { width: 80, height: 36, dpr: 1 };
  const wtBinding = { family: 'table', settings: wideTraitSettings, width: 80, height: 36, scrollLeft: 0, scrollTop: 25, options: NO_OPTIONS } as never;
  const wtLayout = tableLane.layout(wideTraitModel, wtBinding, wtSurface as never, TEST_THEME, measure, 100, { previous: null, now: 39000, speed: 24 } as never);
  const wtBefore = [...tableLane.visibleArt({ model: wideTraitModel, layout: wtLayout, binding: wtBinding, surface: wtSurface, theme: TEST_THEME, hour: 100, now: 39000, changedAt: 39000, continuity: 700, speed: 24, before: null, art: wideTraitCache } as never)];
  assert.deepEqual(wtBefore, ['trait://wide-64x8'], 'before decode, 64x8 trait icon is probed conservatively');
  wideTraitCache.requestVisible(wtBefore, []);
  await new Promise<void>(resolve => setImmediate(resolve));
  const wtAfter = [...tableLane.visibleArt({ model: wideTraitModel, layout: wtLayout, binding: wtBinding, surface: wtSurface, theme: TEST_THEME, hour: 100, now: 39000, changedAt: 39000, continuity: 701, speed: 24, before: null, art: wideTraitCache } as never)];
  assert.deepEqual(wtAfter, [], 'after decoding 64x8 bitmap, fitted vertical bounds above the viewport drop the clipped trait icon from visibleArt');
  wideTraitCache.dispose();

  const suCols: TableColumnSpec[] = [
    { key: 'skill', label: 'Skill', as: 'plain', offered: ['plain'], priority: 0, numeric: true, cellKind: 'skill', skillIcon: 'badge://wide-skill-64x8', minWidth: 80, maxWidth: 80 },
    { key: 'cost', label: 'Cost', as: 'plain', offered: ['plain'], priority: 1, numeric: true, minWidth: 80, maxWidth: 80 },
  ];
  const suSettings = tableSettingsOf(undefined, suCols, { density: 'normal', sort: { column: 'skill', direction: 'descending' } });
  const suModel: TableModel = {
    columns: suCols,
    hour: 100,
    unsorted: true,
    rows: [
      { key: 'su-0', name: 'Row 0', cells: { skill: { value: knownValue(5) }, cost: { value: knownValue(50), unitArt: readyArt('unit://wide-unit-64x8') } } },
      { key: 'su-1', name: 'Pad', cells: { skill: { value: notRecorded('None') }, cost: { value: knownValue(10) } } },
    ],
  };
  const suCache = new LaneArtCache({
    maxEntries: 8,
    maxConcurrent: 32,
    fetchBitmap: async () => ({ width: 64, height: 8, close() {} }) as unknown as ImageBitmap,
  });
  const suSurface = { width: 160, height: 36, dpr: 1 };
  const suBinding = { family: 'table', settings: suSettings, width: 160, height: 36, scrollLeft: 0, scrollTop: 23, options: NO_OPTIONS } as never;
  const suLayout = tableLane.layout(suModel, suBinding, suSurface as never, TEST_THEME, measure, 100, { previous: null, now: 39500, speed: 24 } as never);
  const suBefore = [...tableLane.visibleArt({ model: suModel, layout: suLayout, binding: suBinding, surface: suSurface, theme: TEST_THEME, hour: 100, now: 39500, changedAt: 39500, continuity: 710, speed: 24, before: null, art: suCache } as never)];
  assert.deepEqual(suBefore.sort(), ['badge://wide-skill-64x8', 'unit://wide-unit-64x8'], 'before decode, skill and unit icons are probed conservatively');
  suCache.requestVisible(suBefore, []);
  await new Promise<void>(resolve => setImmediate(resolve));
  const suAfter = [...tableLane.visibleArt({ model: suModel, layout: suLayout, binding: suBinding, surface: suSurface, theme: TEST_THEME, hour: 100, now: 39500, changedAt: 39500, continuity: 711, speed: 24, before: null, art: suCache } as never)];
  assert.deepEqual(suAfter, [], 'after decoding 64x8 bitmaps, clipped skill and unit art are dropped from visibleArt');
  suCache.dispose();

  const medalCountCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item', pinned: true, hideable: false, minWidth: 260, maxWidth: 260 },
    { key: 'medals', label: 'Medals', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'icons', sortable: false, hideable: false, minWidth: 160, maxWidth: 160 },
    { key: 'pad', label: 'Pad', as: 'plain', offered: ['plain'], priority: 2, numeric: true, hideable: false, minWidth: 120 },
  ];
  const medalCountSettings = tableSettingsOf(undefined, medalCountCols, { density: 'normal', sort: { column: 'name', direction: 'descending' } });
  const medalCountIcons = [
    { name: 'Hidden', art: readyArt('medal://cnt-0') },
    { name: 'Wide', art: readyArt('medal://cnt-1'), count: 2 },
  ];
  const medalCountModel: TableModel = {
    columns: medalCountCols,
    hour: 100,
    unsorted: true,
    rows: [
      { key: 'mc-0', name: 'Div', cells: { name: { value: knownValue('Div') }, medals: { value: knownValue('2'), iconStripKind: 'medals', icons: medalCountIcons }, pad: { value: knownValue(1) } } },
      { key: 'mc-1', name: 'Pad', cells: { name: { value: knownValue('Pad') }, medals: { value: knownValue('0') }, pad: { value: knownValue(2) } } },
    ],
  };
  const makeMcCache = () => new LaneArtCache({
    maxEntries: 128,
    maxConcurrent: 32,
    fetchBitmap: async url => ({ width: url.endsWith('-0') ? 4 : 256, height: 32, close() {} }) as unknown as ImageBitmap,
  });
  const mcCold = makeMcCache();
  const mcWarm = makeMcCache();
  mcCold.requestVisible([], leftEdgeOffscreen);
  mcWarm.requestVisible(['medal://cnt-0', 'medal://cnt-1'], leftEdgeOffscreen);
  await new Promise<void>(resolve => setImmediate(resolve));
  const mcSurface = { width: 360, height: 36, dpr: 1 };
  const mcBinding = { family: 'table', settings: medalCountSettings, width: 360, height: 36, scrollLeft: 42, scrollTop: 20, options: NO_OPTIONS } as never;
  const mcLayout = tableLane.layout(medalCountModel, mcBinding, mcSurface as never, TEST_THEME, measure, 100, { previous: null, now: 40000, speed: 24 } as never);
  for (let round = 0; round < 6; round++) {
    const urls = [...tableLane.visibleArt({ model: medalCountModel, layout: mcLayout, binding: mcBinding, surface: mcSurface, theme: TEST_THEME, hour: 100, now: 40000, changedAt: 40000, continuity: 720 + round, speed: 24, before: null, art: mcCold } as never)];
    mcCold.requestVisible(urls, leftEdgeOffscreen);
    await new Promise<void>(resolve => setImmediate(resolve));
  }
  const mcLabelX = (cache: LaneArtCache) => {
    const col = mcLayout.shape.columns.columns[1]!;
    const leftX = mcLayout.shape.columns.pinnedWidth - 42 + 10 + (col.leadInset ?? 0);
    const rightX = mcLayout.shape.columns.pinnedWidth - 42 + col.width - 10 - (col.trailInset ?? 0);
    let labelX: number | null = null;
    const rawCtx = instrumentedContext().ctx;
    const captureCtx = Object.assign(Object.create(rawCtx), {
      fillText(text: string, x: number, y: number) {
        if (text === '\u00d72') labelX = x;
        return rawCtx.fillText(text, x, y);
      },
    }) as unknown as OffscreenCanvasRenderingContext2D;
    drawIconStrip(
      captureCtx,
      leftX,
      rightX,
      18,
      medalCountIcons.map(item => ({ state: toArtSlotState(resolveCachedArt(item.art, cache), item.name), name: item.name, count: item.count })),
      TEST_THEME,
      { kind: 'medals' },
    );
    return labelX;
  };
  assert.notEqual(mcLabelX(mcCold), null, '×2 count badge was painted by drawIconStrip');
  assert.equal(mcLabelX(mcCold), mcLabelX(mcWarm), 'probing hidden 4x32 predecessor keeps vertically-clipped medal ×2 badge at exact warm-cache x');
  const mcClippedBinding = { family: 'table', settings: medalCountSettings, width: 360, height: 36, scrollLeft: 42, scrollTop: 31, options: NO_OPTIONS } as never;
  const mcClippedIcons = [
    { name: 'Hidden', art: readyArt('medal://cnt-clip-0') },
    { name: 'Wide', art: readyArt('medal://cnt-clip-1', { frame: defaultArtFrame(256, 32) }), count: 2 },
  ];
  const mcClippedModel: TableModel = {
    columns: medalCountCols,
    hour: 100,
    unsorted: true,
    rows: [
      { key: 'mc-0', name: 'Div', cells: { name: { value: knownValue('Div') }, medals: { value: knownValue('2'), iconStripKind: 'medals', icons: mcClippedIcons }, pad: { value: knownValue(1) } } },
      { key: 'mc-1', name: 'Pad', cells: { name: { value: knownValue('Pad') }, medals: { value: knownValue('0') }, pad: { value: knownValue(2) } } },
    ],
  };
  const mcClippedLayout = tableLane.layout(mcClippedModel, mcClippedBinding, mcSurface as never, TEST_THEME, measure, 100, { previous: null, now: 40500, speed: 24 } as never);
  const mcClippedUrls = [...tableLane.visibleArt({ model: mcClippedModel, layout: mcClippedLayout, binding: mcClippedBinding, surface: mcSurface, theme: TEST_THEME, hour: 100, now: 40500, changedAt: 40500, continuity: 730, speed: 24, before: null, art: mcCold } as never)];
  assert.deepEqual(mcClippedUrls, [], 'fully clipped ×2 count badge at scrollTop=31 does not probe hidden predecessor medal');
  mcCold.dispose();
  mcWarm.dispose();

  const offSnapKind = 'test-offscreen-speed-snap';
  const offSnapCols: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'item' },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true },
  ];
  registerFamilyProjection({
    family: 'table',
    kind: offSnapKind,
    nextChange: (rv: { readonly hour: number }) => (rv.hour < 101 ? 101 : Infinity),
    project: (rv: { readonly hour: number }): TableModel => {
      const swap = rv.hour >= 101;
      return {
        columns: offSnapCols,
        hour: rv.hour,
        rows: Array.from({ length: 24 }, (_, i) => {
          const score = i === 20 ? (swap ? 3 : 4) : i === 21 ? (swap ? 4 : 3) : 24 - i;
          return { key: `r${i}`, name: `Row ${i}`, cells: { name: { value: knownValue(`Row ${i}`) }, score: { value: knownValue(score) } } };
        }),
      };
    },
  });
  const savedOffscreen2 = globalThis.OffscreenCanvas;
  globalThis.OffscreenCanvas = class {
    width: number;
    height: number;
    constructor(w: number, h: number) { this.width = w; this.height = h; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {} }; }
  } as unknown as typeof OffscreenCanvas;
  try {
    const view = createLaneView(tableLane, () => {});
    const laneState = new Map<string, unknown>();
    const transport = new LaneTransport();
    const offSnapSettings = tableSettingsOf(undefined, offSnapCols, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
    let seq = 1;
    const step = (hour: number, speed: number, scrollTop: number, now: number) => {
      const model = transport.carry({
        family: 'table',
        theme: TEST_THEME,
        kind: offSnapKind,
        options: NO_OPTIONS,
        settings: offSnapSettings,
        sources: NO_SOURCES,
        width: 320,
        height: 108,
        scrollLeft: 0,
        scrollTop,
      });
      const input = {
        widget: 77,
        subs: [],
        pictures: [],
        model,
        hour,
        seek: 0,
        speed,
        binding: { generation: 1, surfaces: [{ width: 320, height: 108, dpr: 1 }], offset: 0, pin: null },
        inspect: null,
        now,
      };
      return { paint: view.paint(input, laneState, () => `s${seq++}`), input };
    };
    step(100, 24, 0, 50000);
    const swapped = step(101, 24, 0, 50005);
    assert.equal(swapped.paint.motion, false, 'offscreen rank swap at 24 h/s does not request animation frames');
    step(101, 720, 0, 50010);
    const scrolled = step(101, 24, 20 * 36, 50015);
    assert.equal(scrolled.paint.hit?.([40, 18]), 'row:r21', 'speed-only switch to 720 h/s discards offscreen glide so scrolling after returning to 24 h/s hits settled r21');
    view.unmount?.(77, laneState);

    const { Lane } = await import('../../draw/lane.ts');
    const { lazyLaneView } = await import('../laneRuntime.ts');
    const flushTick = () => new Promise<void>(resolve => setImmediate(resolve));
    const runWorkerScenario = async (opts: { startSpeed?: number; snapAt720: boolean; busyCrop: boolean; resizeWidth: number }) => {
      const startSpeed = opts.startSpeed ?? 24;
      let clockNow = 60000;
      let lastHit: string | null = null;
      let presentedId = 0;
      let frameSerial = 1;
      let holdCrop = false;
      let releaseCrop: (() => void) | null = null;
      const lazyView = lazyLaneView('table', async () => tableLane, () => {});
      await lazyView.prepare!();
      const blockerView = {
        paint: () => ({
          viewport: { width: 10, height: 10, dpr: 1, scrollLeft: 0, scrollTop: 0 },
          surfaces: [{ source: { width: 10, height: 10 } as unknown as OffscreenCanvas, content: `b${frameSerial}` }],
          fields: {},
          motion: false,
          hit: () => null,
        }),
      };
      const workerLane = new Lane<{ readonly width: number; close(): void }>({
        lane: 0,
        lanes: 1,
        views: name => (name === 'blocker' ? blockerView : lazyView),
        crop: async sources => {
          if (holdCrop) await new Promise<void>(resolve => { releaseCrop = resolve; });
          return sources.map(s => ({ width: s.width, close() {} }));
        },
        present: presentation => {
          for (const entry of presentation.entries) if (entry.widget === 88) presentedId = entry.id;
        },
        release: () => {},
        fail: () => {},
        later: () => {},
        clock: () => clockNow,
        hit: (_wid, key) => { lastHit = key; },
      });
      const sender = new PayloadSender(msg => workerLane.payloads(structuredClone(msg)));
      const wired = (m: unknown) => structuredClone(sender.acquire(m).wire);
      const makeLaneModel = (scrollTop: number, width = 320) => ({
        family: 'table',
        theme: TEST_THEME,
        kind: offSnapKind,
        options: NO_OPTIONS,
        settings: offSnapSettings,
        sources: NO_SOURCES,
        width,
        height: 108,
        scrollLeft: 0,
        scrollTop,
      });
      const askFrame = (target: number, speed: number, ackId?: number, ackBinding = 1) => {
        workerLane.frame({
          type: 'frame',
          serial: frameSerial++,
          now: target,
          target,
          seek: 0,
          speed,
          exact: true,
          inspect: [],
          installed: ackId ? [{ widget: 88, binding: ackBinding, id: ackId }] : [],
          refused: [],
        });
      };
      workerLane.mount(88, [], 'table', wired(makeLaneModel(0)));
      workerLane.bind(88, { generation: 1, surfaces: [{ width: 320, height: 108, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } });
      askFrame(100, startSpeed);
      await flushTick(); await flushTick();
      clockNow = 60005;
      askFrame(101, startSpeed, presentedId);
      await flushTick(); await flushTick();
      if (opts.busyCrop) {
        holdCrop = true;
        workerLane.mount(89, [], 'blocker', {});
        workerLane.bind(89, { generation: 1, surfaces: [{ width: 10, height: 10, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } });
        askFrame(101, 24, presentedId);
        await flushTick();
        clockNow = 60010;
        if (opts.snapAt720) askFrame(101, 720);
        clockNow = 60012;
        askFrame(101, 24);
        holdCrop = false;
        (releaseCrop as (() => void) | null)?.();
        await flushTick(); await flushTick();
        workerLane.unmount(89);
      } else if (opts.snapAt720) {
        clockNow = 60010;
        askFrame(101, 720, presentedId);
        await flushTick(); await flushTick();
      }
      clockNow = 60015;
      workerLane.bind(88, { generation: 2, surfaces: [{ width: opts.resizeWidth, height: 108, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } }, wired(makeLaneModel(20 * 36, opts.resizeWidth)));
      askFrame(101, 24);
      await flushTick(); await flushTick();
      askFrame(101, 24, presentedId, 2);
      await flushTick(); await flushTick();
      workerLane.pointer(88, 0, [40, 18]);
      workerLane.unmount(88);
      return lastHit;
    };
    assert.equal(await runWorkerScenario({ snapAt720: false, busyCrop: false, resizeWidth: 320 }), 'row:r20', 'control: without 720 h/s snap, offscreen swap at 24 h/s glides when scrolled into view');
    assert.equal(await runWorkerScenario({ snapAt720: true, busyCrop: false, resizeWidth: 319 }), 'row:r21', 'speed-only 720 h/s cancels offscreen glide and stays cancelled across a 320->319 px resize');
    assert.equal(await runWorkerScenario({ snapAt720: true, busyCrop: true, resizeWidth: 320 }), 'row:r21', 'coalesced 720->24 h/s while another widget crop is open still cancels the offscreen glide');
    assert.equal(await runWorkerScenario({ startSpeed: 720, snapAt720: false, busyCrop: false, resizeWidth: 319 }), 'row:r21', 'row swap first painted at 720 h/s does not start gliding after slowing to 24 h/s and resizing 320->319 px');

    {
      let clockNow = 70000;
      let lastHit: string | null = null;
      let presentedId = 0;
      let frameSerial = 1;
      let holdCrop = false;
      let releaseCrop: (() => void) | null = null;
      const lazyView = lazyLaneView('table', async () => tableLane, () => {});
      await lazyView.prepare!();
      const workerLane = new Lane<{ readonly width: number; close(): void }>({
        lane: 0,
        lanes: 1,
        views: () => lazyView,
        crop: async sources => {
          if (holdCrop) await new Promise<void>(resolve => { releaseCrop = resolve; });
          return sources.map(s => ({ width: s.width, close() {} }));
        },
        present: presentation => {
          for (const entry of presentation.entries) if (entry.widget === 88) presentedId = entry.id;
        },
        release: () => {},
        fail: () => {},
        later: () => {},
        clock: () => clockNow,
        hit: (_wid, key) => { lastHit = key; },
      });
      const sender = new PayloadSender(msg => workerLane.payloads(structuredClone(msg)));
      const wired = (m: unknown) => structuredClone(sender.acquire(m).wire);
      const visibleModel = wired({
        family: 'table',
        theme: TEST_THEME,
        kind: offSnapKind,
        options: NO_OPTIONS,
        settings: offSnapSettings,
        sources: NO_SOURCES,
        width: 320,
        height: 108,
        scrollLeft: 0,
        scrollTop: 20 * 36,
      });
      const askFrame = (target: number, speed: number, ackId?: number) => {
        workerLane.frame({
          type: 'frame',
          serial: frameSerial++,
          now: target,
          target,
          seek: 0,
          speed,
          exact: true,
          inspect: [],
          installed: ackId ? [{ widget: 88, binding: 1, id: ackId }] : [],
          refused: [],
        });
      };
      workerLane.mount(88, [], 'table', visibleModel);
      workerLane.bind(88, { generation: 1, surfaces: [{ width: 320, height: 108, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } });
      askFrame(100, 24);
      await flushTick(); await flushTick();
      clockNow = 70005;
      askFrame(101, 24, presentedId);
      await flushTick(); await flushTick();
      const glidingId = presentedId;
      holdCrop = true;
      clockNow = 70010;
      askFrame(101, 720, glidingId);
      await flushTick();
      assert.ok(releaseCrop !== null, '720 h/s replacement frame is held in crop');
      workerLane.pointer(88, 0, [40, 18]);
      assert.equal(lastHit, 'row:r20', 'installed gliding bitmap keeps hitting r20 after 720 h/s request until replacement bitmap is installed');
      holdCrop = false;
      (releaseCrop as (() => void) | null)?.();
      await flushTick(); await flushTick();
      assert.notEqual(presentedId, glidingId, 'replacement bitmap is presented once crop finishes');
      askFrame(101, 720, presentedId);
      await flushTick(); await flushTick();
      workerLane.pointer(88, 0, [40, 18]);
      assert.equal(lastHit, 'row:r21', 'once 720 h/s replacement bitmap is installed, pointer hits settled r21');
      workerLane.unmount(88);
    }

    {
      let clockNow = 80000;
      let lastHit: string | null = null;
      let presentedId = 0;
      let frameSerial = 1;
      const lazyView = lazyLaneView('table', async () => tableLane, () => {});
      await lazyView.prepare!();
      const workerLane = new Lane<{ readonly width: number; close(): void }>({
        lane: 0,
        lanes: 1,
        views: () => lazyView,
        crop: async sources => sources.map(s => ({ width: s.width, close() {} })),
        present: presentation => {
          for (const entry of presentation.entries) if (entry.widget === 88) presentedId = entry.id;
        },
        release: () => {},
        fail: () => {},
        later: () => {},
        clock: () => clockNow,
        hit: (_wid, key) => { lastHit = key; },
      });
      const sender = new PayloadSender(msg => workerLane.payloads(structuredClone(msg)));
      const wired = (m: unknown) => structuredClone(sender.acquire(m).wire);
      const makeVisibleModel = (width: number) => wired({
        family: 'table',
        theme: TEST_THEME,
        kind: offSnapKind,
        options: NO_OPTIONS,
        settings: offSnapSettings,
        sources: NO_SOURCES,
        width,
        height: 108,
        scrollLeft: 0,
        scrollTop: 20 * 36,
      });
      const askFrame = (target: number, speed: number, ackId?: number, ackBinding = 1) => {
        workerLane.frame({
          type: 'frame',
          serial: frameSerial++,
          now: target,
          target,
          seek: 0,
          speed,
          exact: true,
          inspect: [],
          installed: ackId ? [{ widget: 88, binding: ackBinding, id: ackId }] : [],
          refused: [],
        });
      };
      workerLane.mount(88, [], 'table', makeVisibleModel(320));
      workerLane.bind(88, { generation: 1, surfaces: [{ width: 320, height: 108, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } });
      askFrame(100, 168);
      await flushTick(); await flushTick();
      clockNow = 80005;
      askFrame(101, 168, presentedId);
      await flushTick(); await flushTick();
      clockNow = 80135;
      askFrame(101, 24, presentedId);
      await flushTick(); await flushTick();
      askFrame(101, 24, presentedId);
      await flushTick(); await flushTick();
      workerLane.pointer(88, 0, [40, 33]);
      assert.equal(lastHit, 'row:r21', '168 h/s swap is settled at +130 ms after slowing to 24 h/s');
      workerLane.bind(88, { generation: 2, surfaces: [{ width: 319, height: 108, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } }, makeVisibleModel(319));
      askFrame(101, 24);
      await flushTick(); await flushTick();
      askFrame(101, 24, presentedId, 2);
      await flushTick(); await flushTick();
      workerLane.pointer(88, 0, [40, 33]);
      assert.equal(lastHit, 'row:r21', 'resizing 320->319 px after a 168 h/s glide completed does not restart it with the 24 h/s duration');
      workerLane.unmount(88);
    }

    {
      const markerKind = 'table:test:speed-change-rank-markers';
      const markerTexts: string[] = [];
      let captureTexts = false;
      class MarkerOffscreenCanvas {
        width: number;
        height: number;
        constructor(width: number, height: number) { this.width = width; this.height = height; }
        getContext() {
          return {
            font: '',
            fillStyle: '',
            strokeStyle: '',
            lineWidth: 1,
            globalAlpha: 1,
            textAlign: 'left',
            textBaseline: 'alphabetic',
            measureText: (t: string) => ({ width: t.length * 7 }),
            save() {},
            restore() {},
            setTransform() {},
            getTransform: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }),
            clearRect() {},
            beginPath() {},
            rect() {},
            clip() {},
            fillRect() {},
            strokeRect() {},
            moveTo() {},
            lineTo() {},
            stroke() {},
            fill() {},
            roundRect() {},
            fillText(t: string) { if (captureTexts) markerTexts.push(t); },
            drawImage() {},
          };
        }
      }
      registerFamilyProjection({
        kind: markerKind,
        family: 'table',
        nextChange: readView => (readView.hour < 101 ? 101 : Infinity),
        project: (readView): TableModel => ({
          columns: offSnapCols,
          rank: true,
          rankMarkers: true,
          rows: readView.hour === 100
            ? [
                { key: 'a', name: 'Alpha', cells: { name: { value: knownValue('Alpha') }, score: { value: knownValue(200) } } },
                { key: 'b', name: 'Beta', cells: { name: { value: knownValue('Beta') }, score: { value: knownValue(100) } } },
              ]
            : [
                { key: 'b', name: 'Beta', cells: { name: { value: knownValue('Beta') }, score: { value: knownValue(300) } } },
                { key: 'a', name: 'Alpha', cells: { name: { value: knownValue('Alpha') }, score: { value: knownValue(200) } } },
              ],
          hour: readView.hour,
        }),
      });
      const prevCanvas = globalThis.OffscreenCanvas;
      globalThis.OffscreenCanvas = MarkerOffscreenCanvas as unknown as typeof OffscreenCanvas;
      try {
        const markerSettings = { ...offSnapSettings, showRank: true };
        const view = createLaneView(tableLane, () => {});
        const state = new Map<string, unknown>();
        const markerTransport = new LaneTransport();
        let seq = 1;
        const paintAt = (hour: number, speed: number, now: number) => view.paint({
          widget: 91,
          subs: [],
          model: markerTransport.carry({
            family: 'table',
            theme: TEST_THEME,
            kind: markerKind,
            options: NO_OPTIONS,
            settings: markerSettings,
            sources: NO_SOURCES,
            width: 320,
            height: 120,
            scrollLeft: 0,
            scrollTop: 0,
          }),
          pictures: [],
          hour,
          seek: 0,
          speed,
          binding: { generation: 1, surfaces: [{ width: 320, height: 120, dpr: 1 }], offset: 0, pin: null },
          inspect: null,
          now,
        }, state, () => `m${seq++}`);
        paintAt(100, 24, 90000);
        captureTexts = true;
        const swapped = paintAt(101, 720, 90005);
        captureTexts = false;
        assert.equal(swapped.motion, false, 'swap on 24->720 h/s snaps row positions without glide animation frames');
        assert.ok(markerTexts.some(t => t.includes('▲1')), `24->720 h/s swap draws ▲1 rank arrow (got: ${markerTexts.join(', ')})`);
        assert.ok(markerTexts.some(t => t.includes('▼1')), `24->720 h/s swap draws ▼1 rank arrow (got: ${markerTexts.join(', ')})`);
        view.unmount?.(91, state);
      } finally {
        globalThis.OffscreenCanvas = prevCanvas;
      }
    }
  } finally {
    globalThis.OffscreenCanvas = savedOffscreen2;
  }
});

test('fast video export glideDurationMs override keeps gliding hit and repaint, and detail-row heading flag settles invalidate keepsCanvas', async () => {
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Name', as: 'text', offered: ['text'], priority: 0, pinned: true, text: true },
    { key: 'score', label: 'Score', as: 'plain', offered: ['plain'], priority: 1, numeric: true, format: { decimals: 0 } },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'normal', sort: { column: 'score', direction: 'descending' } });
  const exportKind = 'test-fast-export-glide-override';
  registerFamilyProjection({
    kind: exportKind,
    family: 'table',
    nextChange: readView => (readView.hour < 101 ? 101 : Infinity),
    project: (readView): TableModel => ({
      columns,
      rows: readView.hour === 100
        ? [
            { key: 'a', name: 'Alpha', cells: { name: { value: knownValue('Alpha') }, score: { value: knownValue(200) } } },
            { key: 'b', name: 'Beta', cells: { name: { value: knownValue('Beta') }, score: { value: knownValue(100) } } },
          ]
        : [
            { key: 'b', name: 'Beta', cells: { name: { value: knownValue('Beta') }, score: { value: knownValue(300) } } },
            { key: 'a', name: 'Alpha', cells: { name: { value: knownValue('Alpha') }, score: { value: knownValue(200) } } },
          ],
      hour: readView.hour,
    }),
  });
  class SimpleOffscreenCanvas {
    width: number;
    height: number;
    constructor(width: number, height: number) { this.width = width; this.height = height; }
    getContext() { return { ...instrumentedContext().ctx, setTransform() {}, clearRect() {}, canvas: this }; }
  }
  const savedOffscreen = globalThis.OffscreenCanvas;
  globalThis.OffscreenCanvas = SimpleOffscreenCanvas as unknown as typeof OffscreenCanvas;
  try {
    const view = createLaneView(tableLane, () => {});
    const state = new Map<string, unknown>();
    const transport = new LaneTransport();
    let seq = 1;
    const paintExport = (hour: number, nowMs: number) => view.paint({
      widget: 92,
      subs: [],
      model: transport.carry({
        family: 'table',
        theme: TEST_THEME,
        kind: exportKind,
        options: NO_OPTIONS,
        settings,
        sources: NO_SOURCES,
        width: 320,
        height: 120,
        scrollLeft: 0,
        scrollTop: 0,
        motion: { nowMs, speed: 240, glideDurationMs: 50 },
      }),
      pictures: [],
      hour,
      seek: 0,
      speed: 240,
      binding: { generation: 1, surfaces: [{ width: 320, height: 120, dpr: 1 }], offset: 0, pin: null },
      inspect: null,
      now: nowMs,
    }, state, () => `ex${seq++}`);
    paintExport(100, 1000);
    const swapped = paintExport(101, 1002);
    assert.equal(swapped.motion, true, '240 h/s export with 50 ms glideDurationMs keeps gliding');
    assert.equal(swapped.hit?.([40, 20]), 'row:a', '2 ms into a 50 ms export glide still hits the outgoing row');
    view.onSpeed?.(240, state);
    const mid = paintExport(101, 1010);
    assert.equal(mid.motion, true, 'onSpeed and keepsCanvas honor the export glideDurationMs override');
    view.unmount?.(92, state);
  } finally {
    globalThis.OffscreenCanvas = savedOffscreen;
  }

  const detailCols: TableColumnSpec[] = [
    { key: 'sides', label: 'Sides', as: 'text', offered: ['text'], priority: 0, pinned: true, text: true, minWidth: 260, maxWidth: 260 },
    { key: 'terms', label: 'Terms', as: 'text', offered: ['text'], priority: 1, text: true, minWidth: 260, maxWidth: 260 },
  ];
  const detailSettings = tableSettingsOf(undefined, detailCols, { density: 'normal', sort: { column: 'sides', direction: 'ascending' } });
  const headingUrl = 'art://peace-heading-flag';
  const detailModel: TableModel = {
    columns: detailCols,
    rows: [{
      key: 'treaty-1',
      name: 'Treaty',
      detailColumn: 'terms',
      cells: {
        sides: { value: knownValue('Allies vs Axis') },
        terms: {
          value: knownValue('Annexed states'),
          heading: { flag: { tag: 'GER', art: readyArt(headingUrl) }, text: knownValue('Treaty') },
        },
      },
    }],
    hour: 100,
  };
  let resolveFlag!: (bmp: { width: number; height: number; close(): void }) => void;
  const artCache = new LaneArtCache<{ width: number; height: number; close(): void }>({
    fetchBitmap: () => new Promise(resolve => { resolveFlag = resolve; }),
  });
  const ctx = { ...instrumentedContext().ctx, canvas: { width: 240, height: 160 } } as unknown as OffscreenCanvasRenderingContext2D;
  const surface = { width: 240, height: 160, dpr: 1 };
  const detailBinding = {
    family: 'table' as const,
    kind: 'test-detail-heading',
    options: NO_OPTIONS,
    settings: detailSettings,
    sources: NO_SOURCES,
    width: 240,
    height: 160,
    scrollLeft: 0,
    scrollTop: 0,
    theme: TEST_THEME,
  };
  const detailLayout = tableLane.layout(detailModel, detailBinding as never, surface as never, TEST_THEME, ctx, 100, { previous: null, now: 100, speed: 0 } as never);
  assert.ok(!detailLayout.visibleColumns?.some(c => c.key === 'terms'), 'terms column is scrolled offscreen at 240px width');
  const detailFrame = {
    binding: detailBinding,
    theme: TEST_THEME,
    model: detailModel,
    layout: detailLayout,
    surface,
    hour: 100,
    inspect: null,
    now: 100,
    speed: 0,
    art: artCache,
    continuity: 1,
    changedAt: 100,
    before: null,
  };
  const wanted = [...tableLane.visibleArt(detailFrame as never)];
  assert.ok(wanted.includes(headingUrl), 'visibleArt requests the detail-row heading flag');
  artCache.requestVisible(wanted, []);
  tableLane.paint(ctx, detailFrame as never);
  assert.equal(tableLane.keepsCanvas!(ctx, detailFrame as never), true, 'unchanged before heading flag decodes');
  resolveFlag({ width: 20, height: 14, close() {} });
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(tableLane.keepsCanvas!(ctx, detailFrame as never), false, 'settling offscreen detailColumn heading flag invalidates keepsCanvas');
  artCache.dispose();
});

test('empty table gaining a recording gap as its first visible row during playback publishes the gap mirror immediately', () => {
  const cols: TableColumnSpec[] = [
    { key: 'name', label: 'Event', as: 'plain', offered: ['plain'], priority: 0, text: true },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'normal', sort: { column: 'name', direction: 'ascending' } });
  registerFamilyProjection({
    family: 'table',
    kind: 'test-empty-to-gap',
    project(readView, options: { readonly gaps: number }): TableModel {
      const slots: TableModelSlot[] = Array.from({ length: options.gaps }, (_, i) => ({
        kind: 'gap' as const,
        key: `gap:${i}`,
        firstHour: 10 + i * 10,
        lastHour: 20 + i * 10,
        reason: `Collector detached ${i}`,
      }));
      return { hour: readView.hour, columns: cols, rows: [], slots, unsorted: true, totalRows: 10 };
    },
  });
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 400, height: 200, dpr: 1 });
    const makeBinding = (gaps: number) => ({
      family: 'table' as const,
      kind: 'test-empty-to-gap',
      options: { gaps },
      settings,
      sources: NO_SOURCES,
      width: 400,
      height: 200,
      scrollLeft: 0,
      scrollTop: 0,
      theme: TEST_THEME,
      expanded: {},
    });
    const empty = lane.paint({ label: 'empty', binding: makeBinding(0), hour: 10, speed: 24, seek: 1 });
    assert.deepEqual((empty.fields.mirror?.value as { rows: unknown[] } | undefined)?.rows, [], 'initial empty log emits an empty mirror');
    const firstGap = lane.paint({ label: 'first-gap', binding: makeBinding(1), hour: 20, speed: 24, seek: 1 });
    const gapRows = (firstGap.fields.mirror?.value as { rows: { name: string }[] } | undefined)?.rows;
    assert.equal(gapRows?.length, 1, 'empty table gaining a gap row during playback publishes its mirror immediately');
    assert.match(gapRows![0]!.name, /Collector detached 0/);
    const secondGap = lane.paint({ label: 'second-gap', binding: makeBinding(2), hour: 30, speed: 24, seek: 1 });
    assert.equal(secondGap.fields.mirror, undefined, 'subsequent gap additions defer once the mirror has visible rows');
  } finally {
    recording.restore();
  }
});

test('deriveTableMirror and deriveTableTooltip share one date presenter for date cells', () => {
  const datedHour = engineHour({ year: 1940, month: 12, day: 27, hour: 1 })!;
  const midnightHour = engineHour({ year: 1936, month: 1, day: 1, hour: 1 })!;
  const rangeEndHour = engineHour({ year: 1936, month: 1, day: 5, hour: 12 })!;
  const cols: TableColumnSpec[] = [
    { key: 'date', label: 'Date', as: 'plain', offered: ['plain'], priority: 0, text: true, cellKind: 'date' },
    { key: 'event', label: 'Event', as: 'plain', offered: ['plain'], priority: 1, text: true, cellKind: 'item' },
    { key: 'last', label: 'Last', as: 'plain', offered: ['plain'], priority: 2, numeric: true, cellKind: 'date' },
    { key: 'span', label: 'Span', as: 'plain', offered: ['plain'], priority: 3, cellKind: 'date', headerGroup: { kind: 'pair', labels: ['Start', 'End'] } },
  ];
  const rows: TableRow[] = [
    {
      key: 'dated',
      name: 'Market purchase',
      cells: {
        date: { value: knownValue(datedHour) },
        event: { value: knownValue('Market purchase') },
        last: { value: knownValue(BigInt(midnightHour)) },
        span: { value: knownValue(midnightHour), pair: [knownValue(midnightHour), knownValue(rangeEndHour)] },
      },
    },
    {
      key: 'partial-and-range',
      name: 'Estimated sighting',
      cells: {
        date: { value: partialValue(datedHour, 'Estimated from nearby hour') },
        event: { value: knownValue('Estimated sighting') },
        last: { value: knownValue('1 Jan 1936 \u2013 5 Jan 1936') },
        span: { value: notApplicable('Single point') },
      },
    },
    {
      key: 'zero-and-missing',
      name: 'Zero and missing',
      cells: {
        date: { value: zeroValue(0) },
        event: { value: knownValue('Zero and missing') },
        last: { value: notRecorded('Last delivery date not recorded') },
        span: { value: notRecorded('Span not recorded') },
      },
    },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'normal', sort: { column: 'date', direction: 'descending' } });
  const flat = resolveTableRows({ hour: datedHour, columns: cols, rows, unsorted: true }, settings);
  const mirror = deriveTableMirror(flat, cols, false);

  for (const row of rows) {
    const mirrorRow = mirror.rows.find(entry => entry.key === row.key)!;
    const tip = deriveTableTooltip(flat, row.key, cols)!;
    for (const colKey of ['date', 'last'] as const) {
      const col = cols.find(c => c.key === colKey)!;
      const mirrorSpoken = mirrorRow.cells.find(cell => cell.column === colKey)!.spoken;
      const tipRow = tip.rows.find(entry => entry.label === col.label)!;
      assert.equal(mirrorSpoken, valueStateText(tipRow.valueState!), `${row.key}.${colKey}: mirror text matches tooltip date text`);
      assert.doesNotMatch(mirrorSpoken, /\b\d{2},\d{3},\d{3}\b/, `${row.key}.${colKey}: never spoken as an eight-digit hour count`);
    }
  }

  const datedMirror = mirror.rows.find(entry => entry.key === 'dated')!;
  assert.equal(datedMirror.cells.find(cell => cell.column === 'date')!.spoken, formatDateHover(datedHour));
  assert.equal(datedMirror.cells.find(cell => cell.column === 'last')!.spoken, formatDateHover(midnightHour));
  const spanMirror = datedMirror.cells.find(cell => cell.column === 'span')!.spoken;
  const datedTip = deriveTableTooltip(flat, 'dated', cols)!;
  const startTip = valueStateText(datedTip.rows.find(entry => entry.label === 'Span (Start)')!.valueState!);
  const endTip = valueStateText(datedTip.rows.find(entry => entry.label === 'Span (End)')!.valueState!);
  assert.equal(spanMirror, `Start ${startTip}, End ${endTip}`);
  assert.equal(startTip, formatDateHover(midnightHour));
  assert.equal(endTip, formatDateHover(rangeEndHour));

  const partialMirror = mirror.rows.find(entry => entry.key === 'partial-and-range')!;
  const partialTip = deriveTableTooltip(flat, 'partial-and-range', cols)!;
  assert.equal(partialMirror.cells.find(cell => cell.column === 'date')!.spoken, `${formatDateHover(datedHour)}, partial: Estimated from nearby hour`);
  assert.equal(partialMirror.cells.find(cell => cell.column === 'last')!.spoken, '1 Jan 1936 \u2013 5 Jan 1936');
  const partialSpanMirror = partialMirror.cells.find(cell => cell.column === 'span')!.spoken;
  const partialSpanTip = valueStateText(partialTip.rows.find(entry => entry.label === 'Span')!.valueState!);
  assert.equal(partialSpanMirror, partialSpanTip);
  assert.equal(partialSpanMirror, 'Not applicable: Single point');

  const zeroMirror = mirror.rows.find(entry => entry.key === 'zero-and-missing')!;
  assert.equal(zeroMirror.cells.find(cell => cell.column === 'date')!.spoken, formatDateHover(0));
  assert.equal(zeroMirror.cells.find(cell => cell.column === 'last')!.spoken, 'Not recorded: Last delivery date not recorded');
});

test('tableHostAdapter.deriveAuditRows matches the painted lane columns without re-laying out final widths', () => {
  const cols: TableColumnSpec[] = [
    { key: 'date', label: 'Date', as: 'plain', offered: ['plain'], priority: 0, cellKind: 'date', minWidth: 70, maxWidth: 70 },
    { key: 'kind', label: '', as: 'plain', offered: ['plain'], priority: 1, cellKind: 'kind', minWidth: 30, maxWidth: 30 },
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName', 'flag'], priority: 2, minWidth: 68, narrow: 'flag', narrowWidth: 40 },
    { key: 'entry', label: 'Entry', as: 'plain', offered: ['plain'], priority: 3, cellKind: 'item', text: true, minWidth: 60, narrow: 'primary' },
    { key: 'place', label: 'Place', as: 'plain', offered: ['plain'], priority: 4, text: true, minWidth: 154, maxWidth: 154, narrow: 'hide' },
    { key: 'status', label: 'Status', as: 'plain', offered: ['plain'], priority: 5, cellKind: 'status', minWidth: 124, maxWidth: 124, narrowWidth: 104 },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'normal', sort: { column: 'date', direction: 'descending' } });
  const model: TableModel = {
    columns: cols,
    hour: 100,
    unsorted: true,
    rows: [{
      key: 'row-1',
      name: 'Alpha entry',
      cells: {
        date: { value: knownValue(100) },
        kind: { value: knownValue('award') },
        country: { value: knownValue('Alpha'), country: 'ALP' },
        entry: { value: knownValue('Alpha entry') },
        place: { value: knownValue('Northern Harbor') },
        status: { value: knownValue('Active') },
      },
    }],
  };
  const ctx = { ...instrumentedContext().ctx, canvas: { width: 822, height: 200 } } as unknown as OffscreenCanvasRenderingContext2D;
  const checkAtWidth = (width: number, expectedKeys: readonly string[], expectedCountryDisplay: string, note: string) => {
    const surface = { width, height: 200, dpr: 1 };
    const binding = {
      family: 'table' as const,
      kind: 'test-audit-columns',
      options: NO_OPTIONS,
      settings,
      sources: NO_SOURCES,
      width,
      height: 200,
      scrollLeft: 0,
      scrollTop: 0,
      theme: TEST_THEME,
      expanded: {},
    };
    const layout = tableLane.layout(model, binding as never, surface as never, TEST_THEME, ctx, 100, { previous: null, now: 100, speed: 0 } as never);
    const frame = {
      binding,
      theme: TEST_THEME,
      model,
      layout,
      surface,
      hour: 100,
      inspect: null,
      now: 100,
      speed: 0,
      art: new LaneArtCache(),
      continuity: 1,
      changedAt: 100,
      before: null,
    };
    const paintedAudit = tableLane.audit!(frame as never) as readonly { key: string; value?: string }[];
    const structureFields = tableLane.structure!(frame as never);
    const verifiedAudit = tableHostAdapter.deriveAuditRows!(model, 100, {
      viewport: { width, height: 200, dpr: 1, scrollLeft: 0, scrollTop: 0, zoomWindow: null },
      settings,
      expanded: {},
      theme: TEST_THEME,
      fields: structureFields,
    });
    const columnsOf = (rows: readonly { key: string; value?: string }[]) =>
      (JSON.parse(rows.find(row => row.key === '__table__:columns')!.value!) as { key: [string, string]; display: [string, string] }[])
        .map(c => ({ key: c.key[1], display: c.display[1] }));
    const paintedCols = columnsOf(paintedAudit);
    const verifiedCols = columnsOf(verifiedAudit);
    assert.deepEqual(paintedCols.map(c => c.key), expectedKeys, `${note}: painted columns`);
    assert.equal(paintedCols.find(c => c.key === 'country')?.display, expectedCountryDisplay, `${note}: painted country display`);
    assert.deepEqual(verifiedCols, paintedCols, `${note}: verifier columns equal painted columns`);
    assert.deepEqual(verifiedAudit, paintedAudit, `${note}: verifier audit equals painted audit`);
  };
  checkAtWidth(822, ['date', 'kind', 'country', 'entry', 'place', 'status'], 'flagName', 'wide table');
  checkAtWidth(420, ['date', 'kind', 'country', 'entry', 'status'], 'flag', 'narrow table');
});

test('resolvePanes namespaces pane-local headers for paint and exact audit while still rejecting duplicates inside one pane', async () => {
  const columns: TableColumnSpec[] = [
    { key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true, minWidth: 100 },
    { key: 'share', label: 'Share', as: 'plain', offered: ['plain'], priority: 1, numeric: true, minWidth: 80 },
  ];
  const settings = tableSettingsOf(undefined, columns, { density: 'compact', barsBehind: 'off', sort: { column: 'share', direction: 'descending' } });
  const row = (key: string, name: string, share: number, group?: { readonly key: string; readonly label: string }): TableRow => ({
    key,
    name,
    ...(group ? { group } : {}),
    cells: {
      name: { value: knownValue(name) },
      share: { value: knownValue(share), format: 'integer' },
    },
  });
  const sharedGroup = { key: 'side', label: 'Share of the side' } as const;
  const twoSideModel: TableModel = {
    hour: 100,
    columns,
    rows: [],
    panes: [
      {
        key: 'opposing',
        title: { name: knownValue('Alpha') },
        totalRow: row('total:opposing', 'Share of the war', 60),
        rows: [row('opposing:AAA', 'Alpha', 60, sharedGroup)],
      },
      {
        key: 'viewpoint',
        title: { name: knownValue('Beta') },
        totalRow: row('total:viewpoint', 'Share of the war', 40),
        rows: [row('viewpoint:BBB', 'Beta', 40, sharedGroup)],
      },
    ],
  };

  const panes = resolvePanes(twoSideModel, settings, undefined, 'standard');
  const paneGroupKeys = panes.map(pane => pane.rows.find(item => item.kind === 'group-header')?.key);
  assert.deepEqual(paneGroupKeys, ['pane:opposing:group:side', 'pane:viewpoint:group:side'], 'each pane carries its own composite key for the local group header');
  const flatRows = combinedPaneRows(panes);
  assert.deepEqual(
    flatRows.filter(item => item.kind === 'group-header').map(item => item.key),
    ['pane:opposing', 'pane:opposing:group:side', 'pane:viewpoint', 'pane:viewpoint:group:side'],
    'combinedPaneRows keeps the pane-qualified group header keys beside each pane header',
  );

  const projection = registerFamilyProjection({
    kind: 'test-panes-audit-group-collision',
    family: 'table',
    project: () => twoSideModel,
  });
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(tableLane, () => {}), recording.canvases, { width: 640, height: 240, dpr: 1 });
    const painted = lane.paint({
      label: 'two-sided-audit',
      binding: {
        family: 'table' as const,
        kind: projection.kind,
        options: NO_OPTIONS,
        settings,
        sources: NO_SOURCES,
        width: 640,
        height: 240,
        scrollLeft: 0,
        scrollTop: 0,
        theme: TEST_THEME,
        reducedMotion: true,
        audit: true,
        auditId: 1,
      },
      hour: 100,
      speed: 0,
      seek: 1,
    });
    const verifier = createPlanAuditVerifier<TableModel, Record<string, never>>();
    const exact = await verifier(100, {
      projection,
      options: NO_OPTIONS,
      exactReadView: async hour => createBoundedReadView({ hour }),
      deriveRows: (model, hour, fields) => tableHostAdapter.deriveAuditRows!(model, hour, {
        settings,
        expanded: {},
        viewport: { width: 640, height: 240, scrollLeft: 0, scrollTop: 0, dpr: 1, zoomWindow: null },
        theme: TEST_THEME,
        fields: fields ?? {},
      }),
    }, 1, Object.fromEntries(Object.entries(painted.fields).map(([k, v]) => [k, v.value])));
    assert.ok(exact, 'verifier returns an exact map for two panes with the same local group key');
    assert.ok(exact.values.has('pane:opposing:group:side:group'));
    assert.ok(exact.values.has('pane:viewpoint:group:side:group'));
    const auditField = painted.fields.audit?.value as { readonly rows: readonly { readonly row: string; readonly key: string; readonly value?: string }[] } | undefined;
    assert.ok(auditField, 'painted picture emits audit rows');
    assert.equal(pictureError('table', auditField.rows, exact), null, 'painted audit matches exact verifier output');
  } finally {
    recording.restore();
  }

  const colonPanes = resolvePanes({
    hour: 100,
    columns,
    rows: [],
    panes: [
      { key: 'a', title: { name: knownValue('A') }, rows: [row('a:1', 'One', 1, { key: 'b', label: 'B' })] },
      { key: 'a:group:b', title: { name: knownValue('AB') }, rows: [row('ab:1', 'Two', 2)] },
    ],
  }, settings, undefined, 'standard');
  const colonKeys = combinedPaneRows(colonPanes).filter(item => item.kind === 'group-header').map(item => item.key);
  assert.equal(new Set(colonKeys).size, colonKeys.length, 'pane keys containing colons never collide with pane-local row keys');

  assert.throws(() => resolvePanes({
    hour: 100,
    columns,
    rows: [],
    panes: [{
      key: 'left',
      title: { name: knownValue('Left') },
      rows: [row('left:A', 'A1', 10, sharedGroup), row('left:A', 'A2', 5, sharedGroup)],
    }],
  }, settings, undefined, 'standard'), /duplicate row key left:A/, 'duplicate data row inside one pane is rejected');

  assert.throws(() => resolvePanes({
    hour: 100,
    columns,
    rows: [],
    slots: [
      { kind: 'loading', key: 'loading:0' },
      { kind: 'loading', key: 'loading:0' },
    ],
    panes: [{
      key: 'left',
      title: { name: knownValue('Left') },
      rows: [],
    }],
  }, settings, undefined, 'standard'), /duplicate row key loading:0/, 'duplicate loading slot inside one pane is rejected');

  assert.throws(() => resolvePanes({
    hour: 100,
    columns,
    rows: [],
    slots: [
      { kind: 'data', row: row('x', 'Alpha', 10) },
      { kind: 'refused', key: 'x', reason: 'Not recorded' },
    ],
    panes: [{
      key: 'left',
      title: { name: knownValue('Left') },
      rows: [],
    }],
  }, settings, undefined, 'standard'), /duplicate row key x/, 'mixed-kind duplicate local key inside one pane is rejected');

  assert.throws(() => resolvePanes({
    hour: 100,
    columns,
    rows: [],
    panes: [
      { key: 'left', title: { name: knownValue('Left') }, rows: [row('shared', 'A', 10)] },
      { key: 'right', title: { name: knownValue('Right') }, rows: [row('shared', 'B', 5)] },
    ],
  }, settings, undefined, 'standard'), /already in another pane/, 'duplicate data row across panes is still rejected');

  assert.throws(() => resolvePanes({
    hour: 100,
    columns,
    rows: [],
    panes: [
      { key: 'same', title: { name: knownValue('One') }, rows: [row('one:A', 'A', 10)] },
      { key: 'same', title: { name: knownValue('Two') }, rows: [row('two:B', 'B', 5)] },
    ],
  }, settings, undefined, 'standard'), /duplicate pane key/, 'duplicate pane key across panes is rejected');
});

test('pending identity yields a pending cell whose painted, mirror, hover and audit states say loading, while resolved shows the name and absent stays missing', () => {
  const makeView = (status: 'pending' | 'ready' | 'absent') => createBoundedReadView({
    hour: 10,
    identity: {
      resolve(kind, key) {
        return {
          kind,
          key,
          holder: { status: 'ready', value: key },
          name: status === 'pending'
            ? { status: 'pending' }
            : status === 'ready'
              ? { status: 'ready', value: kind === 'country' ? 'Valdoria' : 'Northbridge' }
              : { status: 'absent', reason: 'Place not in recording' },
          colour: { status: 'absent', reason: 'none' },
          flagChoice: { status: 'absent', reason: 'none' },
          validInterval: [0, Infinity] as const,
          nextChangeAt: Infinity,
        };
      },
    },
  });
  const cols: TableColumnSpec[] = [
    { key: 'country', label: 'Country', as: 'flagName', offered: ['flagName'], priority: 0, text: true, pinned: true, minWidth: 140 },
    { key: 'item', label: 'Battle', as: 'plain', offered: ['plain'], cellKind: 'item', priority: 1, text: true, minWidth: 140 },
    { key: 'place', label: 'Place', as: 'plain', offered: ['plain'], priority: 2, text: true, minWidth: 140 },
  ];
  const settings = tableSettingsOf(undefined, cols, { density: 'normal', sort: { column: 'country', direction: 'ascending' } });
  const inspect = (status: 'pending' | 'ready' | 'absent') => {
    const view = makeView(status);
    const country = identityCell(view, 'country', 'AAA');
    const resolved = resolveName(view, 'province', '101');
    const row: TableRow = {
      key: 'r1',
      name: resolved.name,
      cells: { country, item: { value: resolved.value }, place: { value: resolved.value } },
    };
    const model: TableModel = { hour: 10, columns: cols, rows: [row], unsorted: true };
    const flat = resolveTableRows(model, settings);
    const layout = layoutFamilyTable(buildColumnLayouts(cols, settings, { country: 160, item: 160, place: 160 }), 520);
    const drawn: string[] = [];
    const ctx = {
      font: '', fillStyle: '', strokeStyle: '', globalAlpha: 1, lineWidth: 1, textAlign: 'left', textBaseline: 'middle',
      save() {}, restore() {}, beginPath() {}, rect() {}, roundRect() {}, clip() {}, moveTo() {}, lineTo() {}, stroke() {}, fill() {},
      fillRect() {}, clearRect() {}, drawImage() {},
      measureText(text: string) { return { width: text.length * 7 }; },
      fillText(text: string) { drawn.push(text); },
    } as unknown as CanvasRenderingContext2D;
    drawFamilyTable(ctx, {
      layout, rows: flat, settings, hour: 10, width: 520, height: 120, scrollLeft: 0, scrollTop: 0,
      rankShown: false, rankWidth: 0, theme: TEST_THEME, now: 0, glideDuration: 0, glide: NO_GLIDE,
      inspectedKey: null, selectedKey: null, focusedKey: null,
    });
    const mirror = deriveTableMirror(flat, cols, false).rows[0]!;
    const tooltip = deriveTableTooltip(flat, 'r1', cols)!;
    const audit = deriveTableAudit({ model, flatRows: flat, visibleColumns: layout.columns });
    return { country, resolved, drawn, mirror, tooltip, audit };
  };

  const pending = inspect('pending');
  assert.equal(pending.country.value.kind, 'pending');
  assert.equal(pending.resolved.value.kind, 'pending');
  assert.equal('loading' in pending.country, false, 'pending state lives on Value, not a separate cell flag');
  assert.equal('loading' in pending.resolved, false, 'resolveName exposes no separate loading flag');
  assert.equal(pending.drawn.filter(t => t === 'Loading…').length, 3, 'country, item and place all paint Loading…');
  assert.equal(pending.drawn.some(t => /not recorded/i.test(t)), false, 'a pending cell never paints Not recorded');
  for (const col of ['country', 'item', 'place']) {
    const spoken = pending.mirror.cells.find(c => c.column === col)!.spoken;
    assert.match(spoken, /^Loading/, `${col} accessible text says loading`);
    assert.doesNotMatch(spoken, /not recorded/i, `${col} accessible text never says not recorded`);
  }
  assert.equal(pending.tooltip.rows.find(r => r.label === 'Place')?.valueState?.kind, 'pending');
  assert.match(String(pending.audit.find(a => a.key === 'r1:place')!.value), /"pending"/);
  assert.doesNotMatch(String(pending.audit.find(a => a.key === 'r1:place')!.value), /"notRecorded"/);

  const ready = inspect('ready');
  assert.deepEqual(ready.country.value, knownValue('Valdoria'));
  assert.deepEqual(ready.resolved.value, knownValue('Northbridge'));
  assert.ok(ready.drawn.includes('Valdoria'));
  assert.ok(ready.drawn.includes('Northbridge'));
  assert.equal(ready.mirror.cells.find(c => c.column === 'place')!.spoken, 'Northbridge');

  const absent = inspect('absent');
  assert.equal(absent.country.value.kind, 'notRecorded');
  assert.equal(absent.resolved.value.kind, 'notRecorded');
  assert.match(absent.mirror.cells.find(c => c.column === 'place')!.spoken, /^Not recorded: Place not in recording/);
  assert.equal(absent.tooltip.rows.find(r => r.label === 'Place')?.valueState?.kind, 'missing');

  const mixedView = createBoundedReadView({
    hour: 10,
    identity: {
      resolve(kind, key) {
        return {
          kind, key, holder: { status: 'ready', value: key },
          name: key === 'AAA' ? { status: 'ready', value: 'Valdoria' } : { status: 'pending' },
          colour: { status: 'absent', reason: 'none' }, flagChoice: { status: 'absent', reason: 'none' },
          validInterval: [0, Infinity] as const, nextChangeAt: Infinity,
        };
      },
    },
  });
  const mixedCell = logCountryCell(mixedView, [{ tag: 'AAA' }, { tag: 'BBB' }], 10);
  assert.equal(mixedCell.value.kind, 'partial');
  assert.equal(mixedCell.value.kind === 'partial' ? mixedCell.value.reason : null, 'Name is loading');

  const paneModel: TableModel = {
    hour: 10,
    columns: cols,
    rows: [],
    panes: [{ key: 'left', title: { name: { kind: 'pending', reason: 'Name is loading' } }, rows: [{ key: 'r1', name: 'Valdoria', cells: { country: { value: knownValue('Valdoria') }, item: { value: knownValue('A') }, place: { value: knownValue('B') } } }] }],
  };
  const paneFlat = combinedPaneRows(resolvePanes(paneModel, settings, undefined, 'standard'));
  const paneMirror = deriveTableMirror(paneFlat, cols, false);
  assert.equal(paneMirror.rows[0]!.name, 'Loading: Name is loading');
  assert.equal(paneMirror.rows[0]!.cells[0]!.spoken, 'Loading: Name is loading');
  const paneAudit = deriveTableAudit({ model: paneModel, flatRows: paneFlat, visibleColumns: buildColumnLayouts(cols, settings) });
  assert.match(String(paneAudit.find(a => a.key === 'pane:left:group')!.value), /"pending"/);
});
