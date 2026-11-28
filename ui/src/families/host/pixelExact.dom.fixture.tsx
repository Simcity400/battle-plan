import { useRef, useState } from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { Responsive } from 'react-grid-layout';
import type { Layout } from 'react-grid-layout';
import 'react-grid-layout/css/styles.css';
import { LABEL_WEIGHT } from '../../charts/draw.ts';
import { liveKey } from '../../charts/live.ts';
import type { LiveValues } from '../../charts/live';
import { measureColour } from '../../charts/palette.ts';
import { DEFAULT_APPEARANCE, resolveAppearance } from '../../shell/appearance/settings.ts';
import { applyAppearance } from '../../shell/appearance/useAppearance.ts';
import { engineHour } from '../../dashboard/calendar.ts';
import type { DashboardClient, DashboardSource } from '../../dashboard/client';
import type { WidgetRuntime } from '../../dashboard/definitions';
import { pipelineFor, type LaneWorker, type PipelineHost } from '../../dashboard/follow/pipeline.ts';
import { FollowedWidgetKey } from '../../dashboard/follow/store.ts';
import { useGridPlacement } from '../../dashboard/gridPlacement.ts';
import type { WidgetSpec } from '../../dashboard/model';
import { PlaybackController } from '../../dashboard/playback.ts';
import { WidgetFrameContext } from '../../dashboard/widgetFrame.tsx';
import type { FromLane, ToLane } from '../../draw/laneMessages';
import { laneDispatch } from '../../draw/laneDispatch.ts';
import { ExportSurfaceContext } from '../../export/exportMotion.ts';
import { drawnViews } from '../../draw/views/drawn.ts';
import type { Breakdown, BreakdownAmount, BreakdownPart, CountryFlowReply, DoctrineMasteryReply, FlowLink, FocusCardReply, IdeaHistoryReply, IdeaLook, PowerSpendingBreakdownReply } from '../../generated/dashboard';
import { doctrineMasteryProjection } from '../../widgets/doctrine-mastery/doctrineMasteryProjection.ts';
import { fuelBranchesProjection } from '../../widgets/fuel-branches/fuelBranchesProjection.ts';
import { ideaTimelineProjection } from '../../widgets/idea-timeline/ideaTimelineProjection.ts';
import { nationalFocusProjection } from '../../widgets/national-focus/nationalFocusProjection.ts';
import { ppSpendingProjection } from '../../widgets/political-power-spending-breakdown/ppSpendingProjection.ts';
import { tradeFlowProjection } from '../../widgets/trade-flows/tradeFlowsProjection.ts';
import { barsHostAdapter } from '../bars/hostAdapter.ts';
import { defaultArtFrame, readyArt } from '../art/types.ts';
import { layoutFamilyBars } from '../bars/layout.ts';
import { barsLane } from '../bars/laneView.ts';
import { drawFamilyBars } from '../bars/painter.ts';
import { createBoundedReadView, displayedHour } from '../boundedView.ts';
import { breakdownHostAdapter } from '../breakdown/hostAdapter.ts';
import { cardHostAdapter } from '../card/hostAdapter.ts';
import type { FamilyProjection } from '../contract.ts';
import { flowHostAdapter } from '../flow/hostAdapter.ts';
import { heatLanesHostAdapter, heatMatrixHostAdapter } from '../heat/hostAdapter.ts';
import type { HeatLanesModel } from '../heat/lanesModel.ts';
import type { HeatMatrixModel, MatrixAxisEntity, MatrixCell } from '../heat/matrixModel.ts';
import { meterHostAdapter } from '../meter/hostAdapter.ts';
import type { FamilyId } from '../plan.ts';
import { registerFamilyProjection } from '../registry.ts';
import { tableHostAdapter } from '../table/hostAdapter.ts';
import { tableLane } from '../table/laneView.ts';
import { declareTableProjection } from '../table/model.ts';
import { tableSettingsOf } from '../table/settings.ts';
import TableChrome from '../table/TableChrome.tsx';
import { timeChartHostAdapter } from '../time-chart/hostAdapter.ts';
import { timeChartLane } from '../time-chart/laneView.ts';
import { createLaneView, type LaneBinding, type LaneFamily } from '../laneRuntime.ts';
import type { TimeChartModel } from '../time-chart/model.ts';
import { timelineHostAdapter } from '../timeline/hostAdapter.ts';
import { knownValue } from '../value.ts';
import { readFamilyTheme } from '../theme.ts';
import type { FamilyHostAdapter, FamilyStaticSources } from './adapter.ts';
import FamilyHost from './FamilyHost.tsx';
import '../../styles.css';
import '../../dashboard/dashboard.css';

const BOUNDS: readonly [number, number] = [0, 2_000_000_000];
const H1936 = engineHour({ year: 1936, month: 1, day: 1, hour: 1 })!;
const H = (year: number, month = 1, day = 1) => engineHour({ year, month, day, hour: 1 })!;

const source = { id: 'pixel', revision: 'r1', path: 'pixel', metadata: { branches: [{ id: '1', firstHour: 0, lastHour: BOUNDS[1] }] } } as unknown as DashboardSource;
let ports: readonly MessagePort[] = [];
const client = {
  available: true,
  openFollow: (opened: readonly MessagePort[]) => { ports = opened; return Promise.resolve(); },
  product: async (request: { params?: { route?: number } }) => ({ data: { available: true, route: request.params?.route } }),
} as unknown as DashboardClient;
const clock = new PlaybackController();
clock.setBounds({ firstHour: BOUNDS[0], lastHour: BOUNDS[1] });
const runtime = { client, clock, tracks: [{ id: 't1', source, branch: { id: '1', firstHour: BOUNDS[0], lastHour: BOUNDS[1] }, axisOffset: 0 }] } as unknown as WidgetRuntime;

let held: Promise<void> | null = null;
let release = () => {};

interface TextDraw { readonly text: string; readonly x: number; readonly y: number }
const textDraws = new WeakMap<OffscreenCanvas, TextDraw[]>();
const bitmapText = new WeakMap<ImageBitmap, readonly TextDraw[]>();
const motionFrames = new Map<string, unknown>();
function measuredFamily<B extends LaneBinding, M, L>(family: LaneFamily<B, M, L>): LaneFamily<B, M, L> {
  return { ...family, paint(ctx, frame) {
    textDraws.set(ctx.canvas, []);
    const result = family.paint(ctx, frame);
    motionFrames.set(family.family, { hour: frame.hour, before: frame.before?.hour, now: frame.now, changedAt: frame.changedAt, speed: frame.speed, result });
    return result;
  } };
}
const fillText = OffscreenCanvasRenderingContext2D.prototype.fillText;
OffscreenCanvasRenderingContext2D.prototype.fillText = function (text, x, y, maxWidth) {
  const t = this.getTransform();
  let draws = textDraws.get(this.canvas);
  if (!draws) textDraws.set(this.canvas, draws = []);
  draws.push({ text, x: t.a * x + t.c * y + t.e, y: t.b * x + t.d * y + t.f });
  if (maxWidth === undefined) fillText.call(this, text, x, y);
  else fillText.call(this, text, x, y, maxWidth);
};
const clearRect = OffscreenCanvasRenderingContext2D.prototype.clearRect;
OffscreenCanvasRenderingContext2D.prototype.clearRect = function (x, y, w, h) {
  if (x === 0 && y === 0) textDraws.set(this.canvas, []);
  clearRect.call(this, x, y, w, h);
};
const drawImage = OffscreenCanvasRenderingContext2D.prototype.drawImage;
OffscreenCanvasRenderingContext2D.prototype.drawImage = function (source: CanvasImageSource, x: number, y: number, ...rest: number[]) {
  const draws = source instanceof OffscreenCanvas ? textDraws.get(source) : undefined;
  if (draws && rest.length === 0) {
    const t = this.getTransform();
    const shifted = draws.map(p => ({ text: p.text, x: t.a * (p.x + x) + t.c * (p.y + y) + t.e, y: t.b * (p.x + x) + t.d * (p.y + y) + t.f }));
    textDraws.set(this.canvas, [...textDraws.get(this.canvas) ?? [], ...shifted]);
  }
  Reflect.apply(drawImage, this, [source, x, y, ...rest]);
};

