import assert from 'node:assert/strict';
import test from 'node:test';
import { encode } from '@msgpack/msgpack';
import type { Fact } from '../dashboard/follow/facts.ts';
import { Page, type Bitmap, type PageMessage } from '../dashboard/follow/page.ts';
import { PayloadSender } from '../dashboard/follow/payloads.ts';
import { Lane, type LaneView, type Painted, type Value } from '../draw/lane.ts';
import { cropSurfaces } from '../draw/laneDispatch.ts';
import type { BoundedReadView, FamilyProjection } from './contract.ts';
import { createLaneView, lazyLaneView, type LaneBinding, type LaneFamily } from './laneRuntime.ts';
import { familyBinding, type FamilyBindingContext, type FamilyHostAdapter } from './host/adapter.ts';
import { assertContinuity, assertFreshPaint, assertQuietFrames, assertTooltipDated, driveLane, installRecordingCanvas, LANE_THEME as THEME, LaneTransport } from './laneRuntime.fixture.ts';
import { bindFamilyManifest, registerFamilyProjection } from './registry.ts';
import { IdentityIndex } from './identity/index.ts';
import { stateKey, type IdentitySnapshot } from './identity/types.ts';
import type { StripFrame } from '../dashboard/strip/scrub.ts';

type Row = { readonly key: string; readonly v: number };
type Options = { readonly order: 'up' | 'down'; readonly name?: string; readonly state?: string };
type Model = { readonly hour: number; readonly bucket: number; readonly rows: readonly Row[]; readonly name: string };
type Layout = { readonly rows: readonly { readonly key: string; readonly y: number; readonly v: number }[] };
type Binding = LaneBinding<Options> & { readonly family: 'test-lane' };

const counts = { project: 0, nextChange: 0, layout: 0, paint: 0, tooltip: 0, mirror: 0 };
const reset = () => { for (const key of Object.keys(counts) as (keyof typeof counts)[]) counts[key] = 0; };

registerFamilyProjection<FamilyProjection<Model, Options>>({
  kind: 'fixture',
  family: 'test-lane',
  project(view: BoundedReadView, options: Options): Model {
    counts.project++;
    const rows = [...(view.panel<{ rows: Row[] }>('main')?.rows ?? [])];
    const name = options.state ? view.who('state', options.state, view.hour).name :
      options.name ? view.who('country', options.name, view.hour).name : null;
    return { hour: view.hour, bucket: Math.floor(view.hour / 10), rows: options.order === 'up' ? rows.reverse() : rows, name: name?.status === 'ready' ? name.value : options.name ?? '' };
  },
  nextChange(view: BoundedReadView): number {
    counts.nextChange++;
    return Math.floor(view.hour / 10) * 10 + 10;
  },
});

const family: LaneFamily<Binding, Model, Layout> = {
  family: 'test-lane',
  boundary: () => Infinity,
  redraw: () => Infinity,
  status: () => 'picture',
  view: [],
  layout(model) {
    counts.layout++;
    return { rows: model.rows.map((row, at) => ({ key: row.key, y: at * 20, v: row.v })) };
  },
  paint(ctx, frame) {
    counts.paint++;
    ctx.clearRect(0, 0, frame.surface.width, frame.surface.height);
    ctx.fillStyle = frame.theme.text;
    for (const row of frame.layout.rows) ctx.fillText(`${row.key} ${row.v} ${frame.model.name} ${frame.model.bucket}`, 0, row.y - frame.binding.scrollTop);
    return false;
  },
  hit: (frame, point) => frame.layout.rows.find(row => point[1] >= row.y && point[1] < row.y + 20)?.key ?? null,
  tooltip(frame, key) {
    counts.tooltip++;
    const row = frame.model.rows.find(item => item.key === key);
    return row ? { rowKey: key, content: { title: key, rows: [] } as never } : null;
  },
  structure: frame => ({ meta: { count: frame.model.rows.length }, nav: frame.layout.rows.map(row => row.key) }),
  mirror: frame => {
    counts.mirror++;
    return frame.model.rows.map(row => `${row.key} ${row.v}`);
  },
  audit: frame => frame.layout.rows.map(row => [row.key, row.v]),
  visibleArt: () => [],
};

const rowsAt = (rows: readonly Row[]) => ({ main: { rows } });
const DOWN: Options = { order: 'down' };
const NO_SOURCES = {};
const binding = (staticPanels: Readonly<Record<string, unknown>>, options: Options = DOWN, extra: Partial<Binding> = {}): Binding => ({
  family: 'test-lane', kind: 'fixture', options, sources: NO_SOURCES, staticPanels, scrollTop: 0, theme: THEME, ...extra,
});

test('a scrub projects a stable statics-only picture and leaves the exact scene, facts and painter alone', () => {
  const recording = installRecordingCanvas();
  try {
    const view = createLaneView(family, () => {});
    const lane = new Lane({
      lane: 0, lanes: 1, views: () => view, crop: async () => assert.fail('scrub must not crop'), present: () => assert.fail('scrub must not present'),
      release() {}, fail: assert.fail, later() {},
    });
    const bound = binding(rowsAt([{ key: 'old', v: -1 }]), { ...DOWN, state: '7' });
    lane.mount(10, [], 'test-lane', bound);
    const whole = encode({ answer: { rows: [{ key: 'A', v: 1 }] } });
    const forward = encode([{ replace: [['answer', 'rows', 0, 'v'], 2] }]);
    const back = encode([{ replace: [['answer', 'rows', 0, 'v'], 1] }]);
    const refused = encode([{ replace: [[], { refused: 'Withheld by the recording' }] }]);
    const resume = encode([{ replace: [[], { answer: { rows: [{ key: 'A', v: 2 }] } }] }]);
    const parts = [whole, forward, back, refused, resume];
    const bytes = Buffer.alloc(8 + parts.reduce((sum, part) => sum + 4 + part.length, 0));
    bytes.set([66, 80, 83, 84, 1, 1, 3, 0]);
    let cursor = 8;
    for (const part of parts) {
      bytes.writeUInt32LE(part.length, cursor);
      bytes.set(part, cursor + 4);
      cursor += 4 + part.length;
    }
    lane.strip(10, 1, { kind: 'append', slot: 'main', hours: [6, 12, 18], fine: false, bytes });
    lane.strip(10, 1, { kind: 'mark', slot: 'main', hours: [0], fine: false });
    lane.strip(10, 1, { kind: 'scenes', page: 2, fine: false,
      track: { axisOffset: 500, branch: { firstHour: 500, lastHour: 520, gaps: [{ firstHour: 509, lastHour: 509, reason: 'Widget gap' }] } },
      frames: [0, 6, 12, 18].map(hour => [hour, { recorded: hour, allowEmpty: false, parts: [{ hour, kind: 'panel', name: 'main', slots: ['main'] }] }]),
    });
    Object.defineProperty(lane, 'facts', { get: () => assert.fail('scrub must not access facts') });
    const show = (hour: number, gap: StripFrame['gap'] = null) => {
      lane.scrub(2, { page: 2, frame: hour + 1, hour, density: 1, gap });
      return lane.scrubScenes?.widgets.get(10);
    };
    const names = new IdentityIndex({ sourceId: 's', revision: 'r', branchId: '1', content: 0 }, [0, 20]);
    lane.payloads({ type: 'payloads', put: [[1, names.exportSnapshot()]] });
    lane.strip(10, 1, { kind: 'identities', fine: false, snapshot: 1 });
    lane.payloads({ type: 'payloads', drop: [1] });
    const discovery = () => lane.strip(10, 1, { kind: 'discover', fine: false, hour: 12, measure: false });
    assert.deepEqual(discovery().identities, [{ key: stateKey(7), hour: 12 }]);
    assert.equal(discovery().unresolved, true);
    names.setEntityRuns(stateKey(7), [
      { firstHour: 0, lastHour: 9, name: 'Before the rename' },
      { firstHour: 10, lastHour: 20, name: 'After the rename' },
    ]);
    lane.strip(10, 1, { kind: 'identities', fine: false, snapshot: names.exportSnapshot() });
    assert.equal(discovery().unresolved, false);
    const staging = show(12);
    assert.equal(staging?.mode === 'picture' && (staging.picture.model as Model).name, '');
    lane.strip(10, 1, { kind: 'identities', fine: false });
    lane.strip(10, 1, { kind: 'identities', fine: false });
    const exact = driveLane(view, recording.canvases);
    const a = show(6), b = show(12);
    assert.equal(a?.mode, 'picture');
    assert.equal(b?.mode, 'picture');
    assert.equal((a.picture.model as Model).name, 'Before the rename');
    assert.equal((b.picture.model as Model).name, 'After the rename');
    const expected = exact.paint({ label: 'exact', binding: binding(rowsAt([{ key: 'A', v: 2 }]), DOWN, { audit: true }), hour: 12 });
    assert.deepEqual((b.picture.model as Model).rows.map(row => [row.key, row.v]), (expected.fields.audit?.value as { rows: unknown }).rows);
    assert.equal((a.picture.model as Model).rows[0]?.v, 1, 'a later in-place patch cannot change the earlier scrub picture');
    assert.deepEqual(show(0), { mode: 'missing' });
    assert.deepEqual(show(18), { mode: 'refused', reason: 'Withheld by the recording' });
    const gap = { kind: 'not-recorded' as const, message: 'Not recorded at this time.', detail: undefined };
    assert.deepEqual(show(9, gap), { mode: 'outside', outside: {
      ...gap, detail: 'Widget gap', span: { firstHour: 509, lastHour: 509 },
    } });
    assert.equal(show(7, gap), undefined, 'a reference gap cannot fabricate a missing picture on a recorded track');
    lane.scrub(2, null);
    assert.equal(lane.scrubScenes, undefined);
    assert.deepEqual(bound.staticPanels, rowsAt([{ key: 'old', v: -1 }]));
    assert.equal(recording.canvases.length, 1, 'only the exact paint used a canvas');
  } finally {
    recording.restore();
  }
});

