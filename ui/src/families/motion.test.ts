import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { Lane, type Bitmapped, type LaneHost, type Presented } from '../draw/lane.ts';
import { contextWidgetKey } from '../draw/kit.ts';
import { datedPanel, datedRecord, message } from '../dashboard/follow/messages.fixture.ts';
import { Page, SAME, type Ack, type Refusal } from '../dashboard/follow/page.ts';
import type { FamilyTheme } from './contract.ts';
import { createLaneView } from './laneRuntime.ts';
import { type MotionClock } from './motion.ts';
import { registerFamilyProjection } from './registry.ts';
import { barsLane, type FamilyBarsBinding } from './bars/laneView.ts';
import type { BarsModel } from './bars/model.ts';
import { RecordedPath2D } from './time-chart/path2d.fixture.ts';
import { knownValue, notRecorded } from './value.ts';
import { defaultArtFrame, readyArt } from './art/types.ts';

const TEST_THEME: FamilyTheme = {
  surface: '#11161d',
  raised: '#1a222d',
  dark: true,
  text: '#eef2f6',
  muted: '#8d9bb0',
  line: '#2a3444',
  lineSoft: '#202836',
  accent: '#4f86f7',
  blue: '#4f86f7',
  red: '#e5534b',
  shortage: '#ff6b6f',
  amber: '#d29922',
  green: '#3fb950',
  grey: '#8d9bb0',
  hatch: '#6e7681',
  hatchEdge: '#484f58',
  font: 'Inter',
  fontSize: { small: 11, body: 12 },
  lineWidth: 1,
  hatchSpacing: 6,
};

const baseClock = (overrides: Partial<MotionClock> = {}): MotionClock => ({
  playing: true,
  anchorPosition: 100,
  anchorTimeMs: 1000,
  speed: 1,
  seek: 1,
  exactHour: 100,
  ...overrides,
});

interface CanvasOp {
  readonly op: string;
  readonly args: readonly unknown[];
  readonly fillStyle: unknown;
  readonly strokeStyle: unknown;
  readonly font: string;
  readonly globalAlpha: number;
  readonly textAlign: string;
  readonly textBaseline: string;
  readonly tx: number;
  readonly ty: number;
}

function makeRecordingCanvas(width = 960, height = 300) {
  const opsByWidget = new Map<object, CanvasOp[]>();
  const ownOps: CanvasOp[] = [];
  const currentKey = () => {
    const key = contextWidgetKey(ctx as unknown as CanvasRenderingContext2D);
    return key === ctx ? undefined : key;
  };
  const record = (op: string, args: readonly unknown[]) => {
    const entry: CanvasOp = {
      op,
      args,
      fillStyle: ctx.fillStyle,
      strokeStyle: ctx.strokeStyle,
      font: ctx.font,
      globalAlpha: ctx.globalAlpha,
      textAlign: ctx.textAlign,
      textBaseline: ctx.textBaseline,
      tx: transform.e,
      ty: transform.f,
    };
    const key = clipped ? currentKey() : undefined;
    if (!key) {
      ownOps.push(entry);
      return;
    }
    let list = opsByWidget.get(key);
    if (!list) opsByWidget.set(key, list = []);
    list.push(entry);
  };
  let clipped = false;
  let transform = { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 };
  const stack: {
    fillStyle: unknown;
    strokeStyle: unknown;
    font: string;
    globalAlpha: number;
    lineWidth: number;
    textAlign: CanvasTextAlign;
    textBaseline: CanvasTextBaseline;
    transform: typeof transform;
    clipped: boolean;
  }[] = [];
  const gradient = () => ({ addColorStop() {} });
  const ctx = {
    font: '400 12px Inter',
    fillStyle: '' as unknown,
    strokeStyle: '' as unknown,
    lineWidth: 1,
    lineJoin: 'miter' as CanvasLineJoin,
    lineCap: 'butt' as CanvasLineCap,
    globalAlpha: 1,
    textAlign: 'left' as CanvasTextAlign,
    textBaseline: 'alphabetic' as CanvasTextBaseline,
    save() {
      stack.push({
        fillStyle: this.fillStyle,
        strokeStyle: this.strokeStyle,
        font: this.font,
        globalAlpha: this.globalAlpha,
        lineWidth: this.lineWidth,
        textAlign: this.textAlign,
        textBaseline: this.textBaseline,
        transform: { ...transform },
        clipped,
      });
    },
    restore() {
      const prev = stack.pop();
      if (prev) {
        this.fillStyle = prev.fillStyle;
        this.strokeStyle = prev.strokeStyle;
        this.font = prev.font;
        this.globalAlpha = prev.globalAlpha;
        this.lineWidth = prev.lineWidth;
        this.textAlign = prev.textAlign;
        this.textBaseline = prev.textBaseline;
        transform = prev.transform;
        clipped = prev.clipped;
      }
    },
    setTransform(a: number, b: number, c: number, d: number, e: number, f: number) {
      transform = { a, b, c, d, e, f };
    },
    resetTransform() {
      transform = { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 };
    },
    getTransform: () => ({ ...transform }),
    translate(x: number, y: number) {
      transform = { ...transform, e: transform.e + x * transform.a, f: transform.f + y * transform.d };
    },
    scale(x: number, y: number) {
      transform = { ...transform, a: transform.a * x, d: transform.d * y };
    },
    clearRect(x: number, y: number, w: number, h: number) { record('clearRect', [x, y, w, h]); },
    fillRect(x: number, y: number, w: number, h: number) { record('fillRect', [x, y, w, h]); },
    strokeRect(x: number, y: number, w: number, h: number) { record('strokeRect', [x, y, w, h]); },
    beginPath() { record('beginPath', []); },
    closePath() { record('closePath', []); },
    rect(x: number, y: number, w: number, h: number) { record('rect', [x, y, w, h]); },
    roundRect(...args: unknown[]) { record('roundRect', args); },
    clip() { clipped = true; },
    moveTo(x: number, y: number) { record('moveTo', [x, y]); },
    lineTo(x: number, y: number) { record('lineTo', [x, y]); },
    quadraticCurveTo(...args: number[]) { record('quadraticCurveTo', args); },
    arc(...args: number[]) { record('arc', args); },
    arcTo(...args: number[]) { record('arcTo', args); },
    fill(path?: unknown) { record('fill', path instanceof RecordedPath2D ? [path.ops] : []); },
    stroke(path?: unknown) { record('stroke', path instanceof RecordedPath2D ? [path.ops] : []); },
    setLineDash(dash: number[]) { record('setLineDash', [dash]); },
    drawImage(...args: unknown[]) {
      const src = args[0] as { __bpOps?: readonly CanvasOp[] } | undefined;
      record('drawImage', src?.__bpOps ? [src.__bpOps.slice(), ...args.slice(1)] : args.slice(1));
    },
    fillText(text: string, x: number, y: number) { record('fillText', [text, x, y]); },
    strokeText(text: string, x: number, y: number) { record('strokeText', [text, x, y]); },
    createLinearGradient: gradient,
    createRadialGradient: gradient,
    createPattern: () => ({}),
    measureText(text: string) {
      const px = Number.parseFloat(/\b(\d+(?:\.\d+)?)px/.exec(this.font)?.[1] ?? '12') || 12;
      return { width: text.length * px * 0.55 };
    },
  };
  const canvas = {
    width,
    height,
    __bpOps: ownOps,
    getContext: () => ctx as unknown as OffscreenCanvasRenderingContext2D,
  };
  return {
    canvas: canvas as unknown as OffscreenCanvas,
    clearOps() { opsByWidget.clear(); ownOps.length = 0; },
    snapshotOps(): CanvasOp[][] {
      return [...opsByWidget.values()].map(list => list.slice());
    },
    stagedCopies() {
      return [...ownOps, ...[...opsByWidget.values()].flat()].filter(op => op.op === 'drawImage' && Array.isArray(op.args[0]));
    },
  };
}


const KIND = 'worker-motion-test';
const TARGET_KIND = `${KIND}-target`;
const SEGMENT_KIND = `${KIND}-segments`;
const SHARE_KIND = `${KIND}-share`;
const BLUE = '#4f86f7';
const ORANGE = '#e97732';
type Sample = { value: number | null; key?: string; form?: BarsModel['form']; target?: number | true; zeroTarget?: boolean; segments?: boolean; parts?: readonly (readonly [string, number])[]; segmentsIncomplete?: boolean; scale?: number; whole?: boolean; art?: boolean; renames?: readonly number[]; france?: number; sectionAt?: number; showSectionCount?: boolean };

const testProjection = registerFamilyProjection({
  family: 'bars',
  kind: KIND,
  singleValueBars: () => true,
  project(view, options): BarsModel {
    const panel = view.panel('s') as Sample | { rows: readonly unknown[]; refused: string | null } | undefined;
    const refused = view.refused('s') ?? (panel && 'rows' in panel && !panel.rows.length ? panel.refused : null);
    if (refused) return { form: 'ordered', hour: view.hour, rows: [], empty: refused, emptyKind: 'unavailable' };
    const sample = panel as Sample | undefined;
    const nextRename = sample?.renames?.find(at => view.hour < at);
    const name = sample?.renames?.filter(at => view.hour >= at).length;
    const numeric = sample?.value == null ? null : sample.value * Number(options ?? 1);
    const value = numeric === null ? notRecorded('Missing') : knownValue(sample?.whole && numeric !== 0 ? BigInt(numeric) : numeric);
    const target = sample?.target === true ? 400 : sample?.target;
    const shortfall = target !== undefined && numeric !== null && target > numeric ? target - numeric : null;
    const rows: BarsModel['rows'][number][] = [{
      kind: 'data',
      key: sample?.key ?? 'GER',
      label: knownValue(name ? `Renamed ${name}` : 'Germany'),
      value: { value },
      numeric,
      color: BLUE,
      ...(sample?.art ? { media: { kind: 'flag', tag: 'GER',
        art: readyArt('art://motion-fixture', { frame: defaultArtFrame(20, 13) }) } } : {}),
      ...(target === undefined ? {} : {
        target: { numeric: target, value: knownValue(target), ...(shortfall === null ? {} : { shortfall: knownValue(shortfall) }) },
      }),
      ...(sample?.form === 'share' ? { readout: { primary: `${numeric} units` } } : target !== undefined || sample?.zeroTarget ? {
        readout: { primary: String(numeric), secondary: `/ ${target ?? 0} a day`, ...(shortfall === null ? {} : { shortfall: `\u2212${shortfall}` }) },
      } : {}),
      ...(sample?.segments ? { segments: [{ key: 'part', label: 'Part', share: 1, color: BLUE, value }] } : {}),
      ...(sample?.parts ? { segments: sample.parts.map(([key, share], i) => ({ key, label: key, share, color: i ? ORANGE : BLUE, value })) } : {}),
      ...(sample?.segmentsIncomplete ? { segmentsIncomplete: true } : {}),
    }];
    if (sample?.france !== undefined) {
      const france = sample.france;
      rows.push({ kind: 'data', key: 'FRA', label: knownValue('France'), value: { value: knownValue(france) }, numeric: france, color: BLUE });
      if (numeric !== null && france > numeric) rows.reverse();
    }
    if (sample?.sectionAt !== undefined) {
      rows.splice(sample.sectionAt, 0, { kind: 'section', key: 'section', label: 'Countries',
        ...(sample.showSectionCount === false ? {} : { count: rows.length - sample.sectionAt }),
      });
    }
    return {
      form: sample?.form ?? 'ordered',
      hour: view.hour,
      showAxis: sample?.form !== 'share',
      scaleMax: sample?.scale ?? 400,
      ...(nextRename !== undefined ? { nextChangeAt: nextRename } : {}),
      rows,
    };
  },
}, 'main');
registerFamilyProjection({ ...testProjection, kind: `${KIND}-mixed`, singleValueBars: () => false }, 'main');
registerFamilyProjection({ ...testProjection, kind: TARGET_KIND, singleValueBars: () => 'target-markers' }, 'main');
registerFamilyProjection({ ...testProjection, kind: SEGMENT_KIND, singleValueBars: () => 'ordered-segments' }, 'main');
registerFamilyProjection({ ...testProjection, kind: SHARE_KIND, singleValueBars: () => 'share' }, 'main');