function createLane(): LaneWorker {
  const worker: LaneWorker = { onmessage: null, postMessage(message: ToLane) { queueMicrotask(() => dispatch(message)); } };
  const dispatch = laneDispatch(
    (message: FromLane) => { queueMicrotask(() => worker.onmessage?.({ data: message } as MessageEvent<FromLane>)); },
    {
      views: (kind, wake) => kind === 'bars' ? createLaneView(measuredFamily(barsLane), wake) : kind === 'table' ? createLaneView(measuredFamily(tableLane), wake) : kind === 'time-chart' ? createLaneView(measuredFamily(timeChartLane), wake) : drawnViews(kind, wake)!,
      crop: async sources => {
        const bitmaps = await Promise.all(sources.map(async item => {
          const bitmap = await createImageBitmap(item as OffscreenCanvas);
          bitmapText.set(bitmap, [...textDraws.get(item as OffscreenCanvas) ?? []]);
          return bitmap;
        }));
        while (held) await held;
        return bitmaps;
      },
      later: run => requestAnimationFrame(() => run()),
    },
  );
  return worker;
}
const host: PipelineHost = { lanes: [createLane(), createLane()], open: list => client.openFollow!(list), channel: () => new MessageChannel(), frameRate: () => 60, rate: () => 1 };
const pipeline = pipelineFor(clock, client, () => host);
void ports;

interface Shown { readonly width: number; readonly height: number; readonly bitmap: ImageBitmap; readonly text: readonly TextDraw[] }
const shown = new WeakMap<HTMLCanvasElement, Shown>();
let installs = 0;
const transfer = ImageBitmapRenderingContext.prototype.transferFromImageBitmap;
ImageBitmapRenderingContext.prototype.transferFromImageBitmap = function (this: ImageBitmapRenderingContext, bitmap: ImageBitmap | null) {
  const canvas = this.canvas as HTMLCanvasElement;
  installs++;
  if (bitmap) {
    const copy = new OffscreenCanvas(bitmap.width, bitmap.height);
    copy.getContext('2d')!.drawImage(bitmap, 0, 0);
    shown.get(canvas)?.bitmap.close();
    shown.set(canvas, { width: bitmap.width, height: bitmap.height, bitmap: copy.transferToImageBitmap(), text: bitmapText.get(bitmap) ?? [] });
  } else { shown.get(canvas)?.bitmap.close(); shown.delete(canvas); }
  transfer.call(this, bitmap);
};

const panel = <T,>(hour: number, reply: T, live?: LiveValues<number | bigint | string>) =>
  createBoundedReadView({ hour, panels: new Map([['main', reply]]), ...(live ? { live: new Map([['main', live]]) } : {}) });

function liveOf(values: Readonly<Record<string, bigint>>): LiveValues<number | bigint | string> {
  return { get: key => values[key], raw: key => values[key], missing: () => 'Not recorded' };
}

const exact = (figure: string): BreakdownAmount => ({ numerator: figure, denominator: '1' });
const part = (key: string, ordinal: number, figure: string, share: number, children: BreakdownPart[] = []): BreakdownPart => ({
  key, name: key, nameMissing: null, ordinal, other: false, amount: exact(figure), state: 'known', reason: null, share, parentShare: null, count: null, last: null, art: null, children,
});
const spent: Breakdown = {
  total: { amount: exact('5972'), state: 'partial', reason: 'Spending was not recorded' },
  parts: [part('Daily costs', 3, '2071', 0.35), part('Decisions', 0, '1415', 0.24, [part('Plan A', 0, '640', 0.11), part('Plan B', 1, '352', 0.059)]), part('Advisors', 1, '1200', 0.2), part('Laws', 2, '900', 0.15)],
  unrecordedHours: 2,
};
const breakdownReply: PowerSpendingBreakdownReply = { from: H1936, hour: H1936 + 10, through: H1936 + 30, breakdown: spent, gained: exact('5306'), lostAtCap: exact('1'), gainedReason: null, lostAtCapReason: null, warnings: [] };

const art = (token: string, name: string | null) => ({ token, name, nameMissing: name ? null : 'Name refused', icon: null, iconMissing: 'No sprite in the installed mods' });
const focusHour = H(1941, 9, 15);
const focusReply: FocusCardReply = {
  hour: focusHour, validThrough: focusHour + 23, missing: null,
  current: { art: art('armament', 'Armament Rationalization'), continuous: false, paused: false, totalDays: 70, elapsedDays: 31, permyriad: 4571, daysLeft: 38, progressMissing: null },
  completed: [
    { art: art('fall_gelb', 'Fall Gelb'), completedByHour: focusHour - 30 * 24, dateMissing: null },
    { art: art('rhineland', 'Rhineland'), completedByHour: focusHour - 90 * 24, dateMissing: null },
    { art: art('four_year_plan', null), completedByHour: null, dateMissing: 'Already complete when first read' },
  ],
  undated: 1, warnings: [],
};

const flowHour = 60_810_000;
const amount = (value: number) => String(value * 100_000);
const sideOf = (links: FlowLink[], total: string | null) => ({ links, total, missing: null, unranked: null });
const link = (from: number, to: number, parts: [number, number][]): FlowLink => ({
  from, to, others: null, amounts: parts.map(([at, value]) => ({ part: at, amount: amount(value) })), total: amount(parts.reduce((sum, [, value]) => sum + value, 0)), missing: null,
});
const endpoint = (slot: number, tag: string) => ({ key: `s${slot}`, slot, tag, collective: false, unknown: null });
const flowReply: CountryFlowReply = {
  endpoints: [endpoint(1, 'USA'), endpoint(2, 'HOL'), endpoint(3, 'RAJ'), endpoint(6, 'ENG')], subject: 0, decimals: 5,
  parts: [{ key: 'oil', name: 'Oil', nameMissing: null, ordinal: 0, icon: null }, { key: 'steel', name: 'Steel', nameMissing: null, ordinal: 1, icon: null }],
  incoming: sideOf([link(1, 0, [[0, 20], [1, 6]]), link(2, 0, [[1, 11]])], amount(37)), outgoing: sideOf([link(0, 3, [[0, 74]])], amount(74)),
  links: [], senders: sideOf([], '0'), receivers: sideOf([], '0'), table: [], largest: amount(74), largestLink: amount(74), missing: null, warnings: [],
};