test('a projection’s identity demands are published by the paint that ran it', () => {
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    const painted = lane.paint({ label: 'first', binding: binding(rowsAt([{ key: 'A', v: 1 }]), { order: 'down', name: 'GER' }), hour: 12 });
    assert.deepEqual(painted.fields.identityDemands?.value, [['country', 'GER', 12]]);
    const plain = lane.paint({ label: 'no names', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12 });
    assert.equal(plain.fields.identityDemands, undefined, 'a projection that asked for nothing publishes no demands');
  } finally {
    recording.restore();
  }
});

test('a quiet, steady or paused frame does no projection, layout or drawing, and next() answers from the held model', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const view = createLaneView(family, () => {});
    const lane = driveLane(view, recording.canvases);
    const bound = binding(rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }]));
    const first = lane.paint({ label: 'first', binding: bound, hour: 12, speed: 24 });
    assert.equal(first.nextChangeAt, 20, 'the proven interval reaches the projection’s next change');
    assert.deepEqual({ ...counts }, { project: 1, nextChange: 1, layout: 1, paint: 1, tooltip: 0, mirror: 1 });
    const scene = { widget: 1, subs: [], pictures: [], model: lane.model() };
    for (let hour = 13; hour < 20; hour++) assert.equal(view.next!(scene, hour, lane.state), 20);
    const steady = lane.paint({ label: 'steady', binding: bound, hour: 15, speed: 24 });
    const paused = lane.paint({ label: 'paused', binding: bound, hour: 15, speed: 0 });
    assert.deepEqual({ ...counts }, { project: 1, nextChange: 1, layout: 1, paint: 1, tooltip: 0, mirror: 1 }, 'quiet hours cost nothing');
    assert.equal(steady.surfaces[0]!.content, first.surfaces[0]!.content, 'a steady frame keeps its content tag');
    assert.equal(paused.surfaces[0]!.content, first.surfaces[0]!.content);
    lane.paint({ label: 'boundary', binding: bound, hour: 20, speed: 24 });
    assert.equal(counts.project, 2, 'the boundary rebuilds');
    assert.equal(counts.paint, 2, 'a family without a canvas comparison draws the new model');
  } finally {
    recording.restore();
  }
});

test('keeping the canvas across a new model skips every canvas operation and finishes animation', () => {
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView({
      ...family,
      keepsCanvas: (_ctx, frame) => frame.hour === 20,
      paint(ctx, frame) { family.paint(ctx, frame); return true; },
    }, () => {}), recording.canvases);
    const bound = binding(rowsAt([{ key: 'A', v: 1 }]));
    const first = lane.paint({ label: 'initial', binding: bound, hour: 19 });
    const frames = recording.canvases[0]!.frames.length;
    const kept = lane.paint({ label: 'new model', binding: bound, hour: 20 });
    assert.equal(kept.motion, false);
    assert.equal(recording.canvases[0]!.frames.length, frames);
    assert.equal(kept.surfaces[0]!.content, first.surfaces[0]!.content);
  } finally {
    recording.restore();
  }
});

test('a followed reply delivered again with the same bytes costs no projection, layout or drawing, and changed bytes rebuild', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    const subs = [{ id: 5, channel: 'followed' as const, route: null }];
    const bound = binding({}, DOWN, { sources: { main: 5 } });
    const at = (hour: number, bytes: number[]) => {
      const value: Value = { kind: 'panel', bytes: Uint8Array.from(bytes), data: { rows: [{ key: 'A', v: bytes.at(-1)! }] }, refused: null };
      const fact: Fact<Value> = { h: hour, c: hour, value, refused: null, cells: [], seeds: new Map(), channel: 'followed', version: hour, bytes: bytes.length };
      return lane.paint({ label: `hour ${hour}`, binding: bound, hour, speed: 24, subs, pictures: [{ h: hour, c: hour, parts: [fact], value, refused: null }] });
    };
    const first = at(12, [1, 2, 3]);
    const work = () => ({ project: counts.project, layout: counts.layout, paint: counts.paint });
    const before = work();
    const again = at(13, [1, 2, 3]);
    assert.deepEqual(work(), before, 'the same reply under a new fact and version is the same input');
    assert.equal(again.surfaces[0]!.content, first.surfaces[0]!.content, 'the picture is the one already shown');
    at(14, [1, 2, 4]);
    assert.deepEqual(work(), { project: before.project + 1, layout: before.layout + 1, paint: before.paint + 1 }, 'a changed reply rebuilds');
  } finally {
    recording.restore();
  }
});

test('a tooltip on a held picture reads the records that prove its drawn hour, even after a later record repeats the value', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const reading: LaneFamily<Binding, Model, Layout> = {
      ...family,
      tooltip(frame, key) {
        const row = frame.readNow().panel<{ rows: Row[] }>('main')?.rows.find(item => item.key === key);
        return row ? { rowKey: key, content: { title: `${key} ${row.v}`, rows: [] } as never } : null;
      },
    };
    const lane = driveLane(createLaneView(reading, () => {}), recording.canvases);
    const subs = [{ id: 5, channel: 'panel' as const, route: null }];
    const bound = binding({}, DOWN, { sources: { main: 5 } });
    const value: Value = { kind: 'panel', bytes: null, data: { hour: 11, rows: [{ key: 'A', v: 7 }] }, refused: null };
    const at = (h: number, hour: number, inspect: string | null) => {
      const fact: Fact<Value> = { h, c: 15, value, refused: null, cells: [], seeds: new Map(), channel: 'panel', version: h, bytes: 0 };
      return lane.paint({ label: `record ${h} at hour ${hour}`, binding: bound, hour, speed: 24, subs, inspect, pictures: [{ h, c: 15, parts: [fact], value, refused: null }] });
    };
    at(11, 11, null);
    const projections = counts.project;
    const repeated = at(13, 13, 'A');
    assert.equal(counts.project, projections, 'the repeated value keeps the held projection and its drawn hour');
    assert.equal(repeated.mode, 'picture');
    assert.equal((repeated.fields.tooltip?.value as { text?: { content?: { title?: string } } } | undefined)?.text?.content?.title, 'A 7');
  } finally {
    recording.restore();
  }
});

test('a held model ends where a bounded series it was given ends, even while its panel is unchanged', () => {
  let projected = 0;
  let seriesEnd = -Infinity;
  registerFamilyProjection<FamilyProjection<Model, Options>>({
    kind: 'fixture-series',
    family: 'test-lane',
    project(view: BoundedReadView): Model {
      projected++;
      seriesEnd = view.series('main')?.get('GER')?.validInterval[1] ?? -Infinity;
      return { hour: view.hour, bucket: seriesEnd, rows: [], name: '' };
    },
  });
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    const bucket = (first: number, last: number, value: number) => ({ firstHour: first, lastHour: last, firstValue: value, lastValue: value, lastValueHour: last,
      minHour: first, minValue: value, maxHour: last, maxValue: value, firstBreak: false, hasBreak: false });
    const line = { source: 'main', tag: 'GER', head: 'h', decimals: 0, binHours: 96, from: 0, to: 191, width: 2, buckets: [bucket(0, 95, 1), bucket(96, 191, 2)], missing: [], parts: [], marks: null };
    const bound = binding(rowsAt([]), DOWN, { kind: 'fixture-series', tiledBySource: { main: { GER: line } } } as Partial<Binding>);
    const first = lane.paint({ label: 'in the first bucket', binding: bound, hour: 50, speed: 24 });
    assert.ok(Number.isFinite(seriesEnd), 'the projection read the series');
    assert.equal(first.nextChangeAt, seriesEnd + 1, 'the held model stands no longer than its series');
    const before = projected;
    lane.paint({ label: 'past the series interval', binding: bound, hour: seriesEnd + 1, speed: 24 });
    assert.equal(projected, before + 1, 'the next hour of the series projects again');
  } finally {
    recording.restore();
  }
});

test('an unfocused mirror arms zero playback timers and synchronises on seek, focus or pause', () => {
  reset();
  const set: number[] = [];
  const pending: (() => void)[] = [];
  const timers = {
    set: (fn: () => void, ms: number) => { set.push(ms); pending.push(fn); return pending.length as unknown as ReturnType<typeof setTimeout>; },
    clear: () => {},
  };
  let wakes = 0;
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => { wakes++; }, { timers }), recording.canvases);
    const first = lane.paint({ label: 'first', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12, speed: 24, seek: 1, inspect: 'A' });
    assert.deepEqual(first.fields.mirror?.value, ['A 1'], 'the first paint emits its initial mirror at once');
    assert.equal(set.length, 0, 'the first paint arms no timer');
    for (let hour = 20; hour < 60; hour += 10) {
      const played = lane.paint({ label: `hour ${hour}`, binding: binding(rowsAt([{ key: 'A', v: hour }])), hour, speed: 24, seek: 1, inspect: 'A' });
      assert.equal(played.fields.mirror, undefined, 'unfocused played hours defer mirror derivation');
    }
    assert.equal(set.length, 0, 'unfocused played hours arm zero cadence timers');
    assert.equal(wakes, 0);
    const sought = lane.paint({ label: 'seek', binding: binding(rowsAt([{ key: 'A', v: 60 }])), hour: 60, speed: 24, seek: 2, inspect: 'A' });
    assert.deepEqual(sought.fields.mirror?.value, ['A 60'], 'a seek settles the mirror once');
    const focused = lane.paint({ label: 'focused', binding: binding(rowsAt([{ key: 'A', v: 70 }]), { order: 'down' }, { focusedKey: 'A' }), hour: 70, speed: 24, seek: 2, inspect: 'A' });
    assert.deepEqual(focused.fields.mirror?.value, ['A 70'], 'focusing updates the mirror live during playback');
    const paused = lane.paint({ label: 'paused', binding: binding(rowsAt([{ key: 'A', v: 80 }])), hour: 80, speed: 0, seek: 2, inspect: 'A' });
    assert.deepEqual(paused.fields.mirror?.value, ['A 80'], 'pausing flushes the mirror immediately');

    const switchedOwner = lane.paint({ label: 'owner-switch', binding: binding(rowsAt([{ key: 'B', v: 1 }])), hour: 90, speed: 24, seek: 2, owner: 'owner-2' });
    assert.deepEqual(switchedOwner.fields.mirror?.value, ['B 1'], 'switching owner at the same seek publishes the new owner’s initial mirror');
    const afterSwitch = lane.paint({ label: 'after-switch', binding: binding(rowsAt([{ key: 'B', v: 2 }])), hour: 100, speed: 24, seek: 2, owner: 'owner-2' });
    assert.equal(afterSwitch.fields.mirror, undefined, 'played frames after owner switch resume deferring');
    const requested = lane.paint({ label: 'mirror-request', binding: binding(rowsAt([{ key: 'B', v: 3 }]), DOWN, { mirrorRequest: 1 }), hour: 110, speed: 24, seek: 2, owner: 'owner-2' });
    assert.deepEqual(requested.fields.mirror?.value, ['B 3'], 'host mirrorRequest publishes a fresh mirror once at the same seek');
    const afterRequest = lane.paint({ label: 'after-request', binding: binding(rowsAt([{ key: 'B', v: 4 }]), DOWN, { mirrorRequest: 1 }), hour: 120, speed: 24, seek: 2, owner: 'owner-2' });
    assert.equal(afterRequest.fields.mirror, undefined, 'played frames after mirrorRequest resume deferring');
  } finally {
    recording.restore();
  }
});