type TestBitmap = Bitmapped & { close(): void };

async function withLane(run: (value: ReturnType<typeof fixture>) => Promise<void>, budget = Infinity) {
  const prior = Object.getOwnPropertyDescriptor(globalThis, 'OffscreenCanvas');
  Object.defineProperty(globalThis, 'OffscreenCanvas', {
    value: class {
      width: number;
      height: number;
      readonly getContext: () => OffscreenCanvasRenderingContext2D | null;
      readonly transferToImageBitmap: () => ImageBitmap;
      constructor(w: number, h: number) {
        this.width = w;
        this.height = h;
        const { canvas, snapshotOps, clearOps } = makeRecordingCanvas(w, h);
        this.getContext = () => canvas.getContext('2d');
        this.transferToImageBitmap = () => {
          const bitmap = { width: this.width, height: this.height, __bpOps: snapshotOps().flat(), close() {} };
          clearOps();
          return bitmap as ImageBitmap;
        };
      }
    },
    configurable: true,
  });
  try { await run(fixture(budget)); }
  finally {
    if (prior) Object.defineProperty(globalThis, 'OffscreenCanvas', prior);
    else Reflect.deleteProperty(globalThis, 'OffscreenCanvas');
  }
}

function fixture(budget: number) {
  const later: (() => void)[] = [];
  const presented: Presented<TestBitmap>[] = [];
  const served: Parameters<NonNullable<LaneHost<TestBitmap>['served']>>[0][] = [];
  const hits: (string | null)[] = [];
  const hoverUpdates: number[] = [];
  const released: number[] = [];
  const sampled: number[] = [];
  const painted: number[] = [];
  let selectionCost = 0;
  let selections = 0;
  let ready = true;
  let paintCost = 0;
  let siblingCost = 0;
  let now = 1000;
  let settleArtwork!: (bitmap: ImageBitmap) => void;
  const artwork = new Promise<ImageBitmap>(resolve => { settleArtwork = resolve; });
  const view = createLaneView(barsLane, widget => lane.touch(widget), { fetchBitmap: () => artwork });
  const workerView = {
    ...view,
    paint(...args: Parameters<typeof view.paint>) {
      if (!ready) return { surfaces: [], fields: {}, motion: false, waiting: true as const };
      painted.push(args[0].widget);
      const result = view.paint(...args);
      const playback = result.playback;
      return playback ? { ...result, playback: {
        ...playback,
        paint(...visual: Parameters<typeof playback.paint>) {
          now += paintCost;
          sampled.push(args[0].widget);
          return playback.paint(...visual);
        },
      } } : result;
    },
  };
  const lane = new Lane<TestBitmap>({
    lane: 0, lanes: 1,
    views: kind => kind === 'bars' ? workerView : { paint: input => {
      now += siblingCost;
      painted.push(input.widget);
      return { surfaces: [], fields: {}, motion: false };
    } },
     crop: async () => [], release: bytes => released.push(bytes),
    present: reply => presented.push(reply), served: reply => served.push(reply),
    hit: (_id, key) => hits.push(key), playbackHover: id => hoverUpdates.push(id), fail: assert.fail, later: fn => later.push(fn),
    clock: () => now, budget,
  });
  const window = lane.facts.window.bind(lane.facts);
  lane.facts.window = (...args) => {
    now += selectionCost;
    selections++;
    return window(...args);
  };
  const canvas = makeRecordingCanvas(480, 240);
  lane.setLaneSurface(canvas.canvas, true);
  lane.follow(1, 'panel', 'p');
  lane.bindRoute(1, 1, 9, 1, 0);
  const bind = (measure = 1, id = 1, reducedMotion = false, mixed: boolean | 'target' | 'segments' | 'share' = false, pin: number | null = null) => {
    lane.payloads({ type: 'payloads', put: [[1, { s: 1 }], [10 + measure, measure]] });
    lane.bind(id, {
      generation: measure, owner: 'track', surfaces: [{ width: 480, height: 240, dpr: 1 }], offset: 0, pin,
    }, {
      family: 'bars', kind: mixed === 'target' ? TARGET_KIND : mixed === 'segments' ? SEGMENT_KIND : mixed === 'share' ? SHARE_KIND : mixed ? `${KIND}-mixed` : KIND, view: 'main', options: measure, sources: { s: 1 }, scrollTop: 0,
      width: 480, height: 240, theme: TEST_THEME, audit: true, reducedMotion,
      revisions: { options: 10 + measure, sources: 1 },
    } satisfies FamilyBarsBinding);
  };
  const mount = (count: number, sibling = false) => {
    const slots = Array.from({ length: count }, (_, i) => {
      const widget = i + 1;
      lane.mount(widget, [1], sibling && widget === count ? 'other' : 'bars', {});
      bind(1, widget);
      return { widget, x: 480 * i, y: 0, w: 480, h: 240, visible: true };
    });
    lane.setLaneSlots({
      width: 480 * count, height: 240, dpr: 1, scrollTop: 0, scrollLeft: 0, surfaceColor: TEST_THEME.surface,
      cornerRadius: 8, clipPathFallback: false, slots,
    });
  };
  mount(1);
  const record = (hour: number, value?: Sample, seek = 1) => lane.receive('panels', datedPanel(1, hour, value, seek, 9));
  let serial = 0;
  const drain = async () => { for (let i = 0; i < 16; i++) await Promise.resolve(); };
  const request = async (hour: number, speed = 0, seek = 1, acknowledge: boolean | Ack[] = false, exact = speed === 0, target = hour, refused: Refusal[] = []) => {
    const installed = Array.isArray(acknowledge) ? acknowledge
      : acknowledge ? (presented.at(-1)?.entries ?? []).map(({ widget, binding, id }) => ({ widget, binding, id })) : [];
    lane.frame({ type: 'frame', serial: ++serial, seek, now: hour, target, exact, speed, installed, refused, inspect: [[1, 'row:GER']] });
    await drain();
  };
  const clock = (anchorPosition: number, speed: number, seek = 1, diagnostics = false) => lane.setClock(baseClock({
    anchorPosition, exactHour: Math.floor(anchorPosition), anchorTimeMs: performance.timeOrigin + now,
    speed, playing: speed > 0, seek,
    ...(diagnostics ? { diagnostics: true } : {}),
  }));
  const tick = (elapsed: number) => { now += elapsed; later.splice(0).forEach(run => run()); };
  const spendOnFactSelection = (cost: number) => {
    const select = lane.facts.select.bind(lane.facts);
    lane.facts.select = (...args) => {
      now += cost;
      return select(...args);
    };
  };
  const operations = () => canvas.snapshotOps().flat();
  const bar = () => {
    const paint = operations().reverse().find(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE);
    assert.ok(paint, 'expected a painted bar');
    return paint;
  };
  const barWidth = () => Number(bar().args[2]);
  return {
    lane, canvas, record, request, clock, tick, operations, bar, barWidth, presented, served, hits, hoverUpdates, released, bind, mount, sampled, painted, drain, spendOnFactSelection,
    selectionCost: (value: number) => { selectionCost = value; selections = 0; },
    selections: () => selections,
    finishArtwork: () => settleArtwork({ width: 20, height: 13, close() {} } as ImageBitmap),
    ready: (value: boolean) => { ready = value; },
    paintCost: (value: number) => { paintCost = value; },
    siblingCost: (value: number) => { siblingCost = value; },
  };
}

function topRowPoint(nav: unknown): [number, number] {
  const entry = (nav as readonly [string, string, number, number, string, unknown, number, number][])[0]!;
  return [32, entry[6] + entry[7] / 2];
}