const masteryHour = 61_320_100;
const masteryReply: DoctrineMasteryReply = {
  warnings: [],
  folders: [{
    ordinal: 0, id: 'land_doctrine', name: 'Land', icon: null, grand: { id: 'mobile_warfare', name: 'Mobile Warfare', icon: null }, completed: [],
    tracks: [
      { ordinal: 0, name: 'Armour', sub: { id: 'armored_spearhead', name: 'Armored Spearhead', icon: null }, unlocked: 2, rewardsUnknown: null,
        rewards: [{ id: 'r1', name: 'Breakthrough', threshold: '5000000' }, { id: 'r2', name: 'Blitz', threshold: '5000000' }, { id: 'r3', name: 'Modern Blitzkrieg', threshold: '10000000' }] },
      { ordinal: 1, name: 'Infantry', sub: { id: 'infantry_offensive', name: 'Infantry Offensive', icon: null }, unlocked: 1, rewardsUnknown: null,
        rewards: [{ id: 'i1', name: 'Grand Offensive', threshold: '6000000' }, { id: 'i2', name: 'Assault', threshold: '6000000' }] },
    ],
  }],
};
const masteryLive = liveOf({
  [liveKey('0|0', 'mastery')]: 12_400_000n, [liveKey('0|0', 'effective')]: 150_000n, [liveKey('0|0', 'daily')]: 100_000n, [liveKey('0|0', 'bank')]: 0n,
  [liveKey('0|1', 'mastery')]: 7_100_000n, [liveKey('0|1', 'effective')]: 90_000n, [liveKey('0|1', 'daily')]: 60_000n, [liveKey('0|1', 'bank')]: 2_000_000n,
});

const idea = (token: string, slot: string) => ({ token, category: 'government', slot, law: true, designer: false,
  effects: [], targeted: [], equipment: [], research: [], traits: [], tooltips: [], effectsKnown: true, unknown: null, nameMissing: null });
const looks: IdeaLook[] = [
  { info: idea('civilian', 'economy'), artwork: { token: 'civilian', name: 'Civilian Economy', description: null, baseDurationDays: null, icon: null } },
  { info: idea('war', 'economy'), artwork: { token: 'war', name: 'War Economy', description: null, baseDurationDays: null, icon: null } },
  { info: idea('free', 'trade'), artwork: { token: 'free', name: 'Free Trade', description: null, baseDurationDays: null, icon: null } },
  { info: idea('autarky', 'trade'), artwork: { token: 'autarky', name: 'Autarky', description: null, baseDurationDays: null, icon: null } },
];
const timelineHour = H(1940);
const timelineReply: IdeaHistoryReply = {
  branchId: '1', tag: 'GER', asked: 'GER', hour: timelineHour, nextChange: H(1941) + 1, unrecorded: null,
  holdings: [
    { token: 'civilian', firstHour: H(1936), lastHour: H(1937, 6) - 1, start: 'heldBefore', end: { kind: 'ended' }, slot: null, character: null, look: 0 },
    { token: 'free', firstHour: H(1936), lastHour: H(1938, 6) - 1, start: 'heldBefore', end: { kind: 'endedBy', hour: H(1938, 10) }, slot: null, character: null, look: 2 },
    { token: 'war', firstHour: H(1937, 6), lastHour: timelineHour, start: 'seen', end: { kind: 'held' }, slot: null, character: null, look: 1 },
    { token: 'autarky', firstHour: H(1938, 10), lastHour: timelineHour, start: 'firstObserved', end: { kind: 'held' }, slot: null, character: null, look: 3 },
  ],
  held: [2, 3], looks,
  categories: [{ token: 'government', name: 'Laws', kind: null, hidden: false, politicsTab: true, slots: [
    { token: 'economy', name: 'Economy law', character: false, seats: 1, laws: ['civilian', 'war'] },
    { token: 'trade', name: 'Trade law', character: false, seats: 1, laws: ['free', 'autarky'] },
  ] }],
  unobserved: [{ firstHour: H(1938, 6), lastHour: H(1938, 10) - 1 }], modifiers: [], texts: [], warnings: [],
};

const axis = (id: string, label: string, group: [string, string, string]): MatrixAxisEntity => ({ id, label, tag: id, group: { id: group[0], label: group[1], color: group[2] } });
const ALLIES: [string, string, string] = ['f:1', 'Allies', '#4f86f7'];
const AXIS: [string, string, string] = ['f:2', 'Axis', '#e55353'];
const matrixRows = [axis('ENG', 'United Kingdom', ALLIES), axis('FRA', 'France', ALLIES), axis('GER', 'German Reich', AXIS), axis('ITA', 'Italy', AXIS)];
const matrixGrid: MatrixCell[][] = matrixRows.map((_row, r) => matrixRows.map((_column, c) => r === c ? { kind: 'diagonal' as const }
  : (r + c) % 3 === 0 ? { kind: 'missing' as const, reason: 'Opinions not recorded' }
    : { kind: 'value' as const, color: (r + c) % 2 ? '#2eb872' : '#e55353', text: `${(r + 1) * (c + 2) * ((r + c) % 2 ? 7 : -9)}`, value: (r + 1) * (c + 2) }));