test('an empty mirror gaining its first row during playback and a refused widget recovering at the same seek both publish their mirror', () => {
  reset();
  let refuse = false;
  const stateful: LaneFamily<Binding, Model, Layout> = {
    ...family,
    status: () => (refuse ? { refused: 'temporarily unavailable' } : 'picture'),
  };
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(stateful, () => {}), recording.canvases);
    const empty = lane.paint({ label: 'empty', binding: binding(rowsAt([])), hour: 10, speed: 24, seek: 1 });
    assert.deepEqual(empty.fields.mirror?.value, [], 'initial empty mirror is emitted');
    const firstRow = lane.paint({ label: 'first-row', binding: binding(rowsAt([{ key: 'E1', v: 1 }])), hour: 20, speed: 24, seek: 1 });
    assert.deepEqual(firstRow.fields.mirror?.value, ['E1 1'], 'transition from empty mirror to first row publishes a focusable mirror during playback');
    const secondRow = lane.paint({ label: 'second-row', binding: binding(rowsAt([{ key: 'E1', v: 1 }, { key: 'E2', v: 2 }])), hour: 30, speed: 24, seek: 1 });
    assert.equal(secondRow.fields.mirror, undefined, 'subsequent rows once populated defer during unfocused playback');

    refuse = true;
    const refused = lane.paint({ label: 'refused', binding: binding(rowsAt([{ key: 'E1', v: 1 }])), hour: 40, speed: 24, seek: 1 });
    assert.equal(refused.mode, 'refused');
    refuse = false;
    const recovered = lane.paint({ label: 'recovered', binding: binding(rowsAt([{ key: 'E1', v: 9 }])), hour: 50, speed: 24, seek: 1 });
    assert.deepEqual(recovered.fields.mirror?.value, ['E1 9'], 'recovery from refusal at the same seek publishes a fresh mirror');
    const afterRecovery = lane.paint({ label: 'after-recovery', binding: binding(rowsAt([{ key: 'E1', v: 10 }])), hour: 60, speed: 24, seek: 1 });
    assert.equal(afterRecovery.fields.mirror, undefined, 'subsequent played frames after recovery resume deferring');
  } finally {
    recording.restore();
  }
});

test('while playing, a correction inside the cadence never publishes the old tooltip or mirror beside the corrected picture', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    lane.paint({ label: 'first', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12, speed: 24, inspect: 'A', seek: 1 });
    const corrected = lane.paint({ label: 'corrected', binding: binding(rowsAt([{ key: 'A', v: 7 }])), hour: 12, speed: 24, inspect: 'A', seek: 1 });
    assert.deepEqual((corrected.fields.tooltip?.value as { text: { rowKey: string } } | undefined)?.text?.rowKey, 'A');
    assert.ok(counts.tooltip >= 2, 'the tooltip was derived again from the corrected frame');
    assert.equal(corrected.fields.mirror, undefined, 'the mirror is withheld until it is due, never the old rows');
    const due = lane.paint({ label: 'paused', binding: binding(rowsAt([{ key: 'A', v: 7 }])), hour: 12, speed: 0, inspect: 'A', seek: 1 });
    assert.deepEqual(due.fields.mirror?.value, ['A 7'], 'paused, the mirror is published at once');
  } finally {
    recording.restore();
  }
});

test('after a seek back, a same-hour correction and a reorder the picture equals a fresh paint', () => {
  const atTen = rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }]);
  const atTwenty = rowsAt([{ key: 'A', v: 5 }, { key: 'B', v: 3 }]);
  const corrected = rowsAt([{ key: 'A', v: 9 }, { key: 'B', v: 2 }]);
  const held = binding(atTwenty);
  assertFreshPaint(() => createLaneView(family, () => {}), [
    { label: 'later bucket', binding: held, hour: 25, speed: 24 },
    { label: 'seek back inside the same facts', binding: held, hour: 15, check: true },
    { label: 'at ten', binding: binding(atTen), hour: 10, check: true },
    { label: 'at twenty', binding: binding(atTwenty), hour: 20, speed: 24 },
    { label: 'seek back', binding: binding(atTen), hour: 10, check: true },
    { label: 'correction', binding: binding(corrected), hour: 10, check: true },
    { label: 'reorder', binding: binding(corrected, { order: 'up' }), hour: 10, check: true },
    { label: 'scroll', binding: binding(corrected, { order: 'up' }, { scrollTop: 20 }), hour: 10, check: true },
  ]);
});

test('a previous frame is supplied only for continuous forward playback of the same placement', () => {
  const seen: (number | null)[] = [];
  const watching: LaneFamily<Binding, Model, Layout> = { ...family, paint: (ctx, frame) => { seen.push(frame.before?.hour ?? null); return family.paint(ctx, frame); } };
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(watching, () => {}), recording.canvases);
    const rows = binding(rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }]));
    const swapped = binding(rowsAt([{ key: 'B', v: 3 }, { key: 'A', v: 1 }]));
    const paint = (step: Partial<Parameters<typeof lane.paint>[0]> & { readonly hour: number }) => {
      seen.length = 0;
      lane.paint({ label: 'step', binding: rows, speed: 24, seek: 1, owner: 'a', ...step });
      return seen.at(-1);
    };
    paint({ hour: 12 });
    assert.equal(paint({ hour: 20 }), 12, 'playing on into the next proven interval glides from the frame drawn before');
    assert.equal(paint({ hour: 10, seek: 2 }), null, 'a seek back gives none');
    assert.equal(paint({ hour: 25, seek: 3 }), null, 'a seek forward gives none');
    assert.equal(paint({ hour: 30, seek: 3, speed: 0 }), null, 'a paused frame gives none');
    assert.equal(paint({ hour: 40, seek: 3 }), null, 'nor does the first playing frame after it');
    assert.equal(paint({ hour: 50, seek: 3, owner: 'b' }), null, 'another owner gives none');
    assert.equal(paint({ hour: 60, seek: 3, owner: 'b', binding: binding(rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }]), { order: 'up' }) }), null, 'another selection gives none');
    assert.equal(paint({ hour: 55, seek: 3, owner: 'b', binding: swapped }), null, 'an earlier hour on the same seek gives none');
    assert.equal(paint({ hour: 70, seek: 3, owner: 'b', binding: swapped }), 55, 'playing on again glides');
  } finally {
    recording.restore();
  }
});

test('a secondary panel read on a clock 24 hours ahead reaches the projection with its shift, never as future data', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    const painted = lane.paint({ label: 'shifted', hour: 12, binding: { ...binding({ main: { hour: 36, rows: [{ key: 'A', v: 1 }] } }), staticShifts: { main: 24 } } });
    assert.equal(painted.mode, 'picture');
    assert.deepEqual(painted.fields.nav?.value, ['A']);
  } finally {
    recording.restore();
  }
});

test('the shared quiet-frame check: no drawing or new field between boundaries, and a playing chart’s next hour is a real boundary', () => {
  assert.equal(assertQuietFrames(() => createLaneView(family, () => {}), { label: 'fixture', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12 }), 20);
  const hourly: LaneFamily<Binding, Model, Layout> = { ...family, redraw: (_model, hour) => hour + 1 };
  assert.equal(assertQuietFrames(() => createLaneView(hourly, () => {}), { label: 'hourly', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12 }), 13);
});

const settle = async () => { for (let i = 0; i < 8; i++) await null; };