describe('Ordered bar playback', () => {
  it('holds the recorded picture when factory playback outruns the delivered proof', async () => withLane(async f => {
    const base = 60788000;
    f.record(base + 28, { value: 30 });
    f.record(base + 50);
    f.clock(base + 47.008, 1440, 1, true);
    await f.request(base + 47, 1440, 1, false, true);
    const unit = f.barWidth() / 30;
    let recorded = 30;
    for (const sample of [
      { position: 61.984, target: 75, expected: 30 },
      { position: 77.248, target: 93, expected: 30 },
      { position: 92.224, target: 107, delivered: [77, 98, 60], expected: 60 },
      { position: 107.488, target: 122, expected: 60 },
      { position: 122.752, target: 137, expected: 60 },
      { position: 137.872, target: 152, delivered: [115, 122, 60], expected: 60 },
      { position: 152.992, target: 167, delivered: [123, 146, 100], expected: 100 },
    ] as const) {
      if (sample.delivered) {
        const [first, through, value] = sample.delivered;
        f.record(base + first, { value });
        f.record(base + through);
      }
      const hour = base + Math.floor(sample.position);
      f.clock(base + sample.position, 1440, 1, true);
      await f.request(hour, 1440, 1, true, false, base + sample.target);
      await f.request(hour, 1440, 1, true, false, base + sample.target);
      assert.ok(Math.abs(f.barWidth() / unit - recorded) < .01, 'the page leaves playing pixels to the tick');
      f.tick(0);
      assert.ok(Math.abs(f.barWidth() / unit - sample.expected) < .01);
      recorded = sample.expected;
    }
    const trace = f.served.filter(frame => frame.motionDiagnostics);
    assert.equal(trace.length, 7);
    assert.equal(trace.reduce((count, frame) => count + frame.changed, 0), 0);
    assert.equal(trace.reduce((count, frame) => count + (frame.motionDiagnostics?.skipped.unproven ?? 0), 0), 6);
  }));

  it('does not restore pixels after a refusal of a metadata-only entry', async () => withLane(async f => {
    for (const [hour, value] of [[100, 30], [101, 60], [102, 100]] as const) f.record(hour, { value });
    f.clock(100, 1);
    await f.request(100, 1);
    const exact = f.barWidth();
    await f.request(100, 1, 1, true);
    f.clock(101.5, 1);
    f.tick(0);
    await f.request(101, 1);
    const metadata = f.presented.at(-1)!.entries[0]!;
    assert.ok(!metadata.repainted);
    await f.request(101, 1, 1, false, false, 101, [{ widget: 1, id: metadata.id, reason: 'superseded' }]);
    assert.ok(Math.abs(f.barWidth() / exact - 80 / 30) < .01);
  }));

  it('paints exact values at recorded hours, including the last endpoint, without waiting for the page', async () => withLane(async f => {
    for (const [hour, value] of [[100, 30], [101, 60], [102, 100]] as const) f.record(hour, { value });
    f.clock(100, 1);
    await f.request(100, 1);
    const unit = f.barWidth() / 30;
    for (const [elapsed, expected, label] of [[500, 45, '30'], [500, 60, '60'], [500, 80, '60'], [500, 100, '100']] as const) {
      f.canvas.clearOps();
      f.tick(elapsed);
      assert.ok(Math.abs(f.barWidth() / unit - expected) < .01);
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === label));
    }
  }));

  it('paints a covered pause after an older eligible stage is acknowledged over a direct picture', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(123);
    f.record(124, { value: 60 });
    f.clock(100, 1);
    await f.request(100, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)!.entries[0]!;
    assert.equal(staged.lo, 124);
    await f.request(100, 1, 1, false, true);
    const exact = f.barWidth();
    f.clock(124, 0);
    await f.request(124, 0, 1, [staged]);
    assert.ok(Math.abs(f.barWidth() / exact - 2) < .01);
  }));

  it('resumes a matching renderer from a picture accepted while paused', async () => withLane(async f => {
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100, 1);
    await f.request(101, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)!.entries[0]!;
    assert.equal(staged.lo, 124);
    f.clock(124, 0);
    await f.request(124, 0, 1, [staged]);
    const unit = f.barWidth() / 60;
    f.clock(136, 1);
    await f.request(136, 1, 1, true);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / unit - 80) < .01);
  }));

  it('rebuilds the exact paused picture and renderer for direct, staged, extended and never-owned slots', async () => {
    let exactWidth: number | undefined;
    for (const history of ['direct', 'staged', 'extended', 'never-owned']) await withLane(async f => {
      f.record(100, { value: 30 });
      f.record(123);
      f.record(124, { value: 60 });
      f.record(147);
      f.record(148, { value: 100 });
      if (history === 'extended') f.lane.supply(1, { dated: { h: 100, c: 148 } });
      f.clock(100, 1);
      if (history === 'direct') await f.request(124, 1);
      else await f.request(100, 1, 1, false, false, 124);
      const ready = f.presented.at(-1)!.entries[0]!;
      assert.equal(ready.lo, 124);
      if (history === 'extended') await f.request(100, 1, 1, false, true);
      if (history === 'extended') f.lane.supply(1, { dated: { h: 100, c: 149 } });
      f.canvas.clearOps();
      f.clock(124, 0);
      await f.request(124, 0, 1, history === 'never-owned' ? [] : [ready]);
      const widths = f.operations()
        .filter(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE)
        .map(op => Number(op.args[2]));
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'), history);
      assert.equal(widths.length, 1, history);
      exactWidth ??= widths[0]!;
      assert.equal(widths[0], exactWidth, history);
      f.clock(136, 1);
      await f.request(136, 1, 1, true);
      f.tick(0);
      assert.ok(Math.abs(f.barWidth() / exactWidth - 80 / 60) < .01, history);
    });
  });

  it('settles to the proven endpoint when the clock skips beyond it before any acknowledgement', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(101, { value: 60 });
    f.clock(100, 1);
    await f.request(100, 1);
    const unit = f.barWidth() / 30;
    await f.request(100, 1, 1, true);
    f.tick(500);
    assert.ok(Math.abs(f.barWidth() / unit - 45) < .01);
    f.tick(1500);
    assert.ok(Math.abs(f.barWidth() / unit - 60) < .01);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
  }));

  it('animates the accepted interval even while a later staged picture awaits acknowledgement', async () => withLane(async f => {
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100, 1);
    await f.request(101, 1, 1, false, false, 124);
    const first = f.presented.at(-1)!.entries[0]!;
    await f.request(102, 1, 1, false, false, 148);
    const later = f.presented.at(-1)!.entries[0]!;
    assert.deepEqual([first.lo, later.lo], [124, 148]);
    f.clock(124, 1);
    await f.request(124, 1, 1, [first], false, 148);
    f.tick(0);
    const unit = f.barWidth() / 60;
    f.clock(136, 1);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / unit - 80) < .01);
    await f.request(148, 1, 1, [later], false, 148);
    assert.ok(Math.abs(f.barWidth() / unit - 80) < .01);
    assert.equal(f.canvas.stagedCopies().length, 0);
  }));

  it('starts the bracket at the last model boundary regardless of which ticks ran', async () => {
    for (const prior of [[], [105.5, 111.5]]) await withLane(async f => {
      f.record(100, { value: 30, renames: [105, 110] });
      f.record(123);
      f.record(124, { value: 100, renames: [105, 110] });
      f.clock(100, 1);
      await f.request(100, 1);
      const unit = f.barWidth() / 30;
      for (const position of [...prior, 117]) {
        f.clock(position, 1);
        f.tick(0);
      }
      assert.ok(Math.abs(f.barWidth() / unit - 65) < .01);
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === 'Renamed 2'));
    });
  });

  it('lets a reduced-motion binding resume shared look-ahead on its first request', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(123);
    f.record(124, { value: 60 });
    f.clock(100, 1);
    await f.request(100, 1);
    await f.request(100, 1, 1, true);
    f.tick(500);
    f.bind(2, 1, true);
    await f.request(101, 1, 1, false, false, 124);
    assert.equal(f.presented.at(-1)?.entries[0]?.lo, 124);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
  }));

  it('publishes the visible hour when a supply update restores playback ownership during look-ahead', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(101, { value: 60 });
    f.record(102, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1);
    const unit = f.barWidth() / 30;
    await f.request(100, 1, 1, true);
    f.lane.supply(1, { statics: {} });
    f.clock(101.5, 1);
    await f.request(101, 1, 1, false, false, 102);
    const entry = f.presented.at(-1)?.entries[0];
    assert.equal(entry?.lo, 101);
    assert.equal(entry?.repainted, undefined);
    assert.ok(Math.abs(f.barWidth() / unit - 60) < .01);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / unit - 80) < .01);
  }));

  it('reacquires pixels at the live hour and publishes metadata for the delayed page request', async () => {
    for (const [liveSeek, liveSpeed] of [[1, 1], [1, 0], [0, 1]] as const) await withLane(async f => {
      f.record(100, { value: 30 });
      f.record(101, { value: 60 });
      f.clock(100, 1);
      await f.request(100, 1);
      const unit = f.barWidth() / 30;
      const previous = f.presented.at(-1)!.entries[0]!;
      await f.request(100, 1, 1, true);
      f.clock(101, 1);
      f.tick(0);
      assert.ok(Math.abs(f.barWidth() / unit - 60) < .01);

      f.lane.supply(1, { statics: {} });
      f.clock(101, liveSpeed, liveSeek);
      f.canvas.clearOps();
      await f.request(100, 1);
      const entry = f.presented.at(-1)!.entries[0]!;
      assert.ok(entry.id > previous.id, 'the delayed request publishes a new recorded entry');
      assert.equal(entry.lo, 100);
      assert.match(JSON.stringify(entry.fields.audit === SAME ? previous.fields.audit : entry.fields.audit), /30/);
      const drawn = liveSeek === 1 ? 60 : 30;
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === String(drawn)), `clock seek ${liveSeek}`);
      assert.ok(Math.abs(f.barWidth() / unit - drawn) < .01);
    });
  });

  it('hands a staged slot to playback at the accepted hour', async () => withLane(async f => {
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100.5, 1);
    f.canvas.clearOps();
    await f.request(101, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)!.entries[0]!;
    assert.equal(staged.lo, 124);
    f.tick(0);
    assert.deepEqual(f.sampled, []);
    assert.equal(f.canvas.stagedCopies().length, 0);
    const point = topRowPoint(staged.fields.nav);
    f.lane.pointer(1, 0, point, staged.id);
    assert.deepEqual(f.hits, [null]);
    f.clock(124, 1);
    await f.request(124, 1, 1, [staged], false, 148);
    assert.equal(f.canvas.stagedCopies().length, 0);
    f.lane.pointer(1, 0, point, staged.id);
    assert.deepEqual(f.hits, [null, null], 'the accepted metadata has no visible row until the worker paints it');
    f.tick(0);
    assert.equal(f.sampled.length, 1);
    f.lane.pointer(1, 0, point, staged.id);
    assert.deepEqual(f.hits, [null, null, 'row:GER']);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
    const exact = f.barWidth();
    f.clock(136, 1);
    f.tick(0);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
    assert.ok(Math.abs(f.barWidth() / exact - 80 / 60) < .01);
    f.clock(148, 0);
    await f.request(148);
    assert.ok(Math.abs(f.barWidth() / exact - 100 / 60) < .01);
    assert.equal(f.canvas.stagedCopies().length, 0);
  }));

  it('does not copy a delayed staged acknowledgement over a newer live bar', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(123);
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100.5, 1);
    await f.request(100, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)?.entries[0];
    assert.equal(staged?.lo, 124);
    await f.request(100, 1, 1, false, true);
    f.clock(136, 1);
    f.tick(0);
    const width = f.barWidth();
    await f.request(124, 1, 1, [staged!], false, 148);
    assert.equal(f.canvas.stagedCopies().length, 0);
    assert.equal(f.barWidth(), width);
  }));

  it('draws the accepted first picture on the tick even when the shared slot was dirty', async () => withLane(async f => {
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)?.entries[0];
    assert.equal(staged?.lo, 124);
    f.clock(124, 1);
    await f.request(124, 1, 1, [staged!], false, 148);
    f.tick(0);
    const exact = f.barWidth();
    assert.equal(f.canvas.stagedCopies().length, 0);
    f.tick(12000);
    assert.ok(Math.abs(f.barWidth() / exact - 80 / 60) < .01);
  }));

  it('uses the visible hour for an active bar even when the page requests a different future picture', async () => withLane(async f => {
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.record(103, { value: 400, key: 'ENG' });
    f.clock(100.5, 1);
    await f.request(100, 1, 1, false, true);
    const exact = f.barWidth();
    f.lane.touch(1);
    await f.request(100, 1, 1, true, false, 103);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() - exact * 1.5) < 1);
    assert.equal(f.presented.at(-1)?.entries[0]?.lo, 100);
    f.clock(101.5, 0);
    await f.request(101);
    assert.ok(Math.abs(f.barWidth() - exact * 2) < 1);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '200'));
  }));

  it('uses a cached confirmed bracket at maximum speed and immediately paints the paused hour', async () => withLane(async f => {
    f.record(100, { value: 100 });
    f.record(108);
    f.record(109, { value: 100 });
    f.record(110, { value: 200 });
    await f.request(100);
    const start = f.barWidth();
    f.clock(100, 1440);
    f.tick(4);
    assert.equal(f.barWidth(), start);
    f.tick(9.5 / 1.44 - 4);
    assert.ok(f.barWidth() > start && f.barWidth() < start * 2);
    f.clock(109.5, 0);
    f.canvas.clearOps();
    await f.request(109);
    assert.equal(f.barWidth(), start);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '100'));
    f.canvas.clearOps();
    f.tick(16);
    assert.equal(f.operations().length, 0);
  }));

  it('repaints every maximum-speed tick between changing confirmed samples across native step intervals', async () => withLane(async f => {
    for (let at = 0; at <= 6; at++) {
      const hour = 100 + 24 * at;
      f.record(hour, { value: 40 + 40 * at });
      f.record(hour + 23);
    }
    f.clock(100, 1440, 1, true);
    await f.request(100, 1440);
    const presents = f.presented.length;
    const unitWidth = f.barWidth() / 40;
    let previous = f.barWidth();
    for (let tick = 0; tick < 8; tick++) {
      f.canvas.clearOps();
      f.tick(1000 / 95);
      const width = f.barWidth();
      const elapsedHours = (tick + 1) * 1440 / 95;
      const expected = 40 + elapsedHours * 40 / 24;
      const recorded = 40 + Math.floor(elapsedHours / 24) * 40;
      assert.ok(width > previous, `tick ${tick + 1} advances the visible bar`);
      assert.ok(Math.abs(width / unitWidth - expected) < .01);
      const rowLabels = f.operations().filter(op => op.op === 'fillText' && Math.abs(Number(op.args[2]) - Number(f.bar().args[1])) < 20);
      assert.ok(rowLabels.some(op => op.args[0] === String(recorded)));
      assert.ok(!rowLabels.some(op => op.args[0] === String(Math.round(expected))));
      previous = width;
    }
    assert.equal(f.served.filter(frame => frame.motionDiagnostics).reduce((count, frame) => count + frame.changed, 0), 8);
    assert.equal(f.presented.length, presents);
    f.clock(244, 1440, 1, true);
    await f.request(244, 1440);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / unitWidth - 280) < .01);
    f.clock(244, 0, 1, true);
    await f.request(244);
    f.tick(1000 / 95);
    assert.equal(f.served.filter(frame => frame.motionDiagnostics).reduce((count, frame) => count + frame.changed, 0), 8);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '280'));
  }));

  it('does not interpolate a long confirmed value toward a reading beyond an unproven hour', async () => withLane(async f => {
    f.record(100, { value: 60 });
    f.record(123);
    f.record(126, { value: 100 });
    f.clock(100, 1440, 1, true);
    await f.request(100, 1440);
    const exact = f.barWidth();
    f.tick(1000 / 95);
    assert.equal(f.barWidth(), exact);
    assert.ok(f.served.some(frame => frame.motionDiagnostics?.skipped.noBracket));
    assert.ok(!f.served.some(frame => frame.motionDiagnostics && frame.changed));
    f.clock(126, 1440);
    await f.request(126, 1440);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / exact - 100 / 60) < .01);
  }));

  it('draws exact missing readings and row boundaries without interpolating across either', async () => withLane(async f => {
    f.record(100, { value: 100 });
    f.record(101, { value: null });
    f.record(102, { value: 200 });
    f.record(103, { value: 400, key: 'ENG' });
    f.clock(100.5, 168, 1, true);
    await f.request(100, 168);
    const start = f.barWidth();
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.barWidth(), start);
    f.clock(101.5, 168, 1, true);
    f.tick(0);
    assert.ok(f.operations().some(op => op.op === 'fillText' && String(op.args[0]).includes('recorded')));
    f.clock(102.5, 168, 1, true);
    f.tick(0);
    assert.equal(f.barWidth(), start * 2);
    assert.deepEqual(f.served.at(-1)?.motionDiagnostics?.rowChanges, { membership: 1 });
    f.canvas.clearOps();
    f.clock(103.5, 168, 1, true);
    f.tick(0);
    assert.equal(f.barWidth(), start * 4);
    assert.ok(!f.served.some(frame => frame.motionDiagnostics && frame.changed));
  }));

  it('interpolates each matched key under an unchanged section until the ranks swap at the boundary', async () => withLane(async f => {
    f.record(100, { value: 100, france: 50, sectionAt: 0 });
    f.record(101, { value: 80, france: 120, sectionAt: 0 });
    f.clock(100, 1, 1, true);
    await f.request(100, 1);
    const unit = f.barWidth() / 50;
    await f.request(100, 1, 1, true);
    const bars = () => f.operations().filter(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE)
      .map(op => Number(op.args[2]) / unit);
    for (const [position, widths, values] of [
      [100.75, [85, 102.5], ['100', '50']],
      [101, [120, 80], ['120', '80']],
    ] as const) {
      f.canvas.clearOps();
      f.clock(position, 1, 1, true);
      f.tick(0);
      assert.equal(bars().length, 2);
      bars().forEach((width, at) => assert.ok(Math.abs(width - widths[at]!) < .01, `hour ${position}, row ${at}, width ${width}`));
      const labels = f.operations().filter(op => op.op === 'fillText');
      const rowValues = labels.filter(op => op.font === `500 ${TEST_THEME.fontSize.body}px ${TEST_THEME.font}`).map(op => op.args[0]);
      assert.deepEqual(rowValues, values);
      assert.deepEqual(labels.map(op => op.args[0]).filter(label => label === 'Germany' || label === 'France'), position < 101 ? ['Germany', 'France'] : ['France', 'Germany']);
      if (position < 101) assert.deepEqual(f.served.at(-1)?.motionDiagnostics?.rowChanges, { reorder: 1 });
    }
  }));

  it('cuts when rows cross a section boundary or a key is replaced by a duplicate', async () => {
    for (const [first, second, rowChanges] of [
      [{ value: 100, france: 50, sectionAt: 1 }, { value: 80, france: 120, sectionAt: 1 }, undefined],
      [{ value: 100, france: 50, sectionAt: 0 }, { value: 80, france: 120, sectionAt: 1 }, undefined],
      [{ value: 100, france: 50 }, { value: 80, france: 120, key: 'FRA' }, { membership: 1 }],
      [{ value: 50, france: 100, sectionAt: 1 }, { value: 80, france: 120, sectionAt: 1, key: 'FRA' }, { membership: 1 }],
    ] as const) await withLane(async f => {
      f.record(100, first);
      f.record(101, second);
      f.clock(100.75, 1, 1, true);
      await f.request(100, 1);
      f.tick(0);
      assert.equal(f.served.at(-1)?.changed, 0);
      assert.deepEqual(f.served.at(-1)?.motionDiagnostics?.rowChanges, rowChanges);
    });
  });

  it('allows a section to gain a count during same-order motion', async () => {
    for (const [france, painted] of [[60, 55], [120, 50]] as const) await withLane(async f => {
      f.record(100, { value: 100, france: 50, sectionAt: 0, showSectionCount: false });
      f.record(101, { value: 80, france, sectionAt: 0 });
      f.clock(100, 1);
      await f.request(100, 1);
      const unit = f.barWidth() / 50;
      f.clock(100.5, 1);
      f.tick(0);
      assert.ok(Math.abs(f.barWidth() / unit - painted) < .01);
    });
  });

  it('records a label change separately and cuts even when the same keys reorder', async () => withLane(async f => {
    f.record(100, { value: 100, france: 50 });
    f.record(101, { value: 80, france: 120, renames: [101] });
    f.clock(100.75, 1, 1, true);
    await f.request(100, 1);
    const start = f.barWidth();
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.barWidth(), start);
    assert.deepEqual(f.served.at(-1)?.motionDiagnostics?.rowChanges, { label: 1 });
    assert.equal(f.served.at(-1)?.changed, 0);
  }));

  it('withholds the installed row hit while the canvas shows a newly proven order', async () => withLane(async f => {
    f.record(100, { value: 100, france: 50 });
    f.record(101, { value: 100, france: 200 });
    f.clock(100, 1);
    await f.request(100, 1);
    const first = f.presented.at(-1)!.entries[0]!;
    await f.request(100, 1, 1, true);
    f.tick(0);
    const point = topRowPoint(first.fields.nav);
    f.lane.pointer(1, 0, point, first.id);
    assert.deepEqual(f.hits, ['row:GER']);

    f.clock(101, 1);
    f.canvas.clearOps();
    f.tick(0);
    const labels = f.operations().filter(op => op.op === 'fillText' && ['Germany', 'France'].includes(String(op.args[0])));
    assert.deepEqual(labels.map(op => op.args[0]), ['France', 'Germany']);
    f.lane.pointer(1, 0, point, first.id);
    assert.deepEqual(f.hits, ['row:GER', null]);

    await f.request(101, 1);
    const second = f.presented.at(-1)!.entries[0]!;
    await f.request(101, 1, 1, true);
    f.lane.pointer(1, 0, point, second.id);
    assert.deepEqual(f.hits, ['row:GER', null, 'row:FRA']);
  }));

  it('refreshes stationary hover when the exact pause picture installs', async () => withLane(async f => {
    f.record(100, { value: 100, france: 50 });
    f.record(101, { value: 100, france: 200 });
    f.clock(100, 1);
    await f.request(100, 1);
    const first = f.presented.at(-1)!.entries[0]!;
    await f.request(100, 1, 1, true);
    f.tick(0);
    const point = topRowPoint(first.fields.nav);
    f.lane.pointer(1, 0, point, first.id, true);
    assert.deepEqual(f.hits, ['row:GER']);
    f.clock(101.5, 1);
    f.tick(0);
    assert.deepEqual(f.hoverUpdates, [1], 'the changed row invalidates the old tooltip');
    f.lane.pointer(1, 0, point, first.id, true);
    assert.deepEqual(f.hits, ['row:GER', null]);

    f.clock(101, 0);
    await f.request(101);
    const paused = f.presented.at(-1)!.entries[0]!;
    assert.equal(paused.lo, 101);
    assert.deepEqual(f.hoverUpdates, [1]);
    await f.request(101, 0, 1, [paused]);
    assert.deepEqual(f.hoverUpdates, [1, 1], 'the paused installation rechecks the same point');
    f.lane.pointer(1, 0, point, paused.id, true);
    assert.equal(f.hits.at(-1), 'row:FRA');
  }));

  it('does not invalidate a stationary hover against an older acknowledgement', async () => withLane(async f => {
    f.record(100, { value: 100, france: 50 });
    f.record(101, { value: 100, france: 200 });
    f.clock(100, 1);
    await f.request(100, 1);
    await f.request(100, 1, 1, true);
    f.clock(101, 1);
    f.tick(0);
    await f.request(101, 1);
    const newer = f.presented.at(-1)!.entries[0]!;
    assert.equal(newer.lo, 101);
    const point = topRowPoint(newer.fields.nav);
    f.lane.pointer(1, 0, point, newer.id, true);
    assert.equal(f.hits.at(-1), 'row:FRA');
    for (let i = 0; i < 4; i++) f.tick(10);
    assert.deepEqual(f.hoverUpdates, [], 'the page-named installation stays authoritative while its acknowledgement is deferred');
    await f.request(101, 1, 1, [newer]);
    assert.deepEqual(f.hoverUpdates, []);

    f.lane.mount(1, [1], 'bars');
    assert.deepEqual(f.hoverUpdates, [1], 'remounting invalidates the tracked hover before the new binding paints');
    f.bind();
    await f.request(101, 1);
    assert.deepEqual(f.hoverUpdates, [1, 1], 'the tracked point survives the remount and can match the new paint');
  }));

  it('does not paint or publish newly segmented rows between page requests', async () => withLane(async f => {
    f.bind(1, 1, false, true);
    f.record(100, { value: 100 });
    f.record(101, { value: 200, segments: true });
    f.record(102, { value: 300, segments: true });
    f.clock(100.5, 1);
    await f.request(100, 1);
    const before = f.presented.length;
    f.canvas.clearOps();
    f.clock(101.5, 1);
    f.tick(0);
    await f.drain();
    assert.equal(f.operations().length, 0);
    assert.equal(f.presented.length, before);
    await f.request(101, 1);
    assert.equal(f.presented.at(-1)?.entries[0]?.lo, 101);
  }));

  it('keeps a mixed-form binding on the page path regardless of intervening ticks', async () => {
    for (const visitSegments of [false, true]) await withLane(async f => {
      f.bind(1, 1, false, true);
      f.record(100, { value: 100 });
      f.record(101, { value: 200, segments: true });
      f.record(102, { value: 300 });
      f.record(103, { value: 400 });
      f.clock(100, 1);
      await f.request(100, 1);
      const width = f.barWidth();
      if (visitSegments) {
        f.clock(101.5, 1);
        f.tick(0);
      }
      f.clock(102.5, 1);
      f.tick(0);
      assert.equal(f.barWidth(), width);
      assert.deepEqual(f.sampled, []);
      await f.request(101, 1, 1, true);
      assert.equal(f.presented.at(-1)?.entries[0]?.lo, 101);
    });
  });

  it('reports an invalid single-value declaration and keeps that binding on the ordinary path', async t => withLane(async f => {
    const errors = t.mock.method(console, 'error', () => {});
    f.record(100, { value: 100, target: true });
    f.record(101, { value: 200 });
    f.record(102, { value: 300 });
    f.clock(100, 1);
    await f.request(100, 1);
    assert.equal(errors.mock.callCount(), 1);
    assert.match(String(errors.mock.calls[0]?.arguments[0]), /single-value guarantee/);
    f.clock(101.5, 1);
    await f.request(101, 1, 1, true);
    const width = f.barWidth();
    f.tick(0);
    assert.equal(f.barWidth(), width);
    assert.deepEqual(f.sampled, []);
  }));

  it('releases a binding once an adjacent endpoint contradicts its single-value guarantee', async t => withLane(async f => {
    const errors = t.mock.method(console, 'error', () => {});
    f.record(100, { value: 100 });
    f.record(101, { value: 200, segments: true });
    f.record(102, { value: 300 });
    f.record(103, { value: 400 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    f.tick(0);
    assert.equal(errors.mock.callCount(), 1);
    assert.match(String(errors.mock.calls[0]?.arguments[0]), /single-value guarantee/);

    f.clock(102.5, 1);
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.operations().length, 0);
    await f.request(102, 1, 1, true);
    assert.equal(f.presented.at(-1)?.entries[0]?.repainted, true);
    f.tick(0);
    assert.deepEqual(f.sampled, [1]);
    assert.equal(errors.mock.callCount(), 1);
  }));

  it('releases target-marker playback when a projected endpoint has segments', async t => withLane(async f => {
    const errors = t.mock.method(console, 'error', () => {});
    f.bind(1, 1, false, 'target');
    f.record(100, { value: 40, target: 100 });
    f.record(101, { value: 60, target: 120, segments: true });
    f.record(102, { value: 80, target: 140 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    f.tick(0);
    assert.equal(errors.mock.callCount(), 1);
    assert.match(String(errors.mock.calls[0]?.arguments[0]), /target-marker guarantee/);
    f.clock(102, 1);
    await f.request(102, 1);
    assert.ok(f.presented.at(-1)?.entries[0]?.repainted);
    f.tick(0);
    assert.deepEqual(f.sampled, [1]);
  }));

  it('retains ordinary pause priority for a target-marked bar', async () => withLane(async f => {
    f.mount(2, true);
    f.bind(1, 1, false, true);
    f.record(100, { value: 100, target: true });
    f.record(101, { value: 200, target: true });
    f.clock(100, 1);
    await f.request(100, 1);
    f.painted.length = 0;
    f.clock(101, 0);
    await f.request(101, 0, 1, true);
    assert.equal(f.painted[0], 1);
  }));

  it('builds a renderer when an uncovered pause restores an accepted stage', async () => withLane(async f => {
    f.lane.supply(1, { dated: { h: 124, c: 124 } });
    f.record(124, { value: 60 });
    f.record(147);
    f.record(148, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)?.entries[0];
    assert.equal(staged?.lo, 124);
    f.clock(125, 0);
    await f.request(125, 0, 1, [staged!]);
    const exact = f.barWidth();
    f.lane.supply(1, { dated: { h: 124, c: 148 } });
    f.clock(136, 1);
    await f.request(136, 1, 1, true);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / exact - 80 / 60) < .01);
  }));

  it('keeps an uncovered pause pending until the accepted picture can actually draw', async () => withLane(async f => {
    f.record(124, { value: 60 });
    f.clock(100, 1);
    await f.request(100, 1, 1, false, false, 124);
    const staged = f.presented.at(-1)?.entries[0];
    assert.equal(staged?.lo, 124);
    f.canvas.clearOps();
    f.ready(false);
    f.clock(125, 0);
    await f.request(125, 0, 1, [staged!]);
    assert.ok(!f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
    f.ready(true);
    await f.request(125);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '60'));
    f.canvas.clearOps();
    await f.request(125);
    assert.equal(f.operations().length, 0);
  }));

  it('keeps the recorded labels, tooltip, mirror and audit through ordinary presentation', async () => withLane(async f => {
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100.5, 1, 1, true);
    await f.request(100, 1);
    const first = f.presented.flatMap(reply => reply.entries).at(-1);
    assert.ok(first?.repainted, 'the ordinary page request paints the direct slot');
    assert.ok(first?.fields);
    const { mirror, tooltip, audit } = first.fields;
    const exact = JSON.stringify({ mirror, tooltip, audit });
    assert.match(exact, /100/);
    assert.match(exact, /mirror/);
    assert.match(exact, /tooltip/);
    assert.match(exact, /audit/);
    assert.doesNotMatch(exact, /150/);
    const credit = [...f.released];
    f.tick(0);
    assert.deepEqual(f.released, credit);
    assert.ok(f.served.some(frame => frame.motionDiagnostics && frame.changed === 1));
    const latest = f.presented.flatMap(reply => reply.entries).at(-1);
    assert.equal(latest, first);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '100'));
  }));

  it('leaves target bars without a playback guarantee on the exact page path', async () => withLane(async f => {
    f.bind(1, 1, false, true);
    f.record(100, { value: 100, target: true });
    f.record(101, { value: 200, target: true });
    f.clock(100.5, 1);
    await f.request(100, 1);
    assert.ok(f.presented.flatMap(reply => reply.entries).at(-1)?.repainted);
    const width = f.barWidth();
    f.tick(0);
    assert.equal(f.barWidth(), width);
  }));

  it('moves delivered and target positions while keeping the recorded fuel readout', async () => {
    for (const nextValue of [40, 60]) await withLane(async f => {
      f.bind(1, 1, false, 'target');
      f.record(100, { value: 40, target: 100 });
      f.record(101, { value: nextValue, target: 180 });
      await f.request(100);
      const targetWidth = () => {
        const gap = f.operations().find(op => op.op === 'roundRect' && op.globalAlpha === 0.16);
        assert.ok(gap, 'expected a painted target gap');
        return Number(gap.args[0]) + Number(gap.args[2]) - Number(f.bar().args[0]);
      };
      const check = (width: number, target: number, value: number, readout: number) => {
        const tick = f.operations().find(op => op.op === 'fillText' && (op.args[0] === '100' || op.args[0] === '200'));
        assert.ok(tick);
        const axisWidth = Number(tick.args[1]) - Number(f.bar().args[0]);
        const axisValue = Number(tick.args[0]);
        assert.ok(Math.abs(f.barWidth() / axisWidth - width / axisValue) < .02);
        assert.ok(Math.abs(targetWidth() / axisWidth - target / axisValue) < .02);
        const text = f.operations().filter(op => op.op === 'fillText').map(op => op.args[0]);
        assert.ok(text.includes(String(value)));
        assert.ok(text.includes(`/ ${readout} a day`));
        assert.ok(text.includes(`\u2212${readout - value}`));
      };
      const first = f.presented.at(-1)?.entries[0];
      assert.match(JSON.stringify(first?.fields.audit), /target.*100/);
      f.canvas.clearOps();
      f.clock(100.5, 1, 1, true);
      f.tick(0);
      check((40 + nextValue) / 2, 140, 40, 100);
      assert.equal(f.served.at(-1)?.changed, 1);
      assert.equal(f.presented.at(-1)?.entries[0], first);

      f.canvas.clearOps();
      f.clock(101, 1);
      f.tick(0);
      check(nextValue, 180, nextValue, 180);
      f.canvas.clearOps();
      f.clock(100.5, 0);
      await f.request(100);
      check(40, 100, 40, 100);
    });
  });

  it('tweens target and delivered edges through plot changes and subpixel fills', async () => {
    const small = { value: 50, target: 100, scale: 100 };
    const large = { value: 505, target: 1000, scale: 1000 };
    const tiny = { value: .1, target: 100, scale: 100 };
    const zero = { value: 0, target: 100, scale: 100 };
    const fulfilled = { ...tiny, target: .1 };
    const unmet = { ...zero, target: .1 };
    for (const [first, next] of [[small, large], [tiny, zero], [zero, tiny], [fulfilled, unmet], [unmet, fulfilled]] as const) await withLane(async f => {
      f.bind(1, 1, false, 'target');
      f.record(100, first);
      f.record(101, next);
      await f.request(100);
      const edges = () => {
        const gap = f.operations().find(op => op.op === 'roundRect' && op.globalAlpha === 0.16);
        const fill = f.operations().find(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE);
        assert.ok(fill || gap);
        const delivered = fill ? Number(fill.args[0]) + Number(fill.args[2]) : Number(gap!.args[0]);
        const requested = gap ? Number(gap.args[0]) + Number(gap.args[2]) : delivered;
        return [delivered, requested];
      };
      const start = edges();
      f.canvas.clearOps();
      await f.request(101);
      const cold = edges();
      if (first.target !== next.target) assert.ok(Math.abs(start[1]! - cold[1]!) > 4, 'the readout shifts the plot edge');
      const end = [cold[0]!, Math.min(start[1]!, cold[1]!)];
      for (const fraction of [.5, .75, .99]) {
        f.canvas.clearOps();
        f.clock(100 + fraction, 1);
        f.tick(0);
        for (const [i, edge] of edges().entries()) {
          const expected = start[i]! + (end[i]! - start[i]!) * fraction;
          assert.ok(Math.abs(edge - expected) < .02, `edge ${i} at ${fraction}, got ${edge}, expected ${expected}`);
        }
        const labels = f.operations().filter(op => op.op === 'fillText').map(op => op.args[0]);
        assert.ok(labels.includes(String(first.value)));
        assert.ok(labels.includes(`/ ${first.target} a day`));
      }
    });
  });

  it('tweens an unmarked ordered fill from its cold minimum width to zero', async () => withLane(async f => {
    f.record(100, { value: .1, scale: 100 });
    f.record(101, { value: 0, scale: 100 });
    await f.request(100);
    assert.equal(f.barWidth(), 2);
    f.canvas.clearOps();
    f.clock(100.5, 1);
    f.tick(0);
    assert.equal(f.barWidth(), 1);
  }));

  it('tweens ordered segment edges from the cold shapes and keeps the total recorded', async () => withLane(async f => {
    f.bind(1, 1, false, 'segments');
    const first = { value: 1, scale: 400, parts: [['Army', .75], ['Navy', .75]] } as const;
    const next = { value: 52, scale: 1000, parts: [['Army', .25], ['Navy', .75]] } as const;
    f.record(100, first);
    f.record(101, next);
    const edges = () => [BLUE, ORANGE].map(color => {
      const op = f.operations().find(op => (op.op === 'roundRect' || op.op === 'fillRect') && op.fillStyle === color);
      assert.ok(op, `expected segment ${color}`);
      return Number(op.args[0]) + Number(op.args[2]);
    });
    const hasLabel = (value: number) => f.operations().some(op => op.op === 'fillText' && op.args[0] === String(value));
    await f.request(100);
    const start = edges();
    f.canvas.clearOps();
    await f.request(101);
    const end = edges();
    for (const fraction of [.25, .5, .99]) {
      f.canvas.clearOps();
      f.clock(100 + fraction, 1, 1, true);
      f.tick(0);
      for (const [i, edge] of edges().entries()) {
        const expected = start[i]! + (end[i]! - start[i]!) * fraction;
        assert.ok(Math.abs(edge - expected) < .02, `segment ${i} at ${fraction}, got ${edge}, expected ${expected}`);
      }
      assert.equal(f.served.at(-1)?.changed, 1);
      assert.ok(hasLabel(first.value));
    }
    f.canvas.clearOps();
    f.clock(101, 1);
    f.tick(0);
    assert.deepEqual(edges(), end);
    assert.ok(hasLabel(next.value));
    f.canvas.clearOps();
    f.clock(100.5, 0);
    await f.request(100);
    assert.deepEqual(edges(), start);
    assert.ok(hasLabel(first.value));
  }));

  it('cuts ordered segment motion at segment membership, order, sign and proof changes', async () => {
    const sameParts = [['Army', .65], ['Navy', .35]] as const;
    for (const [hour, next, segmentsIncomplete] of [
      [101, { value: 80, parts: [['Army', 1]] }],
      [101, { value: 80, parts: [['Army', .65], ['Air', .35]] }],
      [101, { value: 80, parts: [['Navy', .35], ['Army', .65]] }],
      [101, { value: -80, parts: sameParts }],
      [101, { value: 80, parts: sameParts }, true],
      [101, { value: null }],
      [102, { value: 80, parts: sameParts }],
    ] as const) await withLane(async f => {
      f.bind(1, 1, false, 'segments');
      f.record(100, { value: 40, parts: sameParts, segmentsIncomplete });
      f.record(hour, next);
      await f.request(100);
      const before = f.barWidth();
      f.clock(100.5, 1, 1, true);
      f.tick(0);
      assert.equal(f.served.at(-1)?.changed, 0);
      assert.equal(f.barWidth(), before);
    });
  });

  it('keeps existing plain bars moving when a different row crosses zero', async () => withLane(async f => {
    f.record(100, { value: -10, france: 30 });
    f.record(101, { value: 20, france: 50 });
    await f.request(100);
    const topWidth = () => f.operations().find(op => op.op === 'roundRect' && op.fillStyle === BLUE)?.args[2];
    const previous = topWidth();
    f.canvas.clearOps();
    f.clock(100.5, 1, 1, true);
    f.tick(0);
    assert.equal(f.served.at(-1)?.changed, 1);
    assert.ok(Number(topWidth()) > Number(previous));
  }));

  it('interpolates share bounds through a change to a complete bar while keeping recorded labels', async () => withLane(async f => {
    f.bind(1, 1, false, 'share');
    f.record(100, { form: 'share', value: 80, parts: [['Army', .3], ['Navy', .4]] });
    f.record(101, { form: 'share', value: 100, parts: [['Army', .45], ['Navy', .55]] });
    const edges = () => {
      const ops = f.operations();
      const bounds = [BLUE, ORANGE].flatMap(color => {
        const fill = ops.find(op => op.op === 'fillRect' && op.fillStyle === color);
        assert.ok(fill, `expected share segment ${color}`);
        return [Number(fill.args[0]), Number(fill.args[0]) + Number(fill.args[2])];
      });
      const remainder = ops.find(op => op.op === 'roundRect' && Number(op.args[0]) > bounds[3]!);
      return [...bounds, remainder ? Number(remainder.args[0]) : bounds[3]!];
    };
    const labels = () => f.operations().filter(op => op.op === 'fillText').map(op => op.args[0]);
    await f.request(100);
    const initial = edges();
    f.canvas.clearOps();
    await f.request(101);
    const exact = edges();
    assert.ok(initial[4]! > initial[3]!);
    assert.equal(exact[4], exact[3]);
    for (const fraction of [.25, .5, .99]) {
      f.canvas.clearOps();
      f.clock(100 + fraction, 1, 1, true);
      f.tick(0);
      assert.equal(f.served.at(-1)?.changed, 1);
      for (const [i, edge] of edges().entries()) {
        const expected = initial[i]! + (exact[i]! - initial[i]!) * fraction;
        assert.ok(Math.abs(edge - expected) < .01, `share edge ${i} at ${fraction}: ${edge} vs ${expected}`);
      }
      assert.ok(labels().includes('Army 30%'));
      assert.ok(labels().includes('80 units'));
    }
    f.canvas.clearOps();
    f.clock(100.5, 0);
    await f.request(100);
    assert.deepEqual(edges(), initial);
    assert.ok(labels().includes('80 units'));
  }));

  it('holds share bars when reading proof or active segments change', async () => {
    const first: Sample = { form: 'share', value: 80, parts: [['Army', .3], ['Navy', .2]] };
    const next = { ...first, value: 100 };
    for (const [reason, hour, sample] of [
      ['missing successor', 102, { ...next, parts: [['Army', .5], ['Navy', .3]] }],
      ['partial breakdown', 101, { ...next, parts: [['Army', .5], ['Navy', .3]], segmentsIncomplete: true }],
      ['missing total', 101, { ...next, value: null }],
      ['different part', 101, { ...next, parts: [['Army', .5], ['Air', .3]] }],
      ['reordered parts', 101, { ...next, parts: [['Navy', .5], ['Army', .3]] }],
      ['zero-width part', 101, { ...next, parts: [['Army', .5], ['Navy', 0]] }],
      ['changed rows', 101, { ...next, key: 'different-row' }],
    ] as const) await withLane(async f => {
      f.bind(1, 1, false, 'share');
      f.record(100, first);
      f.record(hour, sample);
      await f.request(100);
      f.canvas.clearOps();
      f.clock(100.5, 1, 1, true);
      f.tick(0);
      const diagnostic = f.served.at(-1)?.motionDiagnostics;
      assert.equal(f.served.at(-1)?.changed, 0, reason);
      assert.equal(diagnostic?.skipped[hour === 102 ? 'noBracket' : 'incompatible'], 1, reason);
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '80 units'), reason);
    });
  });

  it('cuts share motion when a segment label changes its cold fit decision', async () => withLane(async f => {
    f.bind(1, 1, false, 'share');
    f.record(100, { form: 'share', value: 80, parts: [['Army', .2], ['Navy', .4]] });
    f.record(101, { form: 'share', value: 100, parts: [['Army', .45], ['Navy', .55]] });
    await f.request(100);
    f.clock(100.5, 1, 1, true);
    f.tick(0);
    assert.equal(f.served.at(-1)?.changed, 0);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === 'Navy 40%'));
  }));

  it('cuts target motion when a marker disappears or proof has a gap', async () => {
    for (const [hour, target] of [[101, undefined], [102, 180]] as const) await withLane(async f => {
      f.bind(1, 1, false, 'target');
      f.record(100, { value: 40, target: 100 });
      f.record(hour, target === undefined ? { value: 0, zeroTarget: true } : { value: 80, target });
      await f.request(100);
      const width = f.barWidth();
      f.clock(100.5, 1, 1, true);
      f.tick(0);
      assert.equal(f.served.at(-1)?.changed, 0);
      assert.equal(f.barWidth(), width);
      f.clock(hour, 1);
      f.canvas.clearOps();
      f.tick(0);
      const recorded = target === undefined ? '/ 0 a day' : '80';
      assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === recorded));
      assert.equal(f.operations().some(op => op.op === 'roundRect' && op.globalAlpha === 0.16), target !== undefined);
    });
  });

  it('draws exact transport and rank refusals within their proof and cuts across adjacent gaps', async () => {
    for (const origin of ['transport', 'rank']) await withLane(async f => {
      f.record(100, { value: 100 });
      f.lane.receive('panels', origin === 'transport' ? datedRecord(1, 101, 0, 'Unavailable') : datedPanel(1, 101, {
        branchId: '1', hour: 101, from: 101, until: 101, measures: [{ name: 'ownerDivisions', decimals: 0 }],
        refused: 'The ranking is incomplete', rows: [],
      }));
      f.record(103, { value: 300 });
      await f.request(100);
      const start = f.barWidth();
      for (const [position, unavailable] of [[100.5, false], [101.5, true], [102.5, false]] as const) {
        f.canvas.clearOps();
        f.clock(position, 1, 1, true);
        f.tick(0);
        const barDrawn = f.operations().some(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE);
        assert.equal(barDrawn, !unavailable, `${origin} at ${position}`);
        if (unavailable) assert.equal(f.served.at(-1)?.motionDiagnostics?.recorded, 1);
        else assert.equal(f.barWidth(), start);
      }
      f.clock(103.5, 1);
      f.tick(0);
      assert.equal(f.barWidth(), start * 3);
      assert.ok(!f.served.some(frame => frame.motionDiagnostics && frame.changed));
    });
  });

  it('repaints a changed measure exactly when the old bracket is no longer confirmed', async () => withLane(async f => {
    f.record(100, { value: 50 });
    f.record(101, { value: 100 });
    await f.request(100);
    const single = f.barWidth();
    f.clock(100.5, 1);
    f.tick(0);
    assert.ok(f.barWidth() > single);
    f.bind(3);
    f.lane.stream(1, 2, 100);
    f.record(100, { value: 50 }, 2);
    f.clock(100.5, 1, 2);
    await f.request(100, 1, 2);
    assert.equal(f.barWidth(), single * 3);
    f.tick(0);
    assert.equal(f.barWidth(), single * 3);
  }));

  it('holds the seek-carried reading through and beyond its proof until fresh facts arrive', async () => withLane(async f => {
    f.record(100, { value: 50 });
    f.record(123);
    f.record(124, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1);
    const exact = f.barWidth();
    await f.request(100, 1, 1, true);
    f.lane.stream(1, 2, 110);
    assert.ok(f.lane.facts.select(1, 110)?.carried);
    f.clock(110, 0, 2);
    await f.request(110, 0, 2);
    assert.equal(f.barWidth(), exact);

    f.clock(110.5, 1, 2, true);
    await f.request(110, 1, 2, true);
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.barWidth(), exact);
    f.clock(124.5, 1, 2, true);
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.barWidth(), exact);
    assert.equal(f.served.at(-1)?.motionDiagnostics?.skipped.unproven, 1);
    f.record(124, { value: 90 }, 2);
    f.clock(122.5, 1, 2, true);
    f.canvas.clearOps();
    f.tick(0);
    assert.equal(f.barWidth(), exact);
    assert.equal(f.served.at(-1)?.changed, 0);
  }));

  it('does not bring a seek-carried bar back past a recorded cut', async () => {
    for (const cut of ['transport refusal', 'rank refusal', 'missing reading', 'proof hole']) await withLane(async f => {
      f.record(100, { value: 50 });
      f.record(123);
      f.clock(100, 1);
      await f.request(100, 1);
      const width = f.barWidth();
      await f.request(100, 1, 1, true);
      f.lane.stream(1, 2, 110);
      f.clock(110, 0, 2);
      await f.request(110, 0, 2);
      f.clock(110.5, 1, 2, true);
      await f.request(110, 1, 2, true);
      f.canvas.clearOps();
      f.clock(124.5, 1, 2, true);
      f.tick(0);
      assert.equal(f.barWidth(), width, cut);

      if (cut === 'transport refusal') f.lane.receive('panels', datedRecord(1, 124, 0, 'Unavailable', 2));
      if (cut === 'rank refusal') f.lane.receive('panels', datedPanel(1, 124, { rows: [], refused: 'Incomplete rank' }, 2));
      if (cut === 'missing reading') f.record(124, { value: null }, 2);
      f.record(126, { value: 100 }, 2);
      assert.equal(f.lane.facts.select(1, 126)?.h, 126, cut);
      f.clock(125.5, 1, 2, true);
      f.canvas.clearOps();
      f.tick(0);
      assert.ok(!f.operations().some(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE), cut);

      if (cut === 'proof hole') {
        f.canvas.clearOps();
        f.lane.supply(1, { statics: {} });
        await f.request(125, 1, 2, false, false, 126);
        assert.ok(!f.operations().some(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE), 'reacquiring the renderer cannot bring the carried bar back');
      }

      f.clock(126.5, 1, 2, true);
      f.tick(0);
      assert.equal(f.barWidth(), width * 2, cut);
    });
  });

  it('paints a newly ready projection on the next ordinary playing request', async () => withLane(async f => {
    f.ready(false);
    f.clock(100.5, 1);
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    await f.request(100, 1);
    assert.equal(f.operations().length, 0);
    f.ready(true);
    f.lane.touch(1);
    await f.drain();
    assert.equal(f.operations().length, 0);
    await f.request(100, 1);
    const exact = f.barWidth();
    f.tick(0);
    assert.ok(f.barWidth() > exact);
  }));

  it('gives subsequent slots their turn when a repaint consumes the worker budget', async () => withLane(async f => {
    f.mount(3);
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    await f.request(100);
    f.paintCost(11);
    f.clock(100.2, 1);
    f.tick(0);
    assert.deepEqual(f.sampled, [1]);
    f.tick(16);
    assert.deepEqual(f.sampled, [1, 2]);
    f.tick(16);
    assert.deepEqual(f.sampled, [1, 2, 3]);
  }, 10));

  it('budgets fact selection even if each playing slot already shows its exact value', async () => withLane(async f => {
    f.mount(3);
    f.record(100, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1);
    f.spendOnFactSelection(11);
    f.tick(0);
    assert.deepEqual(f.sampled, [1]);
    f.tick(0);
    assert.deepEqual(f.sampled, [1, 2]);
  }, 10));

  it('uses extended date proof even when an unchanged entry is still pending', async () => withLane(async f => {
    f.lane.supply(1, { dated: { h: 100, c: 100 } });
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    const exact = f.barWidth();
    f.lane.supply(1, { dated: { h: 100, c: 101 } });
    f.tick(0);
    assert.ok(f.barWidth() > exact);
    await f.drain();
  }));

  it('restores the picture kept by the page when the paused hour is uncovered', async () => withLane(async f => {
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    const exact = f.barWidth();
    await f.request(100, 1, 1, true);
    f.tick(0);
    assert.ok(f.barWidth() > exact);
    const count = f.presented.flatMap(p => p.entries).length;
    f.clock(102, 0);
    await f.request(102);
    assert.equal(f.barWidth(), exact);
    assert.equal(f.presented.flatMap(p => p.entries).length, count);
  }));

  it('uses live proof to restore a playing picture when the page request has no selection', async () => {
    for (const [hour, value, refusal] of [
      [102.5, 100, false], [103, 300, false], [104, 300, false], [104, null, true], [105, 300, true],
    ] as const) await withLane(async f => {
      f.lane.supply(1, { dated: { h: 100, c: 101 } });
      f.record(100, { value: 100 });
      f.record(101, { value: 200 });
      f.clock(100.5, 1);
      await f.request(100, 1);
      const exact = f.barWidth();
      await f.request(100, 1, 1, true);
      f.tick(0);
      assert.ok(f.barWidth() > exact);
      f.lane.supply(1, { dated: { h: 103, c: refusal ? 104 : 103 } });
      f.record(103, { value: 300 });
      if (refusal) f.lane.receive('panels', datedPanel(1, 104, { rows: [], refused: 'Incomplete rank' }));
      f.clock(hour, 1);
      f.tick(0);
      const last = f.presented.flatMap(reply => reply.entries).at(-1);
      const samples = f.sampled.length;
      f.canvas.clearOps();
      await f.request(102, 1);
      if (value === null) {
        assert.equal(f.sampled.length, samples + 1, 'the renderer draws the covered refusal');
        assert.ok(!f.operations().some(op => (op.op === 'fillRect' || op.op === 'roundRect') && op.fillStyle === BLUE));
      } else {
        assert.equal(f.barWidth(), exact * value / 100, `live hour ${hour}`);
      }
      assert.equal(f.presented.flatMap(reply => reply.entries).at(-1), last, 'there is no new page picture for uncovered hour 102');
    });
  });

  it('keeps the recorded picture when a pin has no selection and the live clock does', async () => withLane(async f => {
    f.lane.supply(1, { dated: { h: 100, c: 101 } });
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100, 1);
    await f.request(100, 1);
    const width = f.barWidth();
    await f.request(100, 1, 1, true);

    f.bind(2, 1, false, false, 102);
    f.lane.supply(1, { dated: { h: 103, c: 103 } });
    f.record(103, { value: 150 });
    f.clock(103, 1);
    f.canvas.clearOps();
    const last = f.presented.flatMap(reply => reply.entries).at(-1);
    await f.request(103, 1);
    assert.equal(f.barWidth(), width);
    assert.equal(f.presented.flatMap(reply => reply.entries).at(-1), last);
  }));

  it('paints ready artwork on the next tick despite a matching unacknowledged page entry', async () => withLane(async f => {
    f.record(100, { value: 100, art: true });
    f.clock(100, 1);
    await f.request(100, 1);
    assert.ok(!f.operations().some(op => op.op === 'drawImage'));
    f.painted.length = 0;
    f.finishArtwork();
    await f.drain();
    assert.deepEqual(f.painted, []);
    f.tick(0);
    assert.equal(f.operations().filter(op => op.op === 'drawImage').length, 1);
    await f.request(100, 1);
    assert.deepEqual(f.painted, [1]);
    assert.equal(f.operations().filter(op => op.op === 'drawImage').length, 1);
    assert.equal(typeof f.presented.at(-1)?.entries[0]?.fields.meta, 'object');
    await f.request(100, 1);
    assert.deepEqual(f.painted, [1]);
  }));

  it('keeps the later preview when artwork completes before an older page request', async () => withLane(async f => {
    f.record(100, { value: 30, art: true });
    f.record(101, { value: 60, art: true });
    f.record(102, { value: 100, art: true });
    f.clock(100.5, 1);
    await f.request(100, 1);
    const width = f.barWidth();
    f.clock(101.5, 1);
    f.tick(0);
    const preview = f.barWidth();
    assert.ok(Math.abs(preview / width - 80 / 30) < .01);
    const entries = f.presented.length;
    f.finishArtwork();
    await f.drain();
    assert.equal(f.presented.length, entries);
    await f.request(100, 1);
    assert.equal(f.barWidth(), preview);
    f.tick(0);
    assert.equal(f.barWidth(), preview);
    assert.ok(f.operations().some(op => op.op === 'drawImage'));
  }));

  it('publishes exact recorded metadata without replacing a later preview on an ordinary request', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(101, { value: 60 });
    f.record(102, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1);
    const width = f.barWidth();
    f.clock(101.5, 1);
    f.tick(0);
    const preview = f.barWidth();
    assert.ok(Math.abs(preview / width - 80 / 30) < .01);
    await f.request(101, 1);
    assert.equal(f.barWidth(), preview);
    const entry = f.presented.at(-1)?.entries[0];
    assert.equal(entry?.lo, 101);
    assert.match(JSON.stringify(entry?.fields.audit), /60/);
    assert.ok(!entry?.repainted);
    assert.equal(entry?.recordedPicture, true);
    assert.notEqual(entry?.content[0], f.presented[0]?.entries[0]?.content[0]);
  }));

  it('several expensive artwork wakes do no work between exact playing page requests', async () => withLane(async f => {
    f.mount(3, true);
    f.record(100, { value: 100, art: true });
    f.record(101, { value: 200, art: true });
    f.clock(100.5, 1);
    await f.request(100, 1, 1, false, true);
    f.selectionCost(11);
    f.painted.length = 0;
    f.finishArtwork();
    await f.drain();
    assert.deepEqual(f.painted, []);
    assert.equal(f.selections(), 0);
  }, 10));

  it('selects the first static picture on the next same-hour page publish after dated supply', async () => withLane(async f => {
    f.lane.mount(1, [], 'bars', {});
    f.bind();
    f.clock(100, 1);
    const page = new Page<TestBitmap>({ lanes: 1, seek: 1, post: (_lane, request) => {
      if (request.type === 'frame') f.lane.frame(request);
    } });
    page.mount(1, { route: 1, lane: 0, surfaces: [] });
    page.publish(100, { playing: true, speed: 1 });
    await f.drain();
    assert.deepEqual(f.painted, []);
    f.lane.supply(1, { dated: { h: 100, c: 100 } });
    await f.drain();
    assert.deepEqual(f.painted, []);
    page.publish(100);
    await f.drain();
    assert.deepEqual(f.painted, [1]);
    assert.equal(f.presented.at(-1)?.entries[0]?.lo, 100);
  }));

  it('includes the affected route reset with the first entry after artwork becomes ready', async () => withLane(async f => {
    f.record(100, { value: 100, art: true });
    f.clock(100, 1);
    await f.request(100, 1);
    const before = f.presented.length;
    f.lane.receive('follow', message(100, [], true, 9, 1), 1);
    f.finishArtwork();
    await f.drain();
    await f.request(100, 1);
    const first = f.presented.slice(before).find(p => p.entries.some(entry => entry.gen.reset === 1));
    assert.ok(first);
    assert.ok(first.resets?.some(([route, reset]) => route === 1 && reset === 1));
  }));

  it('does not serve a target bar or its dirty sibling between page requests', async () => withLane(async f => {
    f.mount(2, true);
    f.bind(1, 1, false, true);
    f.record(100, { value: 100, target: true });
    f.clock(100, 1);
    await f.request(100, 1);
    const served = f.served.length;
    f.painted.length = 0;
    f.lane.touch(2);
    f.lane.touch(1);
    await f.drain();
    assert.equal(f.served.length, served);
    assert.deepEqual(f.painted, []);
  }));

  it('keeps eligible artwork and a dirty sibling pending for the next page request', async () => withLane(async f => {
    f.mount(2, true);
    f.record(100, { value: 100 });
    f.clock(100, 1);
    await f.request(100, 1);
    await f.request(100, 1, 1, true);
    f.painted.length = 0;
    f.lane.touch(2);
    f.lane.touch(1);
    await f.drain();
    assert.deepEqual(f.painted, []);
    await f.request(100, 1);
    assert.deepEqual(f.painted, [2, 1]);
  }));

  it('gives a sibling its ordinary paint before spending the pause budget on pending bar restorations', async () => withLane(async f => {
    f.mount(4, true);
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    f.tick(0);
    assert.deepEqual(f.sampled, [1, 2, 3]);
    f.painted.length = 0;
    f.selectionCost(6);
    f.clock(101, 0);
    await f.request(101);
    assert.equal(f.painted[0], 4);
    assert.ok(f.selections() <= 2, 'a pause slice cannot select every previewed bar before yielding');
    for (let i = 0; i < 4; i++) {
      f.tick(16);
      await f.drain();
    }
    assert.deepEqual(f.painted, [4, 1, 2, 3]);
  }, 10));

  it('gives a ready pause its turn even when uncovered bars use the earlier budget slices', async () => withLane(async f => {
    f.mount(3);
    for (const id of [1, 2]) f.lane.supply(id, { dated: { h: 102, c: 102 } });
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100.5, 1);
    await f.request(100, 1);
    f.tick(0);
    assert.deepEqual(f.sampled, [3]);
    f.painted.length = 0;
    f.spendOnFactSelection(11);
    f.clock(101, 0);
    await f.request(101);
    for (let i = 0; i < 3; i++) {
      f.tick(16);
      await f.drain();
    }
    assert.deepEqual(f.painted, [3]);
    assert.ok(f.operations().some(op => op.op === 'fillText' && op.args[0] === '200'));
    const served = f.served.length;
    f.tick(16);
    await f.drain();
    assert.equal(f.served.length, served, 'uncovered bars retry when data or another request arrives');
  }, 10));

  it('gives a sibling its ordinary paint before selecting bars due on the first Play request', async () => withLane(async f => {
    f.mount(4, true);
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    await f.request(100);
    await f.request(100, 0, 1, true);
    f.clock(101.5, 1);
    f.lane.touch(4);
    f.painted.length = 0;
    f.selectionCost(6);
    await f.request(101, 1);
    assert.equal(f.painted[0], 4);
    assert.ok(f.selections() <= 2);
  }, 10));

  it('restores a deferred playback renderer even when a sibling fills each lane slice', async () => withLane(async f => {
    f.mount(2, true);
    f.record(100, { value: 100 });
    f.record(101, { value: 200 });
    f.clock(100, 1);
    await f.request(100, 1);
    const exact = f.barWidth();
    await f.request(100, 1, 1, true);
    f.lane.supply(1, { statics: {} });
    f.painted.length = 0;
    f.siblingCost(11);
    for (let i = 0; i < 4; i++) {
      f.lane.touch(2);
      await f.request(100, 1, 1, true);
    }
    assert.equal(f.painted[0], 2);
    assert.equal(f.painted.filter(id => id === 1).length, 1);
    f.clock(100.5, 1);
    f.tick(0);
    assert.ok(Math.abs(f.barWidth() / exact - 1.5) < .01);
  }, 10));

  it('draws without posting a playback message when performance recording is off', async () => withLane(async f => {
    f.record(100, { value: 30 });
    f.record(101, { value: 60 });
    f.clock(100, 1);
    await f.request(100, 1);
    const start = f.barWidth();
    const served = f.served.length;
    f.tick(500);
    assert.ok(f.barWidth() > start);
    assert.equal(f.served.length, served);
  }));

  it('interpolates a recorded number zero to a recorded whole-number bigint', async () => withLane(async f => {
    f.record(100, { value: 0, whole: true });
    f.record(101, { value: 200, whole: true });
    f.clock(100.5, 1);
    await f.request(100, 1);
    f.tick(0);
    const between = f.barWidth();
    f.clock(101, 0);
    await f.request(101);
    assert.equal(f.barWidth(), between * 2);
  }));

  it('uses the same interpolated scale for the width and the axis', async () => withLane(async f => {
    f.record(100, { value: 60, scale: 200 });
    f.record(101, { value: 100, scale: 400 });
    const sample = () => {
      const tick = f.operations().find(op => op.op === 'fillText' && op.args[0] === '100');
      assert.ok(tick);
      return { width: f.barWidth(), oneHundred: Number(tick.args[1]) - Number(f.bar().args[0]) };
    };
    await f.request(100);
    const first = sample();
    f.canvas.clearOps();
    await f.request(101);
    const last = sample();
    f.canvas.clearOps();
    f.clock(100.5, 1);
    f.tick(0);
    const middle = sample();
    assert.ok(Math.abs(middle.width - (first.width + last.width) / 2) < .02);
    assert.ok(Math.abs(middle.oneHundred - (first.oneHundred + last.oneHundred) / 2) < 1);
  }));
});