const matrixModel: HeatMatrixModel = {
  hour: 240, rows: matrixRows, columns: matrixRows,
  cells: { present: new Map(matrixGrid.flatMap((row, r) => row.map((cell, c) => [r * matrixRows.length + c, cell] as const))), absent: { kind: 'empty' } },
  legend: { gradient: { lowLabel: '-80', highLabel: '+50', stops: ['#e55353', '#3a4556', '#2eb872'] }, entries: [{ id: 'missing', label: 'Not recorded', color: '#3a4556', hatched: true }] },
};
const lanesModel: HeatLanesModel = {
  hour: 119, firstHour: 0, lastHour: 167, dayHours: 24, playhead: 119,
  gaps: [{ firstHour: 48, lastHour: 71, reason: 'Recorder paused' }],
  legend: { entries: [{ id: 'allies', label: 'Allies', color: '#4f86f7' }, { id: 'axis', label: 'Axis', color: '#e55353' }] },
  rows: [
    { id: '10', label: 'English Channel', readout: { kind: 'value', primary: '70%', secondary: 'Since 4 Jan', tone: 'positive' }, runs: [
      { firstHour: 0, lastHour: 47, readHour: 47, cell: { kind: 'parts', look: 'split', parts: [{ id: 'allies', label: 'Allies', color: '#4f86f7', share: 0.7 }, { id: 'axis', label: 'Axis', color: '#e55353', share: 0.3 }] } },
      { firstHour: 72, lastHour: 119, readHour: 119, cell: { kind: 'solid', color: '#4f86f7', label: 'Allies' } },
    ] },
    { id: '11', label: 'Northern France', readout: { kind: 'missing', figure: 'Air', reason: 'Scores withheld' }, runs: [{ firstHour: 0, lastHour: 119, readHour: 119, cell: { kind: 'missing', reason: 'Scores withheld' } }] },
    { id: '12', label: 'North Sea', readout: { kind: 'value', primary: '35%', secondary: 'Since 2 Jan', tone: 'negative' }, runs: [{ firstHour: 0, lastHour: 119, readHour: 119, cell: { kind: 'solid', color: '#e55353', label: 'Axis' } }] },
  ],
};
const chartHour = H1936 + 2000;
const chartModel: TimeChartModel = {
  kind: 'pixel-time-chart', measureTitle: 'Political power', mode: 'lines', hour: chartHour, extent: [H1936, chartHour], yAxis: { format: 'power' },
  series: [{
    id: 'ger', key: 'GER', name: 'German Reich', color: measureColour('politicalPower', '#111111'), role: 'primary', area: true, negRed: true,
    points: Array.from({ length: 21 }, (_, i) => ({ hour: H1936 + i * 100, value: 40 + 30 * Math.sin(i / 3) + i * 2 })), headValue: 80, headHour: chartHour, absentReason: null,
  }],
};
const barsHour = H(1941, 9, 15);
const barsModel = {
  form: 'ordered', hour: barsHour, scaleMax: 300, animateOrder: true,
  rows: [['GER', 'German Reich', 293, '#5f6b7a'], ['USA', 'United States', 254, '#3b6fd6'], ['SOV', 'Soviet Union', 219, '#b8453a'], ['ENG', 'United Kingdom', 141, '#c96b5a'], ['JAP', 'Japan', 97, '#d9d0b8'], ['ITA', 'Italy', 64, '#3f8f4f']]
    .map(([key, label, value, color]) => ({ kind: 'data', key, label: knownValue(label as string), color, value: { value: knownValue(value as number), format: 'integer' }, numeric: value })),
};
const tableHour = 1000;
const tableProjection = declareTableProjection<{ rows: { id: string; name: string; factories: number; divisions: number }[] }, { id: string; name: string; factories: number; divisions: number }, Record<string, never>>({
  kind: 'pixel-table',
  columns: [
    { key: 'name', label: 'Country', as: 'plain', offered: ['plain'], priority: 0, text: true, path: 'name' },
    { key: 'factories', label: 'Factories', as: 'plain', offered: ['plain'], priority: 1, numeric: true, format: 'integer', path: 'factories' },
    { key: 'divisions', label: 'Divisions', as: 'plain', offered: ['plain'], priority: 2, numeric: true, format: 'integer', path: 'divisions' },
  ],
  rows: reply => ({ rows: reply.rows }),
  rowKey: row => row.id,
  rowName: row => row.name,
});
const tableRows = Array.from({ length: 40 }, (_, i) => ({ id: `C${i}`, name: `Country number ${i + 1}`, factories: 400 - i * 7, divisions: 120 - i * 2 }));
const tableSettings = tableSettingsOf(undefined, [
  { key: 'name', label: 'Country', as: 'plain', offered: ['plain'] },
  { key: 'factories', label: 'Factories', as: 'plain', offered: ['plain'] },
  { key: 'divisions', label: 'Divisions', as: 'plain', offered: ['plain'] },
], { density: 'comfortable', sort: { column: 'factories', direction: 'descending' } });

interface Placed {
  readonly id: string;
  readonly family: FamilyId;
  readonly adapter: FamilyHostAdapter<any, any, any, any>;
  readonly declaration: FamilyProjection<any, any>;
  readonly hour: number;
  readonly panel: unknown;
  readonly extent?: readonly [number, number];
  readonly settings?: unknown;
}

function fixed(family: FamilyId, extra: Record<string, unknown> = {}): FamilyProjection<unknown, unknown> {
  return registerFamilyProjection({ family, kind: `pixel-${family}`, project: view => view.panel('main'), ...extra } as FamilyProjection<unknown, unknown>);
}

const PLACED: readonly Placed[] = [
  { id: 'bars', family: 'bars', adapter: barsHostAdapter, declaration: fixed('bars', { nextChange: () => Infinity }), hour: barsHour, panel: barsModel },
  { id: 'breakdown', family: 'breakdown', adapter: breakdownHostAdapter, declaration: fixed('breakdown'), hour: H1936 + 10, panel: ppSpendingProjection.project(panel(H1936 + 10, breakdownReply), { digits: 0 }) },
  { id: 'card', family: 'card', adapter: cardHostAdapter, declaration: fixed('card'), hour: focusHour, panel: nationalFocusProjection.project(panel(focusHour, focusReply), {}) },
  { id: 'flow', family: 'flow', adapter: flowHostAdapter, declaration: fixed('flow'), hour: flowHour, panel: tradeFlowProjection.project(panel(flowHour, flowReply), {}) },
  { id: 'heat-lanes', family: 'heat-lanes', adapter: heatLanesHostAdapter, declaration: fixed('heat-lanes'), hour: lanesModel.hour, panel: lanesModel },
  { id: 'heat-matrix', family: 'heat-matrix', adapter: heatMatrixHostAdapter, declaration: fixed('heat-matrix'), hour: matrixModel.hour, panel: matrixModel },
  { id: 'meters', family: 'meters', adapter: meterHostAdapter, declaration: fixed('meters'), hour: masteryHour, panel: doctrineMasteryProjection.project(panel(masteryHour, masteryReply, masteryLive), { folders: [], countryName: 'Germany' }) },
  { id: 'table', family: 'table', adapter: tableHostAdapter, declaration: registerFamilyProjection(tableProjection), hour: tableHour, panel: { rows: tableRows }, settings: tableSettings },
  { id: 'time-chart', family: 'time-chart', adapter: timeChartHostAdapter, declaration: fixed('time-chart', { variant: 'line', joinMode: 'join', measureTitle: 'Political power' }), hour: chartHour, panel: chartModel, extent: [H1936, chartHour] },
  { id: 'timeline', family: 'timeline', adapter: timelineHostAdapter, declaration: fixed('timeline'), hour: timelineHour, panel: ideaTimelineProjection.project(panel(timelineHour, timelineReply), { category: '', iconsOnBars: true, firstHour: H(1936), surface: '#161616' }) },
];

const statics = new Map<string, FamilyStaticSources>(PLACED.map(item => [item.id, { stamp: {}, proof: null, panels: { main: item.panel }, hour: item.hour }]));
const widgetOf = (id: string) => ({ id, kind: id, trackIds: ['t1'], time: { mode: 'follow' }, countries: { kind: 'all' }, options: {}, layout: { x: 0, y: 0, w: 4, h: 8 } }) as unknown as WidgetSpec;
const frame = { token: 1, announce: () => {}, clearAnnouncement: () => {}, setSkeleton: () => {}, reportState: () => {}, setDynamicViewSwitch: () => {} };
const NO_SUBSCRIPTIONS: never[] = [];
const OPTIONS = {};
let layoutReads = 0;
let playbackLayoutReads = 0;
const boundingRect = Element.prototype.getBoundingClientRect;
Element.prototype.getBoundingClientRect = function () { layoutReads++; return boundingRect.call(this); };