test('a cold projection or lane code load walks no history, keeps one waiter per placement, and a failed import fails by name until the host’s retry re-binds it', async () => {
  let release: (value: unknown) => void = () => {};
  let loads = 0;
  bindFamilyManifest([
    { family: 'test-lane', kind: 'cold', loadSpec: () => { loads++; return new Promise(resolve => { release = resolve; }) as never; } },
    { family: 'test-lane', kind: 'broken', loadSpec: () => { loads++; return Promise.reject(new Error('chunk missing')); } },
  ]);
  const recording = installRecordingCanvas();
  try {
    let wakes = 0;
    const view = createLaneView(family, () => { wakes++; });
    const lane = driveLane(view, recording.canvases);
    const cold = { ...binding(rowsAt([])), kind: 'cold' };
    const scene = { widget: 1, subs: [], pictures: [], model: cold };
    for (let hour = 0; hour < 1000; hour++) assert.equal(view.next!(scene, hour, lane.state), null, 'loading locates no change, so the lane walks no history and paints the target');
    for (let i = 0; i < 5; i++) assert.equal(lane.paint({ label: 'loading', binding: cold, hour: 10 }).surfaces.length, 0);
    release({ projection: { kind: 'cold', family: 'test-lane', project: (view: BoundedReadView) => ({ hour: view.hour, bucket: 0, rows: [], name: '' }) } });
    await settle();
    assert.deepEqual([loads, wakes], [1, 1], 'one import and one wake for the placement');

    const broken = { ...binding(rowsAt([])), kind: 'broken' };
    lane.paint({ label: 'broken', binding: broken, hour: 10 });
    await settle();
    for (let i = 0; i < 5; i++) assert.throws(() => lane.paint({ label: 'broken', binding: broken, hour: 10 }), /chunk missing/);
    assert.equal(loads, 2, 'a failed import is not retried on every paint');
    assert.equal(view.next!({ widget: 1, subs: [], pictures: [], model: broken }, 10, lane.state), null);
    assert.equal(lane.paint({ label: 'retried', binding: broken, hour: 10, generation: 2 }).surfaces.length, 0, 'a re-bind clears the failure and loads again');
    assert.equal(loads, 3);

    let releaseLane: (lane: LaneFamily<Binding, Model, Layout>) => void = () => {};
    let laneLoads = 0;
    const laneWakes: number[] = [];
    const lazy = lazyLaneView('test-lane', () => { laneLoads++; return new Promise(resolve => { releaseLane = resolve; }); }, widget => laneWakes.push(widget));
    const lazyLane = driveLane(lazy, recording.canvases);
    const shown = binding(rowsAt([{ key: 'A', v: 1 }]));
    for (const widget of [1, 2]) assert.equal(lazy.next!({ widget, subs: [], pictures: [], model: shown }, 10, lazyLane.state), null, 'loading lane code locates no change');
    for (let i = 0; i < 3; i++) assert.equal(lazyLane.paint({ label: 'lane loading', binding: shown, hour: 10 }).waiting, true, 'a placement waits, as for a loading projection');
    releaseLane(family);
    await settle();
    assert.deepEqual([laneLoads, laneWakes], [1, [1, 2]], 'one import, and each waiting placement woken once');
    assert.ok(lazyLane.paint({ label: 'lane loaded', binding: shown, hour: 10 }).surfaces.length > 0);
    assert.deepEqual(lazyLane.picture(), ['t:0:0=#eef1f6|A 1  1']);

    let releasePrepared: (lane: LaneFamily<Binding, Model, Layout>) => void = () => {};
    const prepared = lazyLaneView('test-lane', () => { laneLoads++; return new Promise(resolve => { releasePrepared = resolve; }); }, () => {});
    let ready = false;
    const preparing = prepared.prepare!().then(() => { ready = true; });
    void prepared.prepare!();
    await settle();
    assert.deepEqual([ready, laneLoads], [false, 2], 'preparing starts the one import and is not ready until it lands');
    releasePrepared(family);
    await preparing;
    const preparedLane = driveLane(prepared, recording.canvases);
    const first = preparedLane.paint({ label: 'prepared', binding: shown, hour: 10 });
    assert.ok(!first.waiting && first.surfaces.length > 0, 'a prepared lane paints on its first ask, without waiting');
    assert.equal(laneLoads, 2);

    const failing = lazyLaneView('test-lane', () => { laneLoads++; return Promise.reject(new Error('lane chunk missing')); }, () => {});
    await failing.prepare!();
    const failed = driveLane(failing, recording.canvases);
    for (let i = 0; i < 3; i++) assert.throws(() => failed.paint({ label: 'lane broken', binding: shown, hour: 10 }), /The test-lane lane did not load: lane chunk missing/);
    assert.equal(laneLoads, 3, 'preparing settles on a failed lane import, and the failure is not retried on every paint');
    assert.equal(failed.paint({ label: 'lane retried', binding: shown, hour: 10, generation: 2 }).surfaces.length, 0, 'a re-bind clears the failure and loads the lane code again');
    assert.equal(laneLoads, 4);
  } finally {
    recording.restore();
  }
});

registerFamilyProjection<FamilyProjection<Model, Options>>({
  kind: 'unbounded',
  family: 'test-lane',
  project: (view: BoundedReadView): Model => ({ hour: view.hour, bucket: view.hour, rows: [{ key: 'A', v: view.hour }], name: '' }),
});

test('a placement unmounted while its projection loads is not woken by the load, and a loading paint waits instead of presenting', async () => {
  let release: (value: unknown) => void = () => {};
  bindFamilyManifest([{ family: 'test-lane', kind: 'slow', loadSpec: () => new Promise(resolve => { release = resolve; }) as never }]);
  const recording = installRecordingCanvas();
  try {
    let wakes = 0;
    const view = createLaneView(family, () => { wakes++; });
    const lane = driveLane(view, recording.canvases);
    const slow = { ...binding(rowsAt([])), kind: 'slow' };
    assert.equal(lane.paint({ label: 'loading', binding: slow, hour: 10 }).waiting, true, 'nothing to present until the import settles');
    view.unmount!(1, lane.state);
    release({ projection: { kind: 'slow', family: 'test-lane', project: (read: BoundedReadView) => ({ hour: read.hour, bucket: 0, rows: [], name: '' }) } });
    await settle();
    assert.equal(wakes, 0, 'the unmounted placement left the registry');
  } finally {
    recording.restore();
  }
});

test('a seek to an hour the held proof cannot locate paints at that hour, through Lane.frame', async () => {
  const flushed = () => new Promise<void>(resolve => setImmediate(resolve));
  const hourly: LaneFamily<Binding, Model, Layout> = { ...family, boundary: (_model, hour) => hour + 1 };
  const recording = installRecordingCanvas();
  try {
    const page = new Page<Bitmap & { content: string }>({ lanes: 1, post: (_lane, m: PageMessage) => { if (m.type === 'frame') lane.frame(m); } });
    const lane: Lane<Bitmap & { content: string }> = new Lane({
      lane: 0, lanes: 1,
      views: () => createLaneView(hourly, () => {}),
      crop: async sources => sources.map(() => ({ content: 'x', width: 1, height: 1, close() {} })),
      present: presentation => page.offer(presentation),
      release: () => {}, fail: () => {}, later: () => {},
    });
    const bound = { ...binding(rowsAt([])), kind: 'unbounded' };
    lane.mount(1, [], 'chart', bound);
    page.mount(1, { route: 1, lane: 0, offset: 0, pin: null, surfaces: [{ install: () => {}, clear: () => {} }] });
    lane.bind(1, { generation: 1, surfaces: [{ width: 320, height: 200, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: Infinity } });
    page.publish(100, { playing: false, seek: true });
    await flushed(); await flushed(); page.frame(); await flushed(); await flushed();
    assert.deepEqual([page.view(1).mode, page.view(1).painted, page.view(1).fields?.mirror], ['picture', 100, ['A 100']], 'the first reply paints the target, not the fourth hour walked from the window start');
  } finally {
    recording.restore();
  }
});

test('across the worker boundary a scroll, a focus change and an art arrival cost no projection, layout or index, an arrival or snapshot the picture does not read redraws nothing, and two placements share one index', () => {
  reset();
  const recording = installRecordingCanvas();
  const original = IdentityIndex.fromSnapshot;
  let built = 0;
  IdentityIndex.fromSnapshot = snapshot => { built++; return original.call(IdentityIndex, snapshot); };
  try {
    const adapter: FamilyHostAdapter<Model, Options, Binding> = { family: 'test-lane', navigation: 'rows' };
    const snapshot: IdentitySnapshot = { scope: { sourceId: 's', revision: 'r', branchId: 'b', content: 1 } };
    const options: Options = { order: 'down', name: 'GER' };
    const sources = {};
    const staticPanels = rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }]);
    const viewport = { width: 320, height: 200, dpr: 1, scrollLeft: 0, scrollTop: 0, zoomWindow: null };
    const context = (change: Partial<FamilyBindingContext<Options>> = {}): FamilyBindingContext<Options> => ({
      kind: 'fixture', view: 'default', options, settings: undefined, sources, extent: null, plan: null, viewport, theme: THEME, reducedMotion: false,
      selectedKey: null, focusedKey: null, expanded: {}, audit: false, auditId: undefined, pauseFlush: 0, ...change,
    });
    const bound = (change?: Partial<FamilyBindingContext<Options>>): Binding => ({ ...familyBinding(adapter, context(change))!, staticPanels, identitySnapshot: snapshot });
    const transport = new LaneTransport();
    const views = [createLaneView(family, () => {}), createLaneView(family, () => {})];
    const lanes = views.map((view, at) => driveLane(view, recording.canvases, undefined, transport, `placement ${at}`));
    const paintBoth = (binding: Binding) => { for (const lane of lanes) lane.paint({ label: 'bind', binding, hour: 12 }); };
    paintBoth(bound());
    assert.deepEqual([counts.project, counts.layout, built], [2, 2, 1], 'two placements of one snapshot share one index, though each was cloned');
    const sent = transport.sent.payloads;
    const steps: [string, Binding, number, number, number][] = [
      ['a scroll', bound({ viewport: { ...viewport, scrollTop: 40 } }), 0, 2, 1],
      ['a focus change', bound({ viewport: { ...viewport, scrollTop: 40 }, focusedKey: 'B' }), 0, 2, 1],
      ['an art arrival', { ...bound({ viewport: { ...viewport, scrollTop: 40 }, focusedKey: 'B' }), artResolutions: { 'sprite:x': { status: 'noArtExpected' } } }, 1, 0, 1],
      ['a new identity snapshot', { ...bound({ viewport: { ...viewport, scrollTop: 40 }, focusedKey: 'B' }), artResolutions: { 'sprite:x': { status: 'noArtExpected' } }, identitySnapshot: { scope: { ...snapshot.scope, content: 2 } } }, 2, 0, 2],
    ];
    let payloads = sent;
    for (const [label, binding, crossed, painted, indexes] of steps) {
      reset();
      paintBoth(binding);
      assert.deepEqual([counts.project, counts.layout, built], [0, 0, indexes], `${label}: no projection, no layout`);
      assert.equal(transport.sent.payloads - payloads, crossed, `${label}: crosses only what changed`);
      payloads = transport.sent.payloads;
      assert.equal(counts.paint, painted, painted ? `${label}: each placement redraws` : `${label}: the picture does not read it, so nothing redraws`);
    }
    const indexed = built;
    for (let hour = 12; hour < 400; hour++) for (const [at, view] of views.entries()) view.next!({ widget: 1, subs: [], pictures: [], model: lanes[at]!.model() }, hour, lanes[at]!.state);
    assert.equal(built, indexed, 'next() never indexes a snapshot');
  } finally {
    IdentityIndex.fromSnapshot = original;
    recording.restore();
  }
});

test('a changed static answer, option or source mapping crosses once and rebuilds; an unchanged one keeps its revision', () => {
  reset();
  const recording = installRecordingCanvas();
  try {
    const transport = new LaneTransport();
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases, undefined, transport);
    const a = { rows: [{ key: 'A', v: 1 }] };
    const b = { rows: [{ key: 'B', v: 2 }] };
    const sources = { main: 3 };
    const bound = (panels: Readonly<Record<string, unknown>>, options: Options = DOWN, mapping: Readonly<Record<string, number>> = sources) =>
      ({ ...binding(panels, options), sources: mapping });
    lane.paint({ label: 'first', binding: bound({ main: a, other: b }), hour: 12 });
    const after = (label: string, next: Binding, projected: number, crossed: number) => {
      reset();
      const sent = transport.sent.payloads;
      lane.paint({ label, binding: next, hour: 12 });
      assert.deepEqual([counts.project, transport.sent.payloads - sent], [projected, crossed], label);
    };
    after('a new bundle with the same answers', bound({ main: a, other: b }), 0, 0);
    after('one changed answer', bound({ main: { rows: [{ key: 'A', v: 9 }] }, other: b }), 1, 1);
    const up: Options = { order: 'up' };
    after('another option', bound({ main: a, other: b }, up), 1, 2);
    after('another source mapping', bound({ main: a, other: b }, up, { main: 4 }), 1, 1);
  } finally {
    recording.restore();
  }
});

registerFamilyProjection<FamilyProjection<Model, Options>>({
  kind: 'windowed',
  family: 'test-lane',
  project(view: BoundedReadView): Model {
    counts.project++;
    const first = view.rowWindow?.first ?? 0;
    return { hour: view.hour, bucket: 0, rows: [{ key: `row ${first}`, v: first }], name: '' };
  },
});

test('a row window change re-projects only a projection that read the window', () => {
  const recording = installRecordingCanvas();
  try {
    const lane = driveLane(createLaneView(family, () => {}), recording.canvases);
    const rows = rowsAt([{ key: 'A', v: 1 }]);
    const at = (first: number) => ({ first, count: 10 });
    lane.paint({ label: 'plain', binding: binding(rows, DOWN, { window: at(0) }), hour: 12 });
    reset();
    lane.paint({ label: 'plain, scrolled', binding: binding(rows, DOWN, { window: at(40) }), hour: 12 });
    assert.equal(counts.project, 0, 'a projection that never read the window is reused');
    const windowed = (first: number) => ({ ...binding(rows, DOWN, { window: at(first) }), kind: 'windowed' });
    lane.paint({ label: 'windowed', binding: windowed(0), hour: 12 });
    reset();
    lane.paint({ label: 'windowed, same window', binding: windowed(0), hour: 12 });
    assert.equal(counts.project, 0);
    lane.paint({ label: 'windowed, scrolled', binding: windowed(40), hour: 12 });
    assert.equal(counts.project, 1, 'a projection that read the window projects the new one');
    assert.deepEqual(lane.picture(), ['t:0:0=#eef1f6|row 40 40  0']);
  } finally {
    recording.restore();
  }
});

const GLIDE_MS = 100;
const gliding: LaneFamily<Binding, Model, Layout> = {
  ...family,
  status: model => (model.rows.length ? 'picture' : { refused: 'Nothing was recorded at this hour' }),
  paint(ctx, frame) {
    ctx.clearRect(0, 0, frame.surface.width, frame.surface.height);
    ctx.fillStyle = frame.theme.text;
    const moving = frame.before !== null && frame.now - frame.changedAt < GLIDE_MS;
    const from = new Map(frame.before?.layout.rows.map(row => [row.key, row.y]) ?? []);
    for (const row of frame.layout.rows) ctx.fillText(row.key, 0, moving ? (from.get(row.key) ?? row.y) : row.y);
    return moving;
  },
};

test('a tooltip on a picture that stands across hours is dated by the hour it is shown at, as a fresh paint dates it', () => {
  assertTooltipDated(() => createLaneView(family, () => {}), { label: 'fixture', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 12 }, 'A');
});

test('a transition continues only from the last drawn placement: a seek, a pin or a refusal between drops it even when the model is reused, and a same-hour tick keeps it', () => {
  assertContinuity(() => createLaneView(gliding, () => {}), {
    from: { label: 'before the swap', binding: binding(rowsAt([{ key: 'A', v: 1 }, { key: 'B', v: 2 }])), hour: 11 },
    refused: { label: 'an unrecorded hour', binding: binding(rowsAt([])), hour: 12 },
    to: { label: 'the swap', binding: binding(rowsAt([{ key: 'B', v: 2 }, { key: 'A', v: 1 }])), hour: 13 },
  });
});

test('a refused frame leaves no layout for the next frame to continue from', () => {
  const recording = installRecordingCanvas();
  try {
    const seen: (number | null)[] = [];
    const remembering: LaneFamily<Binding, Model, Layout> = {
      ...gliding,
      layout(model, binding, surface, theme, measure, hour, step) {
        seen.push(step.previous ? step.previous.hour : null);
        return gliding.layout(model, binding, surface, theme, measure, hour, step);
      },
    };
    const lane = driveLane(createLaneView(remembering, () => {}), recording.canvases);
    lane.paint({ label: 'drawn', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 10, speed: 24 });
    assert.equal(lane.paint({ label: 'refused', binding: binding(rowsAt([])), hour: 11, speed: 24 }).mode, 'refused');
    lane.paint({ label: 'drawn again', binding: binding(rowsAt([{ key: 'A', v: 2 }])), hour: 12, speed: 24 });
    assert.deepEqual(seen, [null, null], 'the frame after a refusal continues from nothing');
  } finally {
    recording.restore();
  }
});

test('a family without a mirror arms no mirror timer when its model changes while playing', () => {
  const set: number[] = [];
  const timers = { set: (_fn: () => void, ms: number) => { set.push(ms); return set.length as unknown as ReturnType<typeof setTimeout>; }, clear: () => {} };
  const recording = installRecordingCanvas();
  try {
    const silent: LaneFamily<Binding, Model, Layout> = { ...family, mirror: null };
    const lane = driveLane(createLaneView(silent, () => {}, { timers }), recording.canvases);
    for (let hour = 12; hour < 60; hour += 10) {
      const painted = lane.paint({ label: `hour ${hour}`, binding: binding(rowsAt([{ key: 'A', v: hour }])), hour, speed: 24, now: 1000 + hour });
      assert.equal(painted.fields.mirror, undefined);
    }
    assert.deepEqual(set, [], 'no timer for a field the family never publishes');
  } finally {
    recording.restore();
  }
});

test('the previous layout is given only while the same placement plays forward, never across a seek or a pause', () => {
  const recording = installRecordingCanvas();
  try {
    const seen: (number | null)[] = [];
    const remembering: LaneFamily<Binding, Model, Layout> = {
      ...family,
      layout(model, binding, surface, theme, measure, hour, step) {
        seen.push(step.previous ? step.previous.hour : null);
        return family.layout(model, binding, surface, theme, measure, hour, step);
      },
    };
    const lane = driveLane(createLaneView(remembering, () => {}), recording.canvases);
    lane.paint({ label: 'first', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 10, speed: 24 });
    lane.paint({ label: 'forward', binding: binding(rowsAt([{ key: 'A', v: 2 }])), hour: 12, speed: 24 });
    lane.paint({ label: 'seek back', binding: binding(rowsAt([{ key: 'A', v: 3 }])), hour: 5, speed: 24 });
    lane.paint({ label: 'paused, same hour', binding: binding(rowsAt([{ key: 'A', v: 4 }])), hour: 5, speed: 0 });
    lane.paint({ label: 'paused jump', binding: binding(rowsAt([{ key: 'A', v: 5 }])), hour: 9, speed: 0 });
    assert.deepEqual(seen, [null, 10, null, null, null], 'only continuous forward playback supplies it');
  } finally {
    recording.restore();
  }
});

test('a drawing that expires at a wall-clock time arms the one timer, even while paused, and redraws when it passes', () => {
  const recording = installRecordingCanvas();
  const set: number[] = [];
  const timers = { set: (_fn: () => void, ms: number) => { set.push(ms); return set.length as unknown as ReturnType<typeof setTimeout>; }, clear: () => {} };
  try {
    let draws = 0;
    const holding: LaneFamily<Binding, Model, Layout> = { ...family, paint: (ctx, frame) => { draws++; family.paint(ctx, frame); return frame.now + 500; } };
    const lane = driveLane(createLaneView(holding, () => {}, { timers }), recording.canvases);
    const bound = binding(rowsAt([{ key: 'A', v: 1 }]));
    lane.paint({ label: 'held', binding: bound, hour: 12, speed: 0 });
    assert.deepEqual([draws, set], [1, [500]], 'one timer for the expiry while paused');
    lane.paint({ label: 'still held', binding: bound, hour: 12, speed: 0 });
    assert.deepEqual([draws, set.length], [1, 1], 'no redraw and no second timer before it passes');
    lane.paint({ label: 'expired', binding: bound, hour: 12, speed: 0, now: 1600 });
    assert.equal(draws, 2, 'the expired drawing is redrawn');
  } finally {
    recording.restore();
  }
});

test('incremental families keep their backing canvas out of destructive transferToImageBitmap', async () => {
  const recording = installRecordingCanvas();
  try {
    const incremental: LaneFamily<Binding, Model, Layout> = { ...family, paint: () => ({ drew: true, next: false }) };
    const view = createLaneView(incremental, () => {});
    const lane = driveLane(view, recording.canvases);
    const painted = lane.paint({ label: 'inc', binding: binding(rowsAt([{ key: 'A', v: 1 }])), hour: 10 });
    const canvas = painted.surfaces[0]!.source as unknown as { width: number; height: number; transferToImageBitmap?: () => unknown };
    let transfers = 0;
    canvas.transferToImageBitmap = () => { transfers++; throw new Error('must not transfer incremental canvas'); };
    let fallbacks = 0;
    const bitmaps = await cropSurfaces([canvas], async source => {
      fallbacks++;
      return { width: source.width, close() {} };
    });
    assert.equal(bitmaps.length, 1);
    assert.equal(transfers, 0, 'incremental family canvas is never detached');
    assert.equal(fallbacks, 1, 'incremental family uses non-destructive fallback snapshot');
  } finally {
    recording.restore();
  }
});