interface Look { readonly layout: Layout; readonly width: number; readonly zoom: Readonly<Record<string, number>>; readonly motionAt?: number }
const START_LAYOUT: Layout = PLACED.map((item, at) => ({ i: item.id, x: (at % 3) * 4, y: Math.floor(at / 3) * 8, w: 4, h: 8 }));
let setLook: (look: Look) => void = () => {};
let look: Look = { layout: START_LAYOUT, width: 0, zoom: {} };

function Dashboard() {
  const [state, set] = useState(look);
  setLook = next => { look = next; flushSync(() => set(next)); };
  const container = useRef<HTMLDivElement>(null);
  const placement = useGridPlacement(container);
  return <ExportSurfaceContext.Provider value={{ active: false, ...(state.motionAt !== undefined ? { motion: { nowMs: state.motionAt, speed: 1, easeValues: true, glideDurationMs: 180 } } : {}) }}><div className="dash-grid" ref={container} style={{ width: `${state.width}px` }}>
    <Responsive width={state.width} layouts={{ lg: state.layout }} breakpoints={{ lg: 0 }} cols={{ lg: 12 }} rowHeight={26} margin={[6, 6]} containerPadding={[0, 0]}
      positionStrategy={placement} dragConfig={{ enabled: false }} resizeConfig={{ enabled: false }}>
      {PLACED.map(item => <div key={item.id} className="dash-grid-cell">
        <article className="dash-widget" data-widget={item.id}>
          <div className="dash-widget-header"><h2>{item.id}</h2></div>
          <div className="dash-widget-content">
            <div className="dash-widget-scaled" style={{ zoom: state.zoom[item.id] ?? 1 }}>
              <FollowedWidgetKey.Provider value={item.id}>
                <FamilyHost adapter={item.adapter} declaration={item.declaration} view="default" options={OPTIONS} label={item.id} subject={item.id} widget={widgetOf(item.id)}
                  runtime={runtime} active subscriptions={NO_SUBSCRIPTIONS} staticSources={statics.get(item.id)!} offset={state.motionAt === undefined ? 0 : item.hour - 1000} pin={state.motionAt === undefined ? item.hour : null} bounds={BOUNDS}
                  {...(item.family === 'table' ? { chrome: TableChrome as React.ComponentType<any> } : {})}
                  {...(item.extent ? { extent: item.extent } : {})} {...(item.settings ? { settings: item.settings } : {})} />
              </FollowedWidgetKey.Provider>
            </div>
          </div>
        </article>
      </div>)}
    </Responsive>
  </div></ExportSurfaceContext.Provider>;
}

const frames = (count = 1) => new Promise<void>(resolve => {
  const step = (left: number) => requestAnimationFrame(() => (left <= 1 ? resolve() : step(left - 1)));
  step(count);
});

const canvases = () => [...document.querySelectorAll<HTMLCanvasElement>('canvas.fh-canvas')];
const scroller = () => document.getElementById('dashboard-scroll')!;
const familyOf = (canvas: HTMLCanvasElement) => canvas.closest<HTMLElement>('[data-widget]')!.dataset.widget!;

async function settle(limitMs = 15_000): Promise<{ readonly settled: boolean; readonly pending: readonly string[] }> {
  const deadline = performance.now() + limitMs;
  let quiet = 0;
  let last = installs;
  while (performance.now() < deadline) {
    await frames();
    const pending = canvases().filter(canvas => !shown.get(canvas));
    if (installs !== last || pending.length) { quiet = 0; last = installs; continue; }
    if (++quiet >= 8) return { settled: true, pending: [] };
  }
  return { settled: false, pending: canvases().filter(canvas => !shown.get(canvas)).map(familyOf) };
}

function deviceBoxes(): Promise<Map<HTMLCanvasElement, readonly [number, number]>> {
  return new Promise(resolve => {
    const list = canvases();
    const observer = new ResizeObserver(entries => {
      observer.disconnect();
      resolve(new Map(entries.map(entry => [entry.target as HTMLCanvasElement, [entry.devicePixelContentBoxSize[0]!.inlineSize, entry.devicePixelContentBoxSize[0]!.blockSize] as const])));
    });
    for (const canvas of list) observer.observe(canvas, { box: 'device-pixel-content-box' });
  });
}

function visibleClip(canvas: HTMLCanvasElement): { readonly clip: DOMRect; readonly corners: readonly DOMRect[] } {
  let left = 0, top = 0, right = innerWidth, bottom = innerHeight;
  const corners: DOMRect[] = [];
  for (let node: Element | null = canvas; node; node = node.parentElement) {
    const box = node.getBoundingClientRect();
    const style = getComputedStyle(node);
    const clips = node === canvas || style.overflow !== 'visible';
    if (!clips) continue;
    const inner = node === canvas ? box : new DOMRect(box.left + node.clientLeft, box.top + node.clientTop, node.clientWidth, node.clientHeight);
    left = Math.max(left, inner.left); top = Math.max(top, inner.top); right = Math.min(right, inner.right); bottom = Math.min(bottom, inner.bottom);
    const radius = Math.max(parseFloat(style.borderTopLeftRadius) || 0, parseFloat(style.borderTopRightRadius) || 0, parseFloat(style.borderBottomLeftRadius) || 0, parseFloat(style.borderBottomRightRadius) || 0);
    if (radius > 0) for (const [x, y] of [[box.left, box.top], [box.right - radius, box.top], [box.left, box.bottom - radius], [box.right - radius, box.bottom - radius]] as const) corners.push(new DOMRect(x - 1, y - 1, radius + 2, radius + 2));
  }
  for (const edge of canvas.closest('.fh-frame')!.querySelectorAll('.tf-edges > i')) {
    if (getComputedStyle(edge).display !== 'none') {
      const box = edge.getBoundingClientRect();
      corners.push(new DOMRect(box.left - 1, box.top - 1, box.width + 2, box.height + 2));
    }
  }
  return { clip: new DOMRect(left, top, Math.max(0, right - left), Math.max(0, bottom - top)), corners };
}

async function decode(png: string): Promise<ImageData> {
  const bytes = Uint8Array.from(atob(png), character => character.charCodeAt(0));
  const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
  const board = new OffscreenCanvas(bitmap.width, bitmap.height).getContext('2d')!;
  board.drawImage(bitmap, 0, 0);
  return board.getImageData(0, 0, bitmap.width, bitmap.height);
}

export interface Placement {
  readonly id: string;
  readonly backing: readonly [number, number];
  readonly box: readonly [number, number];
  readonly css: readonly [number, number];
  readonly dpr: number;
  readonly origin: readonly [number, number];
  readonly offset: readonly [number, number];
  readonly textDraws: number;
  readonly fractionalText: number;
  readonly textError: number;
}

const specified = (canvas: HTMLCanvasElement, property: 'width' | 'height') => (canvas.attributeStyleMap.get(property) as CSSUnitValue).value;

async function placements(): Promise<Placement[]> {
  const boxes = await deviceBoxes();
  return canvases().map(canvas => {
    const rect = canvas.getBoundingClientRect();
    const body = canvas.closest('.fh-body')!.getBoundingClientRect();
    const draws = shown.get(canvas)?.text ?? [];
    const error = (p: TextDraw) => Math.max(Math.abs(p.x - Math.round(p.x)), Math.abs(p.y - Math.round(p.y)));
    return {
      id: familyOf(canvas), backing: [shown.get(canvas)?.width ?? 0, shown.get(canvas)?.height ?? 0], box: boxes.get(canvas) ?? [0, 0],
      css: [specified(canvas, 'width'), specified(canvas, 'height')], dpr: devicePixelRatio * canvas.currentCSSZoom,
      origin: [rect.left * devicePixelRatio, rect.top * devicePixelRatio],
      offset: [(rect.left - body.left) * devicePixelRatio, (rect.top - body.top) * devicePixelRatio],
      textDraws: draws.length, fractionalText: draws.filter(p => error(p) > 0.01).length, textError: Math.max(0, ...draws.map(error)),
    };
  });
}

export interface Exactness extends Placement {
  readonly compared: number;
  readonly differing: number;
  readonly worst: number;
  readonly shift: readonly [number, number] | null;
  readonly samples: readonly string[];
}

async function exactness(withCanvases: string, withoutCanvases: string): Promise<Exactness[]> {
  const screen = await decode(withCanvases);
  const under = await decode(withoutCanvases);
  const dpr = devicePixelRatio;
  const placed = await placements();
  return Promise.all(canvases().map(async (canvas, at) => {
    const held = shown.get(canvas)!;
    const { clip, corners } = visibleClip(canvas);
    const rounded = (x: number, y: number) => corners.some(corner => x >= corner.left * dpr && x < corner.right * dpr && y >= corner.top * dpr && y < corner.bottom * dpr);
    const rect = canvas.getBoundingClientRect();
    const x0 = Math.ceil(clip.left * dpr) + 1, y0 = Math.ceil(clip.top * dpr) + 1;
    const x1 = Math.floor(clip.right * dpr) - 1, y1 = Math.floor(clip.bottom * dpr) - 1;
    const width = x1 - x0, height = y1 - y0;
    let best: { compared: number; differing: number; worst: number; shift: readonly [number, number]; samples: string[] } | null = null;
    if (width > 0 && height > 0) {
      for (const dy of [0, -1, 1]) for (const dx of [0, -1, 1]) {
        const left = Math.round(rect.left * dpr) + dx, top = Math.round(rect.top * dpr) + dy;
        const board = new OffscreenCanvas(width, height).getContext('2d')!;
        board.putImageData(under, -x0, -y0, x0, y0, width, height);
        board.drawImage(held.bitmap, left - x0, top - y0);
        const want = board.getImageData(0, 0, width, height).data;
        let compared = 0, differing = 0, worst = 0;
        const samples: string[] = [];
        for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
          if (rounded(x0 + x, y0 + y)) continue;
          compared++;
          const s = ((y0 + y) * screen.width + x0 + x) * 4, w = (y * width + x) * 4;
          const d = Math.max(Math.abs(screen.data[s]! - want[w]!), Math.abs(screen.data[s + 1]! - want[w + 1]!), Math.abs(screen.data[s + 2]! - want[w + 2]!));
          if (d > 1 && samples.length < 4) samples.push(`(${x0 + x - left},${y0 + y - top}) screen ${[...screen.data.subarray(s, s + 3)].join(',')} painted ${[...want.subarray(w, w + 3)].join(',')}`);
          if (d > 1) differing++;
          if (d > worst) worst = d;
        }
        if (!best || differing < best.differing) best = { compared, differing, worst, shift: [dx, dy], samples };
        if (differing === 0) break;
      }
    }
    return { ...placed[at]!, compared: best?.compared ?? 0, differing: best?.differing ?? 0, worst: best?.worst ?? 0, shift: best?.shift ?? null, samples: best?.samples ?? [] };
  }));
}

function hideCanvases(hidden: boolean) {
  for (const canvas of canvases()) canvas.style.visibility = hidden ? 'hidden' : '';
}

let root: ReturnType<typeof createRoot> | null = null;

function pausedTableFit() {
  applyAppearance(document.documentElement, DEFAULT_APPEARANCE, resolveAppearance(DEFAULT_APPEARANCE, true, false));
  const theme = readFamilyTheme()!;
  const surface = { width: 600, height: 160, dpr: window.devicePixelRatio };
  const lane = createLaneView(tableLane, () => {});
  const state = new Map<string, unknown>();
  let revision = 0;
  const picture = (target: Map<string, unknown>, hour: number, speed: number, name: string) => {
    const painted = lane.paint({
      widget: 1, subs: [], pictures: [], hour, speed, seek: 0, inspect: null, now: 0,
      binding: { generation: 1, surfaces: [surface], offset: 0, pin: null },
      model: {
        kind: tableProjection.kind, family: 'table', theme, settings: tableSettings,
        sources: {}, options: {}, staticPanels: { main: { rows: [{ id: 'C0', name, factories: 40, divisions: 7 }] } },
        revisions: { sources: 1, options: 1, settings: 1, theme: 1, staticPanels: ++revision },
        width: surface.width, height: surface.height, scrollTop: 0, scrollLeft: 0, reducedMotion: true,
      },
    }, target, () => String(revision));
    const canvas = painted.surfaces[0]?.source;
    if (!(canvas instanceof OffscreenCanvas)) throw new Error('The table did not paint');
    return canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data;
  };
  const short = 'i'.repeat(18);
  const corrected = 'W'.repeat(short.length);
  const baseline = picture(new Map(), 2, 0, short);
  picture(state, 1, 24, 'A country name with enough text to widen the fitted column');
  const playing = picture(state, 2, 24, short);
  const paused = picture(state, 2, 0, short);
  const correction = picture(state, 2, 0, corrected);
  const correctedBaseline = picture(new Map(), 2, 0, corrected);
  const differences = (data: Uint8ClampedArray, exact = baseline) => exact.reduce((count, pixel, i) => count + Number(pixel !== data[i]), 0);
  return { playing: differences(playing), paused: differences(paused), correction: differences(correction, correctedBaseline) };
}