test('a zero-sized surface waits without presenting or looping kept-content refusals until resized', async () => {
  const recording = installRecordingCanvas();
  try {
    let presentations = 0;
    let refusals = 0;
    const view = createLaneView(family, () => {});
    let lane!: Lane<Bitmap>;
    const page = new Page<Bitmap>({
      lanes: 1,
      post: (_lane, m: PageMessage) => {
        if (m.type === 'frame') {
          if (m.refused?.length) refusals += m.refused.length;
          lane.frame(m);
        }
      },
    });
    lane = new Lane<Bitmap>({
      lane: 0,
      lanes: 1,
      clock: () => 0,
      views: () => view,
      crop: async sources => sources.map(s => ({ width: s.width, close() {} })),
      present: presentation => { presentations++; page.offer(presentation); },
      release: () => {},
      fail: reason => { throw new Error(reason); },
      later: () => {},
    });
    const bound = binding(rowsAt([{ key: 'A', v: 1 }]));
    lane.mount(1, [], 'test-lane', bound);
    lane.bind(1, { generation: 1, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.mount(1, { route: 0, lane: 0, owner: 'o1', surfaces: [{ install() {}, clear() {} }] });
    for (let step = 0; step < 4; step++) {
      page.publish(10, { playing: false, seek: 0 });
      await new Promise<void>(r => setImmediate(r));
      page.frame();
    }
    assert.equal(presentations, 0, 'zero-sized surface never presents');
    assert.equal(refusals, 0, 'zero-sized surface never loops kept-content refusals');
    lane.bind(1, { generation: 2, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture', 'resizing to positive dimensions wakes the view and installs');
  } finally {
    recording.restore();
  }
});

test('shrinking a visible widget to zero size withdraws its installed picture, mirror, nav and viewport until resized back', async () => {
  const recording = installRecordingCanvas();
  try {
    let presentations = 0;
    let clears = 0;
    let installs = 0;
    const view = createLaneView(family, () => {});
    let lane!: Lane<Bitmap>;
    const page = new Page<Bitmap>({
      lanes: 1,
      post: (_lane, m: PageMessage) => {
        if (m.type === 'frame') lane.frame(m);
      },
    });
    lane = new Lane<Bitmap>({
      lane: 0,
      lanes: 1,
      clock: () => 0,
      views: () => view,
      crop: async sources => sources.map(s => ({ width: s.width, close() {} })),
      present: presentation => { presentations++; page.offer(presentation); },
      release: () => {},
      fail: reason => { throw new Error(reason); },
      later: () => {},
    });
    const bound = binding(rowsAt([{ key: 'A', v: 1 }]));
    lane.mount(1, [], 'test-lane', bound);
    lane.bind(1, { generation: 1, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.mount(1, { route: 0, lane: 0, owner: 'o1', surfaces: [{ install() { installs++; }, clear() { clears++; } }] });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture');
    assert.ok(page.view(1).fields?.mirror);
    assert.ok(page.view(1).fields?.nav);
    assert.ok(page.view(1).viewport);
    assert.equal(installs, 1);

    const beforeCollapse = presentations;
    lane.bind(1, { generation: 2, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(presentations, beforeCollapse + 1, 'collapsing sends one withdrawal presentation');
    assert.equal(clears, 1, 'the visible canvas is cleared');
    assert.equal(page.view(1).mode, 'reading');
    assert.equal(page.view(1).fields, undefined);
    assert.equal(page.view(1).viewport, undefined);

    for (let step = 0; step < 3; step++) {
      page.publish(10, { playing: false, seek: 0 });
      await new Promise<void>(r => setImmediate(r));
      page.frame();
    }
    assert.equal(presentations, beforeCollapse + 1, 'staying zero-sized emits no further presentations');

    lane.bind(1, { generation: 3, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture');
    assert.equal(installs, 2);
    assert.ok(page.view(1).fields?.mirror);

    lane.bind(1, { generation: 4, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    lane.bind(1, { generation: 5, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.frame();
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'reading', 'a second zero-size bind before commit carries withdrawal forward');
    assert.equal(page.view(1).viewport, undefined);

    lane.bind(1, { generation: 6, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture');

    lane.supply(1, { dated: null });
    lane.bind(1, { generation: 7, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'reading', 'collapsing to zero size after losing data proof still withdraws the old picture');
    assert.equal(page.view(1).viewport, undefined);

    lane.bind(1, { generation: 8, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture');

    lane.bind(1, { generation: 9, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    lane.mount(1, [], 'test-lane', bound);
    lane.bind(1, { generation: 10, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1', subscription: true });
    page.frame();
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'reading', 'a same-widget remount before commit carries withdrawal forward');
    assert.equal(page.view(1).viewport, undefined);
    assert.equal(page.view(1).fields, undefined);

    lane.bind(1, { generation: 11, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'picture');

    lane.supply(1, { dated: null });
    lane.bind(1, { generation: 12, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null });
    page.rebind(1, { owner: 'o1' });
    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    lane.bind(1, { generation: 13, owner: 'o1', surfaces: [{ width: 300, height: 100, dpr: 1 }], offset: 0, pin: null });
    page.rebind(1, { owner: 'o1' });
    page.frame();
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(page.view(1).mode, 'reading', 'expanding before commit after losing data proof still clears the old picture');
    assert.equal(page.view(1).viewport, undefined);
    assert.equal(page.view(1).fields, undefined);

    let holdSiblingCrop = false;
    let siblingEntries = 0;
    const pendingCrops: (() => void)[] = [];
    let siblingLane!: Lane<Bitmap>;
    const siblingPage = new Page<Bitmap>({
      lanes: 1,
      post: (_lane, m: PageMessage) => {
        if (m.type === 'frame') siblingLane.frame(m);
      },
    });
    const plainView: LaneView = {
      paint(input, _state, content): Painted {
        const surface = input.binding.surfaces[0];
        if (!surface || surface.width <= 0 || surface.height <= 0) return { surfaces: [], fields: {}, motion: false, waiting: true };
        return {
          surfaces: [{ source: { width: surface.width, height: surface.height }, content: content() }],
          fields: { nav: { value: [`w${surface.width}`], content: `w${surface.width}` } },
          motion: false,
        };
      },
    };
    siblingLane = new Lane<Bitmap>({
      lane: 0,
      lanes: 1,
      clock: () => 0,
      views: () => plainView,
      crop: sources => new Promise<Bitmap[]>(resolve => {
        const finish = () => resolve(sources.map(s => ({ width: s.width, close() {} })));
        if (holdSiblingCrop) pendingCrops.push(finish);
        else finish();
      }),
      present: presentation => { siblingEntries += presentation.entries.length; siblingPage.offer(presentation); },
      release: () => {},
      fail: reason => { throw new Error(reason); },
      later: () => {},
    });
    siblingLane.mount(1, [], 'plain');
    siblingLane.bind(1, { generation: 1, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    siblingPage.mount(1, { route: 0, lane: 0, owner: 'o1', surfaces: [{ install() {}, clear() {} }] });
    siblingLane.mount(2, [], 'plain');
    siblingLane.bind(2, { generation: 1, owner: 'o2', surfaces: [{ width: 100, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    siblingPage.mount(2, { route: 0, lane: 0, owner: 'o2', surfaces: [{ install() {}, clear() {} }] });
    siblingPage.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    siblingPage.frame();
    assert.equal(siblingPage.view(1).viewport?.width, 200);

    holdSiblingCrop = true;
    siblingLane.bind(2, { generation: 2, owner: 'o2', surfaces: [{ width: 150, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    siblingPage.rebind(2, { owner: 'o2' });
    siblingPage.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));

    const entriesBeforeExpand = siblingEntries;
    siblingLane.bind(1, { generation: 2, owner: 'o1', surfaces: [{ width: 0, height: 0, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    siblingPage.rebind(1, { owner: 'o1' });
    siblingLane.bind(1, { generation: 3, owner: 'o1', surfaces: [{ width: 300, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 9 } });
    siblingPage.rebind(1, { owner: 'o1' });
    holdSiblingCrop = false;
    while (pendingCrops.length) pendingCrops.shift()!();
    await new Promise<void>(r => setImmediate(r));
    siblingPage.frame();
    await new Promise<void>(r => setImmediate(r));
    siblingPage.frame();
    assert.ok(siblingEntries > entriesBeforeExpand, 'binding 3 emitted an entry alongside the preserved binding 2 withdrawal');
    assert.equal(siblingPage.view(1).installedBinding, 3, 'binding 2 withdrawal cleared binding 1 before binding 3 installed');
    assert.equal(siblingPage.view(1).viewport?.width, 300);
    assert.deepEqual(siblingPage.view(1).fields?.nav, ['w300']);
  } finally {
    recording.restore();
  }
});

test('pause and seek restore exact layout inside the same proof without breaking quiet reuse', async () => {
  const recording = installRecordingCanvas();
  try {
    let draws = 0;
    let lastContinuity = -1;
    let markerShown = false;
    let trackingWidths = false;
    let holdingWidths = false;
    let lastHeld = false;
    const layouts: [number, number, boolean][] = [];
    const heldLayout = new WeakSet<Layout>();
    const expiring: LaneFamily<Binding, Model, Layout> = {
      ...family,
      get layoutTracksPlayback() { return trackingWidths ? true : undefined; },
      layout(model, binding, surface, theme, measure, hour, step) {
        layouts.push([hour, step.speed, step.previousLayout != null]);
        const layout = family.layout(model, binding, surface, theme, measure, hour, step);
        if (holdingWidths && step.speed > 0 && step.previousLayout != null) heldLayout.add(layout);
        return layout;
      },
      layoutHeld: layout => heldLayout.has(layout),
      paint(ctx, frame) {
        draws++;
        lastContinuity = frame.continuity;
        lastHeld = heldLayout.has(frame.layout);
        family.paint(ctx, frame);
        if (draws === 1) {
          markerShown = true;
          return frame.now + 500;
        }
        markerShown = false;
        return false;
      },
    };
    const view = createLaneView(expiring, () => {});
    let lane!: Lane<Bitmap>;
    const page = new Page<Bitmap>({
      lanes: 1,
      post: (_lane, m: PageMessage) => {
        if (m.type === 'frame') lane.frame(m);
      },
    });
    lane = new Lane<Bitmap>({
      lane: 0,
      lanes: 1,
      clock: () => 100,
      views: () => view,
      crop: async sources => sources.map(s => ({ width: s.width, close() {} })),
      present: presentation => page.offer(presentation),
      release: () => {},
      fail: reason => { throw new Error(reason); },
      later: () => {},
    });
    const firstBinding = new PayloadSender(message => lane.payloads(message)).acquire(binding(rowsAt([{ key: 'A', v: 1 }])));
    lane.mount(1, [], 'test-lane', firstBinding.wire);
    lane.bind(1, { generation: 1, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.mount(1, { route: 0, lane: 0, owner: 'o1', surfaces: [{ install() {}, clear() {} }] });

    page.publish(10, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.deepEqual([draws, markerShown], [1, true]);
    const initialContinuity = lastContinuity;

    page.publish(12, { playing: false, seek: 0 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(draws, 1, 'same seek within the same proof reuses without repaint');

    page.publish(12, { playing: false, seek: 1 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.deepEqual([draws, markerShown], [2, false], 'a seek inside the same proof repaints and clears the lingering marker');
    assert.ok(lastContinuity > initialContinuity, 'continuity advances across the seek cut');

    page.publish(12, { playing: false, seek: 2 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(draws, 2, 'once no ephemeral marker remains, a seek in the same proof does not force a repaint');

    page.publish(20, { playing: true, seek: 2, speed: 24, step: 10 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(draws, 3, 'continuous forward playback across a bucket boundary repaints');

    page.publish(20, { playing: false, seek: 3 });
    await new Promise<void>(r => setImmediate(r));
    page.frame();
    assert.equal(draws, 3, 'a stationary family returning false after forward playback is not marked ephemeral and reuses across a seek');

    const step = async (hour: number, playing: boolean, seek: number) => {
      page.publish(hour, { playing, seek, speed: playing ? 24 : 0, step: 1 });
      await new Promise<void>(r => setImmediate(r));
      page.frame();
    };
    trackingWidths = true;
    await step(30, true, 3);
    const beforeExactSeek = draws;
    const exactHour = layouts.at(-1)![0];
    await step(exactHour, true, 4);
    assert.equal(draws, beforeExactSeek + 1, 'seeking within an exact table still seeds its next fit');
    assert.equal(lastHeld, false);
    holdingWidths = true;
    await step(40, true, 4);
    assert.ok(lastHeld, 'playing shows a width inherited from the previous layout');
    const beforeSeek = draws;
    const seekHour = layouts.at(-1)![0];
    await step(seekHour, true, 5);
    assert.equal(draws, beforeSeek + 1);
    assert.equal(layouts.at(-1)![0], seekHour, 'the seek stays within the same proof');
    assert.equal(lastHeld, false, 'a seek drops previous widths even inside an unchanged proof');
    await step(50, true, 5);
    assert.ok(lastHeld, JSON.stringify(layouts));
    const beforePause = draws;
    const pauseHour = layouts.at(-1)![0];
    await step(pauseHour, false, 5);
    assert.equal(draws, beforePause + 1);
    assert.equal(layouts.at(-1)![0], pauseHour);
    assert.equal(lastHeld, false, 'pause installs an exact fit without a new hour or binding');
  } finally {
    recording.restore();
  }
});

test('failing to acquire a 2D OffscreenCanvas context reports a named drawing failure instead of looping kept-content refusals', async () => {
  const prev = (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas;
  class NoContextCanvas {
    width: number;
    height: number;
    constructor(width: number, height: number) { this.width = width; this.height = height; }
    getContext() { return null; }
  }
  (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas = NoContextCanvas;
  try {
    const widgetFailures: [number, string][] = [];
    let presentations = 0;
    const view = createLaneView(family, () => {});
    let lane!: Lane<Bitmap>;
    const page = new Page<Bitmap>({
      lanes: 1,
      post: (_lane, m: PageMessage) => {
        if (m.type === 'frame') lane.frame(m);
      },
    });
    lane = new Lane<Bitmap>({
      lane: 0,
      lanes: 1,
      clock: () => 0,
      views: () => view,
      crop: async () => [],
      present: presentation => { presentations++; page.offer(presentation); },
      release: () => {},
      fail: reason => { throw new Error(reason); },
      failWidget: (widget, reason) => { widgetFailures.push([widget, reason]); },
      later: () => {},
    });
    const bound = binding(rowsAt([{ key: 'A', v: 1 }]));
    lane.mount(1, [], 'test-lane', bound);
    lane.bind(1, { generation: 1, owner: 'o1', surfaces: [{ width: 200, height: 100, dpr: 1 }], offset: 0, pin: null, dated: { h: 0, c: 100 } });
    page.mount(1, { route: 0, lane: 0, owner: 'o1', surfaces: [{ install() {}, clear() {} }] });
    for (let step = 0; step < 3; step++) {
      page.publish(10, { playing: false, seek: 0 });
      await new Promise<void>(r => setImmediate(r));
      page.frame();
    }
    assert.equal(widgetFailures.length, 1, 'failure is reported once and halts the widget');
    assert.match(widgetFailures[0]![1], /drawing failed:.*2D canvas context/);
    assert.equal(presentations, 0);
  } finally {
    if (prev === undefined) delete (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas;
    else (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas = prev;
  }
});

test('non-hover frames deduplicate structure, mirror, and audit fields structurally without JSON.stringify', () => {
  const recording = installRecordingCanvas();
  const origStringify = JSON.stringify;
  let stringifyCalls = 0;
  try {
    const lane = driveLane(createLaneView({
      ...family,
      paint: () => false,
      structure: frame => ({ nav: frame.model.rows.map(r => [r.key, BigInt(r.v)]), meta: { bucket: frame.model.bucket } }),
      audit: frame => frame.model.rows.map(r => ({ key: r.key, value: BigInt(r.v) })),
    }, () => {}), recording.canvases);
    JSON.stringify = ((...args: Parameters<typeof JSON.stringify>) => {
      stringifyCalls++;
      return origStringify(...args);
    }) as typeof JSON.stringify;
    const first = lane.paint({ label: 'p1', hour: 10, binding: { ...binding(rowsAt([{ key: 'A', v: 1 }])), audit: true } });
    const same = lane.paint({ label: 'p2', hour: 12, binding: { ...binding(rowsAt([{ key: 'A', v: 1 }])), audit: true } });
    const changed = lane.paint({ label: 'p3', hour: 25, binding: { ...binding(rowsAt([{ key: 'A', v: 2 }])), audit: true } });
    assert.equal(first.fields.nav!.content, same.fields.nav!.content, 'identical nav keeps the same content token');
    assert.equal(first.fields.audit!.content, same.fields.audit!.content, 'identical audit keeps the same content token');
    assert.notEqual(first.fields.nav!.content, changed.fields.nav!.content, 'changed bigint nav produces a new content token');
    assert.notEqual(first.fields.audit!.content, changed.fields.audit!.content, 'changed bigint audit produces a new content token');
    assert.equal(stringifyCalls, 0, 'field deduplication never calls JSON.stringify');
  } finally {
    JSON.stringify = origStringify;
    recording.restore();
  }
});

test('unrelated flag batch and non-country entity settlements reuse the 1,500-country index with zero projections, rebuilds, identity lookups, or flag lookups', async () => {
  const { barsLabelText, countryRowPresentation } = await import('./bars/model.ts');
  reset();
  const recording = installRecordingCanvas();
  const countryCount = 1500;
  const tags = Array.from({ length: countryCount }, (_, i) => `C${String(i).padStart(4, '0')}`);
  let manyProjects = 0;
  registerFamilyProjection<FamilyProjection<Model, Options>>({
    kind: 'fixture-many-countries',
    family: 'test-lane',
    project(view: BoundedReadView): Model {
      manyProjects++;
      let first = '';
      for (let i = 0; i < tags.length; i++) {
        const pres = countryRowPresentation(view, tags[i]!);
        if (i === 0) first = barsLabelText(pres.label);
      }
      return { hour: view.hour, bucket: 0, rows: [{ key: 'A', v: 1 }], name: first };
    },
  });
  const origSetFacts = IdentityIndex.prototype.setFacts;
  const origSetIdentities = IdentityIndex.prototype.setIdentities;
  const origResolveText = IdentityIndex.prototype.resolveText;
  const origGetDrawnFlagByKey = IdentityIndex.prototype.getDrawnFlagByKey;
  let factBuilds = 0;
  let identityBuilds = 0;
  let lookups = 0;
  let flagLookups = 0;
  try {
    const scope = { sourceId: 's', revision: '1', branchId: 'b1', content: 1 };
    const index = new IdentityIndex(scope, [0, 500]);
    index.setFacts({
      branchId: 'b1',
      known: [[0, 500]],
      names: tags.map(tag => `Country ${tag}`),
      reasons: [],
      flags: tags.map(tag => ({ keys: [`${tag}_democratic`, null, null], reasons: [null, null, null] })),
      countries: tags.map((tag, i) => ({ tag, runs: [[0, i, 0x336699, -1, i]] })),
      warnings: [],
    }, true);
    index.setIdentities({
      branchId: 'b1',
      known: [{ firstHour: 0, lastHour: 500 }],
      countries: tags.map((tag, i) => ({ tag, identities: [{ firstHour: 0, lastHour: 500, slot: i + 1, countryId: i + 1 }], unresolved: [] })),
      unknown: [],
      warnings: [],
    });

    IdentityIndex.prototype.setFacts = function (...args) { factBuilds++; return origSetFacts.apply(this, args); };
    IdentityIndex.prototype.setIdentities = function (...args) { identityBuilds++; return origSetIdentities.apply(this, args); };
    IdentityIndex.prototype.resolveText = function (...args) { lookups++; return origResolveText.apply(this, args); };
    IdentityIndex.prototype.getDrawnFlagByKey = function (...args) { flagLookups++; return origGetDrawnFlagByKey.apply(this, args); };

    const transport = new LaneTransport();
    const fetchBitmap = async () => ({ width: 20, height: 13, close() {} }) as unknown as ImageBitmap;
    const lane = driveLane(createLaneView({
      ...family,
      visibleArt: () => ['art://unrelated-bitmap'],
    }, () => {}, { fetchBitmap }), recording.canvases, undefined, transport);
    const staticPanels = rowsAt([{ key: 'A', v: 1 }]);
    const bound = (identitySnapshot: IdentitySnapshot) => binding(staticPanels, DOWN, { kind: 'fixture-many-countries', identitySnapshot });

    lane.paint({ label: 'initial', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(manyProjects, 1);
    assert.equal(factBuilds, 1);
    assert.equal(identityBuilds, 1);
    assert.equal(lookups, countryCount);

    factBuilds = 0;
    identityBuilds = 0;
    lookups = 0;
    flagLookups = 0;
    await new Promise<void>(resolve => setImmediate(resolve));
    lane.paint({ label: 'bitmap settled', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(manyProjects, 1, 'an unrelated bitmap settlement keeps the projection');
    assert.equal(lookups, 0, 'an unrelated successful bitmap decode repeats zero country identity lookups');
    assert.equal(flagLookups, 0, 'an unrelated successful bitmap decode repeats zero flag lookups');

    index.setDrawnFlag('panel', { keys: ['FRA_democratic', null, null], reasons: [null, null, null] }, { url: 'flag://FRA' });
    lane.paint({ label: 'unrelated flag batch', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(manyProjects, 1, 'an unrelated flag batch settlement triggers zero re-projections');
    assert.equal(factBuilds, 0, 'an unrelated flag batch settlement rebuilds zero country facts');
    assert.equal(identityBuilds, 0, 'an unrelated flag batch settlement rebuilds zero country identities');
    assert.equal(lookups, 0, 'an unrelated flag batch settlement repeats zero country identity lookups');
    assert.equal(flagLookups, 0, 'an unrelated flag batch settlement repeats zero flag lookups');

    index.setEntityRuns({ kind: 'state', id: '42' }, [{ firstHour: 0, lastHour: 500, name: 'Brandenburg' }]);
    lane.paint({ label: 'unrelated state entity', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(manyProjects, 1, 'an unrelated entity settlement triggers zero re-projections');
    assert.equal(factBuilds, 0, 'an unrelated entity settlement rebuilds zero country facts');
    assert.equal(identityBuilds, 0, 'an unrelated entity settlement rebuilds zero country identities');
    assert.equal(lookups, 0, 'an unrelated entity settlement repeats zero country identity lookups');
    assert.equal(flagLookups, 0, 'an unrelated entity settlement repeats zero flag lookups even after an earlier bitmap settlement');
    assert.ok(transport.sent.lastIdentity !== null, 'state update arrived as an identity delta');
    assert.ok(!transport.sent.lastIdentity!.changes.set.some(([field]) => field === 'flagVersions'), 'an unrelated state update does not retransmit flagVersions');
    assert.ok(!(transport.sent.lastIdentity!.changes.nested ?? []).some(([field]) => field === 'flagVersions'), 'an unrelated state update sends no flagVersions nested diff');

    const deltasBefore = transport.sent.identities;
    index.setDrawnFlag('panel', { keys: ['C0000_democratic', null, null], reasons: [null, null, null] }, { url: 'flag://C0000' });
    lane.paint({ label: 'read flag settled after unrelated flag', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(transport.sent.identities, deltasBefore + 1, 'read flag update arrived through message.identity delta');
    const flagVersionDelta = (transport.sent.lastIdentity!.changes.nested ?? []).find(([field]) => field === 'flagVersions')?.[1];
    assert.equal(flagVersionDelta?.set.length, 1, 'flag batch delta transmits only the newly settled flag key, not the full history');
    assert.equal(manyProjects, 2, 'a read flag settled after an earlier flag crosses PayloadSender delta and re-projects');

    const { PayloadSender, PayloadStore } = await import('../dashboard/follow/payloads.ts');
    const batchIndex = new IdentityIndex({ sourceId: 's-batch', revision: '1', branchId: 'b1', content: 1 }, [0, 500]);
    const store = new PayloadStore();
    type DeltaMsg = { id: number; base: number; changes: { set: [string, unknown][]; drop: string[]; nested?: [string, { set: [string, unknown][]; drop: string[] }][] } };
    const deltas: { current: DeltaMsg | null } = { current: null };
    const sender = new PayloadSender(message => {
      if (message.identity) deltas.current = message.identity;
      store.apply(message);
    });
    const sendSnap = (snap: IdentitySnapshot, prevIdentity?: number) => {
      deltas.current = null;
      const encoded = sender.acquire({ identitySnapshot: snap }, prevIdentity);
      const resolved = store.resolve(encoded.wire) as { identitySnapshot: IdentitySnapshot };
      const identityId = (encoded.wire as { revisions: { identitySnapshot: number } }).revisions.identitySnapshot;
      return { encoded, identityId, snapshot: resolved.identitySnapshot, delta: deltas.current as DeltaMsg | null };
    };
    let prev = sendSnap(batchIndex.exportSnapshot());
    let workerIndex = IdentityIndex.fromSnapshot(prev.snapshot);
    let totalFlagVersionEntries = 0;
    const batchSize = 32;
    for (let start = 0; start < countryCount; start += batchSize) {
      const end = Math.min(countryCount, start + batchSize);
      for (let i = start; i < end; i++) {
        batchIndex.setDrawnFlag('panel', { keys: [`B${i}_democratic`, null, null], reasons: [null, null, null] }, { url: `flag://B${i}` });
      }
      const next = sendSnap(batchIndex.exportSnapshot(), prev.identityId);
      assert.ok(next.delta !== null, 'flag batch carries an identity delta');
      const setEntry = next.delta!.changes.set.find(([field]) => field === 'flagVersions');
      const nestedEntry = (next.delta!.changes.nested ?? []).find(([field]) => field === 'flagVersions');
      totalFlagVersionEntries += setEntry ? Object.keys(setEntry[1] as Record<string, number>).length : (nestedEntry?.[1].set.length ?? 0);
      workerIndex = IdentityIndex.fromSnapshot(next.snapshot, workerIndex);
      sender.release(prev.encoded.ids);
      prev = next;
    }
    assert.equal(totalFlagVersionEntries, countryCount, '47 batches of 32 flag settlements send exactly 1,500 flagVersions delta entries total');

    batchIndex.setEntityRuns({ kind: 'state', id: '99' }, [{ firstHour: 0, lastHour: 500, name: 'Prussia' }]);
    const stateNext = sendSnap(batchIndex.exportSnapshot(), prev.identityId);
    assert.ok(stateNext.delta !== null, 'unrelated state update carries an identity delta');
    assert.ok(!stateNext.delta!.changes.set.some(([field]) => field === 'flagVersions'), 'unrelated state update sends no flagVersions set');
    assert.ok(!(stateNext.delta!.changes.nested ?? []).some(([field]) => field === 'flagVersions'), 'unrelated state update sends no flagVersions nested diff');
    workerIndex = IdentityIndex.fromSnapshot(stateNext.snapshot, workerIndex);
    sender.release(prev.encoded.ids);
    prev = stateNext;

    const retryChoice = { keys: ['RETRY_democratic', null, null] as [string, string | null, string | null], reasons: [null, null, null] as [string | null, string | null, string | null] };
    for (let i = 0; i < 1000; i++) {
      batchIndex.setDrawnFlag('panel', retryChoice, { url: 'flag://retry' });
      batchIndex.forgetDrawnFlag('panel', retryChoice);
      const retryNext = sendSnap(batchIndex.exportSnapshot(), prev.identityId);
      workerIndex = IdentityIndex.fromSnapshot(retryNext.snapshot, workerIndex);
      sender.release(prev.encoded.ids);
      prev = retryNext;
    }
    const hostLogLen = (batchIndex as unknown as { flagLog: unknown[] }).flagLog.length;
    const workerLogLen = (workerIndex as unknown as { flagLog: unknown[] }).flagLog.length;
    assert.ok(hostLogLen <= 512, `host flagLog stays bounded across 1,000 retries (got ${hostLogLen})`);
    assert.ok(workerLogLen <= 512, `worker flagLog stays bounded across 1,000 retries (got ${workerLogLen})`);
    const changedAfterRetry = workerIndex.flagsChangedSince(workerIndex.flagRevision()! - 1);
    assert.ok(changedAfterRetry !== null && changedAfterRetry.length === 1, 'flagsChangedSince still detects the latest retried handle');

    index.setFacts({
      branchId: 'b1',
      known: [[0, 500]],
      names: tags.map((tag, i) => (i === 0 ? 'Renamed C0000' : `Country ${tag}`)),
      reasons: [],
      flags: tags.map(tag => ({ keys: [`${tag}_democratic`, null, null], reasons: [null, null, null] })),
      countries: tags.map((tag, i) => ({ tag, runs: [[0, i, 0x336699, -1, i]] })),
      warnings: [],
    }, true);
    lane.paint({ label: 'changed country facts', binding: bound(index.exportSnapshot()), hour: 12 });
    assert.equal(manyProjects, 3, 'a real country identity change still re-projects');
  } finally {
    IdentityIndex.prototype.setFacts = origSetFacts;
    IdentityIndex.prototype.setIdentities = origSetIdentities;
    IdentityIndex.prototype.resolveText = origResolveText;
    IdentityIndex.prototype.getDrawnFlagByKey = origGetDrawnFlagByKey;
    recording.restore();
  }
});