function pausedFuelReadout(width: number, names = ['Expeditionary Infantry Command', 'Fleet'],
  amounts = [['100000000', '90000000'], ['1000000000', '1000000000']], capture = false) {
  const dpr = window.devicePixelRatio;
  const height = 150;
  applyAppearance(document.documentElement, DEFAULT_APPEARANCE, resolveAppearance(DEFAULT_APPEARANCE, true, false));
  const base = readFamilyTheme()!;
  const theme = { ...base, font: 'Segoe UI', fontSize: { ...base.fontSize, body: 15 } };
  const sprite = new OffscreenCanvas(16, 16);
  const art = sprite.getContext('2d')!;
  art.fillStyle = theme.accent;
  art.fillRect(1, 1, 14, 14);
  const bitmap = sprite.transferToImageBitmap();
  const icon = { ...readyArt('fixture-fuel-icon', { frame: defaultArtFrame(16, 16) }), bitmap };
  const reply = {
    tag: 'GER', scale: 100000, consumerNames: names, consumerIcons: names.map(() => 'fixture-branch-icon'),
    consumerIconReasons: names.map(() => null), fuelIcon: 'fixture-fuel-icon', fuelIconReason: null,
    readHour: 100, missing: null, warnings: [],
    consumers: amounts.map(([requested, received], index) => ({ index, requested, received, priority: index + 1 })),
  };
  const model = fuelBranchesProjection.project(createBoundedReadView({
    hour: displayedHour(100), panels: new Map([['main', reply]]), art: { demandOwner: 'view', resolve: () => icon },
  }), { digits: 5, surface: theme.surface });
  const canvas = document.createElement('canvas');
  canvas.width = Math.round(width * dpr);
  canvas.height = Math.round(height * dpr);
  const ctx = canvas.getContext('2d')!;
  ctx.scale(dpr, dpr);
  ctx.font = `${LABEL_WEIGHT} ${theme.fontSize.body}px ${theme.font}`;
  const layout = layoutFamilyBars(model, width, height, theme, text => ctx.measureText(text).width);
  const draw = (picture: typeof model, at = layout) => {
    drawFamilyBars(ctx, { model: picture, layout: at, theme, scrollTop: 0, now: 0, glideDuration: 0 });
    ctx.save();
    ctx.globalCompositeOperation = 'destination-over';
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, 0, width, height);
    ctx.restore();
  };
  draw(model);
  const image = capture ? canvas.toDataURL('image/png') : null;
  const readoutX = Math.floor(layout.readoutX * dpr);
  const readoutPixels = () => ctx.getImageData(readoutX, 0, canvas.width - readoutX, canvas.height).data;
  const painted = readoutPixels();
  const noBars = { ...model, rows: model.rows.map(row => row.kind === 'data' ? { ...row, numeric: 0, target: undefined } : row) };
  draw(noBars);
  const labels = readoutPixels();
  let overlapPixels = 0;
  for (let i = 0; i < painted.length; i += 4) {
    if (painted[i] !== labels[i] || painted[i + 1] !== labels[i + 1] || painted[i + 2] !== labels[i + 2] || painted[i + 3] !== labels[i + 3]) overlapPixels++;
  }
  const gutterLeft = Math.ceil(layout.plotRight * dpr);
  const gutterWidth = readoutX - gutterLeft;
  let readoutInkInGutter = 0;
  if (gutterWidth > 0) {
    const gutterPixels = () => ctx.getImageData(gutterLeft, 0, gutterWidth, canvas.height).data;
    const withReadout = gutterPixels();
    draw({ ...noBars, rows: noBars.rows.map(row => row.kind === 'data' ? { ...row, readout: { primary: '' } } : row) });
    const withoutReadout = gutterPixels();
    readoutInkInGutter = withReadout.reduce((changed, value, i) => changed + Number(value !== withoutReadout[i]), 0);
  }
  const moving = { ...layout, plotLeft: layout.plotLeft - 20, plotWidth: layout.plotWidth + 20 };
  const movedLeft = Math.ceil(moving.plotLeft * dpr);
  const movedWidth = Math.floor(moving.plotRight * dpr) - movedLeft;
  draw(noBars, moving);
  const withNames = ctx.getImageData(movedLeft, 0, movedWidth, canvas.height).data;
  draw({ ...noBars, rows: noBars.rows.map(row => row.kind === 'data' ? { ...row, label: undefined } : row) }, moving);
  const withoutNames = ctx.getImageData(movedLeft, 0, movedWidth, canvas.height).data;
  const nameInkInPlot = withNames.reduce((changed, value, i) => changed + Number(value !== withoutNames[i]), 0);
  bitmap.close();
  const { nameWidth, plotLeft, plotRight, readoutX: column, readoutWidth } = layout;
  return { image, dpr, nameWidth, plotLeft, plotRight, column, readoutWidth, overlapPixels, nameInkInPlot, readoutInkInGutter };
}

const api = {
  pausedFuelReadout,
  pausedTableFit,
  async mount(motion = false): Promise<{ readonly settled: boolean; readonly pending: readonly string[]; readonly dpr: number }> {
    applyAppearance(document.documentElement, DEFAULT_APPEARANCE, resolveAppearance(DEFAULT_APPEARANCE, true, false));
    await document.fonts.ready;
    document.documentElement.style.setProperty('--surface', '#16181d');
    document.documentElement.style.setProperty('--panel', '#1b1e24');
    document.body.style.margin = '0';
    document.body.innerHTML = '<div style="position:fixed;inset:0;display:flex;flex-direction:column"><div style="flex:0 0 41.7px"></div><div id="dashboard-scroll" style="flex:1;min-height:0;overflow:auto;padding-left:10.3px"><div id="dashboard-root"></div></div></div>';
    look = { layout: START_LAYOUT, width: Math.floor(scroller().clientWidth - 10.3 - 4) + 0.4, zoom: {}, ...(motion ? { motionAt: 1000 } : {}) };
    if (motion) {
      for (const item of PLACED) statics.set(item.id, { ...statics.get(item.id)!, proof: [item.hour, item.hour + 1], follows: true });
      clock.stepExport(1000, 1, true);
    }
    root = createRoot(document.getElementById('dashboard-root')!);
    pipeline.prepare(PLACED.map(item => item.family), []);
    flushSync(() => root!.render(<WidgetFrameContext.Provider value={frame as never}><Dashboard /></WidgetFrameContext.Provider>));
    await pipeline.whenPrepared();
    return { ...await settle(20_000), dpr: devicePixelRatio };
  },
  settle,
  placements,
  motionFrames: () => Object.fromEntries(motionFrames),
  async costFrames() {
    const before = layoutReads;
    await frames(8);
    return { layoutReads: layoutReads - before };
  },
  playbackLayoutReads: () => playbackLayoutReads,
  async tableCost() {
    api.release();
    api.scrollTable(0);
    const source = statics.get('table')!;
    const rows = (source.panels!.main as { rows: typeof tableRows }).rows.slice(0, 30);
    statics.set('table', { ...source, stamp: {}, panels: { main: { rows } } });
    setLook({ ...look });
    const body = document.querySelector<HTMLElement>('[data-widget="table"] .fh-body')!;
    body.style.height = '1320px'; body.style.flex = 'none';
    await frames(12);
    const row = (await placements()).find(item => item.id === 'table')!;
    return { height: specified(canvases().find(canvas => familyOf(canvas) === 'table')!, 'height'), textDraws: row.textDraws, ...await api.costFrames() };
  },
  async smoothing() {
    const code = `onmessage = ({data: ratio}) => {
      const out = [true, false].map(alpha => {
        const canvas = new OffscreenCanvas(Math.round(260 * ratio), Math.round(48 * ratio));
        const ctx = canvas.getContext('2d', {alpha});
        ctx.fillStyle = '#202020'; ctx.fillRect(0, 0, canvas.width, canvas.height);
        ctx.scale(ratio, ratio); ctx.fillStyle = '#ededed'; ctx.font = '14px Arial';
        ctx.fillText('Moving text 123', 8, 28);
        const pixels = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
        return {alpha, pixels, bitmap: canvas.transferToImageBitmap()};
      });
      postMessage(out, out.map(item => item.bitmap));
    };`;
    const url = URL.createObjectURL(new Blob([code], { type: 'text/javascript' }));
    const worker = new Worker(url);
    try {
      const results = await new Promise<{ alpha: boolean; pixels: Uint8ClampedArray; bitmap: ImageBitmap }[]>((resolve, reject) => {
        worker.onmessage = event => resolve(event.data);
        worker.onerror = reject;
        worker.postMessage(devicePixelRatio);
      });
      let differing = 0, colored = 0;
      for (let i = 0; i < results[0]!.pixels.length; i += 4) {
        if ([0, 1, 2].some(c => results[0]!.pixels[i + c] !== results[1]!.pixels[i + c])) differing++;
        if (results[1]!.pixels[i] !== results[1]!.pixels[i + 1] || results[1]!.pixels[i] !== results[1]!.pixels[i + 2]) colored++;
      }
      const holder = document.createElement('div');
      holder.id = 'smoothing-proof'; holder.style.cssText = 'position:fixed;left:0;top:0;z-index:999;background:#202020;color:#ededed;font:14px Arial';
      holder.innerHTML = '<div style="height:48px;box-sizing:border-box;padding:12px 8px">Moving text 123</div>';
      for (const item of results) {
        const canvas = document.createElement('canvas');
        canvas.width = item.bitmap.width; canvas.height = item.bitmap.height;
        canvas.style.cssText = 'display:block;width:260px;height:48px';
        canvas.dataset.alpha = String(item.alpha);
        canvas.getContext('bitmaprenderer')!.transferFromImageBitmap(item.bitmap);
        holder.append(canvas);
      }
      document.body.append(holder);
      return { differing, colored, pixels: results[0]!.pixels.length / 4 };
    } finally { worker.terminate(); URL.revokeObjectURL(url); }
  },
  hideCanvases,
  exactness,
  scrollDashboard(top: number) { scroller().scrollTop = top; return scroller().scrollTop; },
  nudgeDashboard(left: number) { document.getElementById('dashboard-root')!.style.marginLeft = `${left}px`; },
  placeEmptyBars() {
    statics.set('bars', { stamp: {}, proof: null, hour: barsHour,
      panels: { main: { ...barsModel, rows: [], empty: 'No samples at this hour.' } } });
    setLook({ ...look });
    const cell = document.querySelector('[data-widget="bars"]')!.parentElement!;
    cell.style.cssText = 'position:fixed; left:238.4px; top:472.4px; width:774px; height:186px; z-index:10;';
  },
  moveEmptyBars() {
    const cell = document.querySelector('[data-widget="bars"]')!.parentElement!;
    cell.style.left = '238px';
    cell.style.top = '472px';
  },
  barsPicture() {
    const canvas = canvases().find(canvas => familyOf(canvas) === 'bars')!;
    const body = canvas.closest('.fh-body')!;
    const picture = pipeline.view(Number(canvas.dataset.placement));
    return { id: picture?.id, binding: picture?.binding, hour: picture?.through,
      busy: body.closest('.fh-frame')!.getAttribute('aria-busy'), width: body.clientWidth, height: body.clientHeight };
  },
  async barsLeadingEdgeAtSmallScale() {
    document.documentElement.style.zoom = '0.75';
    api.zoom({ bars: 0.5 });
    api.placeEmptyBars();
    await frames(6);
    const canvas = canvases().find(canvas => familyOf(canvas) === 'bars')!;
    const extent = canvas.parentElement!;
    const body = extent.parentElement!;
    const cell = canvas.closest('[data-widget="bars"]')!.parentElement!;
    const top = body.getBoundingClientRect().top * devicePixelRatio;
    const nudge = (Math.floor(top) + 0.49 - top) / (devicePixelRatio * cell.currentCSSZoom);
    cell.style.top = `${parseFloat(cell.style.top) + nudge}px`;
    await frames(6);
    const leading = canvas.getBoundingClientRect().top;
    return { offset: (leading - body.getBoundingClientRect().top) / canvas.currentCSSZoom,
      inside: leading >= extent.getBoundingClientRect().top };
  },
  scrollTable(top: number) {
    const body = document.querySelector<HTMLElement>('[data-widget="table"] .fh-body')!;
    body.scrollTop = top;
    return body.scrollTop;
  },
  resize(id: string, w: number, h: number, widthChange: number) {
    setLook({ ...look, width: look.width + widthChange, layout: look.layout.map(item => (item.i === id ? { ...item, w, h } : item)) });
  },
  zoom(zoom: Readonly<Record<string, number>>) { setLook({ ...look, zoom }); },
  move() { setLook({ ...look, layout: look.layout.map(item => ({ ...item, x: item.x === 0 ? 8 : item.x === 8 ? 0 : item.x })) }); },
  oddHeader() { document.querySelectorAll<HTMLElement>('.dash-widget-header').forEach(node => { node.style.height = '39px'; node.style.minHeight = '39px'; }); },
  tallTable() { document.querySelector('.tf-head-clip')!.classList.add('is-grouped'); },
  async glide() {
    statics.set('bars', { stamp: {}, proof: [barsHour + 1, barsHour + 1], hour: barsHour + 1, follows: true, panels: { main: { ...barsModel, hour: barsHour + 1, rows: [...barsModel.rows].reverse() } } });
    statics.set('table', { stamp: {}, proof: [tableHour + 1, tableHour + 1], hour: tableHour + 1, follows: true, panels: { main: { rows: tableRows.map((row, i) => ({ ...row, factories: i === 0 ? 300 : i === 1 ? 450 : row.factories })) } } });
    setLook({ ...look });
    clock.stepExport(1001, 1, false);
    await frames(8);
    setLook({ ...look, motionAt: 1067 });
    await frames(8);
    playbackLayoutReads = (await api.costFrames()).layoutReads;
    const before = layoutReads;
    const extent = canvases().find(canvas => familyOf(canvas) === 'table')!.parentElement!;
    for (let i = 0; i < 8; i++) {
      extent.style.height = `${2000 + i * 44}px`;
      await frames();
    }
    playbackLayoutReads += layoutReads - before;
    api.hold();
    return settle();
  },
  hold() {
    const undim = document.createElement('style');
    undim.textContent = '[data-held] { opacity: 1 !important; transition: none !important; }';
    document.head.append(undim);
    held = new Promise(resolve => { release = () => { held = null; undim.remove(); resolve(); }; });
  },
  release() { release(); },
  frames,
};

(window as unknown as { pixelExact: typeof api }).pixelExact = api;
