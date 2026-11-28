import assert from 'node:assert/strict';
import { test } from 'node:test';
import { inProcess, managedInProcess, NATIVE } from './inProcess.fixture.ts';
import { datedStore, heldFor, lanesHost } from './useFollowed.ts';
import { datedPanel, datedRecord, message } from './messages.fixture.ts';
import { cellValue, parseAnswer } from './message.ts';
import type { PageSurface } from './page.ts';
import type { LaneView } from '../../draw/lane';
import type { FromLane } from '../../draw/laneMessages';
import { classifyFailure, ShownError } from '../queryError.ts';
import { pipelineFor, prepareLanes, restartPipeline, type LaneWorker } from './pipeline.ts';
import { PlaybackController } from '../playback.ts';
import { configuredStripPage } from '../strip/driver.ts';
import { charge } from './split.ts';
import { measureWith, type PerfRecorder } from '../../shell/perf.ts';

const surface = (): PageSurface<ImageBitmap> & { cleared: number } => ({ cleared: 0, install() {}, clear() { this.cleared++; } });
const acknowledgingLane = (): LaneWorker => ({
  onmessage: null,
  postMessage(message) {
    if (message.type === 'init') message.port.close();
    if (message.type === 'follow' && message.route) {
      const data: FromLane = { type: 'route-bound', sub: message.sub, route: message.route.id, seek: message.route.seek };
      queueMicrotask(() => this.onmessage?.({ data } as MessageEvent<FromLane>));
    }
  },
});

test('a held lane bind cannot start a burst of Occupied states-sized replies', async () => {
  let held!: (dispatch: () => void) => void;
  const binding = new Promise<() => void>(resolve => { held = resolve; });
  let failure!: (reason: string) => void;
  const failed = new Promise<string>(resolve => { failure = resolve; });
  const env = inProcess({
    gateBind: (message, dispatch) => {
      if (message.type !== 'route' && (message.type !== 'follow' || !('route' in message))) return false;
      held(dispatch);
      return true;
    },
    follow: id => {
      const seek = env.clock.getSnapshot().seekSequence;
      const request = env.asked.find(ask => ask.product === 'follow')!;
      const route = Number(request.params.route ?? NATIVE);
      const record = datedPanel(id, 10, { padding: new Uint8Array(209680) }, seek, route);
      assert.equal(record.byteLength - 24, 209712);
      const burst = new Uint8Array(24 + 21 * 209712);
      burst.set(record);
      new DataView(burst.buffer).setUint32(8, 21, true);
      for (let at = 1; at < 21; at++) burst.set(record.subarray(24), 24 + at * 209712);
      env.send(burst);
      env.send(datedRecord(0, 10, 4, '', seek, route));
      return { available: true, route };
    },
  });
  try {
    env.holdBack(message => {
      if (message.type === 'sub-failed') failure(message.reason);
      return false;
    });
    env.clock.seek(10);
    const first = env.pipeline.acquire('occupied|0', { offset: 0, pin: null });
    const query = { panel: 'occupied-states' } as const;
    const id = env.pipeline.subscribe(first, {}, env.source, '1', 0, query, 'panel', 'occupied-states');
    const release = await binding;
    const sent = env.asked.filter(ask => ask.product === 'follow').length;
    assert.equal(sent === 0 ? null : await failed, null, 'the native producer must still be gated');
    const second = env.pipeline.acquire('copy|0', { offset: 0, pin: null });
    assert.equal(env.pipeline.subscribe(second, {}, env.source, '1', 0, query, 'panel', 'occupied-states'), id);
    release();
    for (let turn = 0; turn < 32 && [first, second].some(widget => env.pipeline.view(widget)?.mode !== 'picture'); turn++) {
      await new Promise<void>(resolve => setImmediate(resolve));
      env.pipeline.page.frame();
    }
    assert.deepEqual([first, second].map(widget => env.pipeline.view(widget)?.mode), ['picture', 'picture'],
      JSON.stringify({ asked: env.asked.map(ask => ask.product), status: env.pipeline.status(id!), frames: env.pipeline.page.stats,
        controls: env.posted.map(posted => posted.type) }));
    assert.equal(env.asked.filter(ask => ask.product === 'follow').length, 1);
  } finally { env.close(); }
});

for (const action of ['cancel', 'unmount', 'seek', 'lane error'] as const) {
  test(`a held lane bind retires cleanly on ${action}`, async () => {
    const posted: (() => void)[] = [];
    const env = inProcess({ gateBind: (message, deliver) => {
      if (!posted.length && !(message.type === 'follow' && message.route)) return false;
      posted.push(deliver);
      return true;
    } });
    const turn = () => new Promise<void>(resolve => setImmediate(resolve));
    try {
      env.clock.seek(10);
      const widget = env.pipeline.acquire('waiting|0', { offset: 0, pin: null });
      const token = {}, query = { panel: 'occupied-states' } as const;
      const sub = env.pipeline.subscribe(widget, token, env.source, '1', 0, query, 'panel')!;
      assert.ok(posted.length, 'the lane registration is gated');
      if (action === 'cancel') env.pipeline.unsubscribe(widget, token);
      if (action === 'unmount') env.pipeline.unmount(widget);
      if (action === 'seek') env.clock.seek(11);
      if (action === 'lane error') env.workers[0]!.onerror!({ message: 'worker stopped during bind' } as ErrorEvent);
      if (action === 'seek') {
        posted.shift()!();
        await turn(); await turn();
        assert.equal(env.asked.length, 0, 'the old seek acknowledgement cannot activate the native reader');
      }
      for (const deliver of posted.splice(0)) deliver();
      await turn(); await turn(); await turn();
      const follows = env.asked.filter(ask => ask.product === 'follow');
      assert.equal(follows.length, action === 'seek' ? 1 : 0);
      if (action === 'lane error') assert.match(env.pipeline.view(widget)?.reason ?? '', /Lane 0 failed: worker stopped during bind/);
      if (action === 'unmount') assert.equal(env.pipeline.view(widget), null);
      if (action === 'cancel') assert.equal(env.pipeline.status(sub).status, 'pending');
    } finally { env.close(); }
  });
}

test('a copy joining between the bind acknowledgment and native reply shares the reserved route', async () => {
  let started!: () => void, finish!: () => void;
  const request = new Promise<void>(resolve => { started = resolve; });
  const reply = new Promise<void>(resolve => { finish = resolve; });
  const env = inProcess({ gate: product => { if (product === 'follow') { started(); return reply; } } });
  try {
    const { pipeline, source, clock } = env;
    clock.seek(10);
    const one = pipeline.acquire('first|0', { offset: 0, pin: null });
    const query = { product: 'series-state' } as const;
    const sub = pipeline.subscribe(one, {}, source, '1', 0, query, 'followed')!;
    await request;
    const two = pipeline.acquire('second|0', { offset: 0, pin: null });
    assert.equal(pipeline.subscribe(two, {}, source, '1', 0, query, 'followed'), sub);
    const route = Number(env.asked[0]!.params.route);
    assert.deepEqual([one, two].map(widget => pipeline.page.widgets.get(widget)!.route), [route, route]);
    const ready = (mode: 'picture' | 'reading') => Promise.all([one, two].map(widget => new Promise<void>(resolve => {
      const stop = pipeline.onInstall(widget, () => { if (pipeline.view(widget)?.mode === mode) { stop(); resolve(); } });
    })));
    const installed = ready('picture');
    env.send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, route, clock.getSnapshot().seekSequence));
    await installed;
    const reset = ready('reading');
    env.send(message(10, [], true, route, clock.getSnapshot().seekSequence));
    await reset;
    assert.equal(env.asked.filter(ask => ask.product === 'follow').length, 1);
  } finally { finish(); env.close(); }
});

test('a seek after acknowledgment but before transport open requires a new acknowledgment', async () => {
  let opened!: () => void, acknowledged!: () => void, nextBind!: (deliver: () => void) => void;
  const open = new Promise<void>(resolve => { opened = resolve; });
  const ack = new Promise<void>(resolve => { acknowledged = resolve; });
  const next = new Promise<() => void>(resolve => { nextBind = resolve; });
  let seek = -1;
  const env = inProcess({ open: () => open.then(() => undefined), gateBind: (message, deliver) => {
    if (message.type !== 'follow' || message.route?.seek !== seek) return false;
    nextBind(deliver);
    return true;
  } });
  try {
    env.holdBack(message => { if (message.type === 'route-bound') acknowledged(); return false; });
    env.clock.seek(10);
    const widget = env.pipeline.acquire('occupied|0', { offset: 0, pin: null });
    const sub = env.pipeline.subscribe(widget, {}, env.source, '1', 0, { panel: 'occupied-states' }, 'panel')!;
    await ack;
    env.clock.seek(11);
    seek = env.clock.getSnapshot().seekSequence;
    opened();
    const release = await next;
    assert.equal(env.asked.length, 0);
    release();
    assert.ok(await env.settle(() => env.pipeline.status(sub).status === 'followed'));
    assert.equal(env.asked.filter(ask => ask.product === 'follow').length, 1);
  } finally { opened(); env.close(); }
});

test('a failed panel retires its route and refunds a late whole live-cell answer', async () => {
  const env = managedInProcess();
  try {
    const { pipeline, source, clock } = env;
    clock.seek(10);
    const widget = pipeline.acquire('occupied|0', { offset: 0, pin: null });
    const followed = env.nextEvent('follow-clock');
    const sub = pipeline.subscribe(widget, {}, source, '1', 0, { panel: 'occupied-states' }, 'panel')!;
    await followed;
    const route = pipeline.page.widgets.get(widget)!.route, seek = clock.getSnapshot().seekSequence, cell = 0x80000001;
    env.send(datedPanel(sub, 10, { live: { groups: [{ table: 1, columns: [0], keys: ['1'], follow: cell }],
      rows: ['1'], kinds: [{ id: 'v', op: 'value', scale: 1 }], cells: [0, 0, 1, 0, 0, 0] } }, seek, route));
    env.send(message(10, [{ id: cell, answer: { keys: [1n], columns: 1, values: [2n] } }], false, route, seek));
    await env.waitView(pipeline, widget, view => view?.mode === 'picture');
    const unfollowed = env.nextEvent('unfollow');
    env.send(message(11, [{ id: cell, answer: { keys: [2n], columns: 1, values: [9n], changed: true } }], false, route, seek));
    await unfollowed;
    const late = message(12, [{ id: cell, answer: { keys: [1n], columns: 1, values: [3n] } }], false, route, seek);
    for (let turn = 0; turn < 4; turn++) await new Promise<void>(resolve => setImmediate(resolve));
    const before = env.credit();
    env.send(late);
    for (let turn = 0; turn < 32 && env.credit().credited < before.credited + charge(late); turn++) await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(env.credit().charged - before.charged, charge(late));
    assert.ok(env.credit().credited - before.credited >= charge(late), JSON.stringify([before, env.credit()]));
    assert.equal(pipeline.view(widget)?.mode, 'failed');
  } finally { env.close(); }
});

test('an in-flight clock command keeps only the newest pending anchor, including its seek', async () => {
  let release!: () => void, started!: () => void, latest!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const first = new Promise<void>(resolve => { started = resolve; });
  const second = new Promise<void>(resolve => { latest = resolve; });
  const requests: Record<string, unknown>[] = [];
  const env = managedInProcess({ gate: (product, params) => {
    if (product !== 'follow-clock') return;
    requests.push(params);
    if (requests.length === 1) { started(); return held; }
    latest();
  } });
  try {
    const { clock, pipeline, source } = env;
    const id = pipeline.acquire('queue|0', { offset: 0, pin: null });
    pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed');
    await first;
    for (let hour = 1; hour <= 50; hour++) clock.seek(hour);
    clock.setSpeed(24);
    assert.equal(requests.length, 1, 'there is only one in flight');
    const completed = env.nextEvent('follow-clock');
    release();
    await completed; await second;
    assert.equal(requests.length, 2);
    assert.equal(requests[1]!.hour, 50);
    assert.equal(requests[1]!.speed, 24);
    assert.equal(requests[1]!.seek, clock.getSnapshot().seekSequence);
    assert.equal(requests[1]!.sought, true, 'the speed update keeps the seek whose pending command it replaces');
  } finally { release(); env.close(); }
});

test('closing a pipeline closes every lane channel, though no follow ever opened them', () => {
  const { channels, close } = inProcess({ lanes: 2 });
  assert.deepEqual([...channels.values()], [false, false]);
  close();
  assert.deepEqual([...channels.values()], [true, true], 'a channel left open keeps the page, or a test process, alive');
});

test('astra 8: a child binding its first canvas, a view change and a new array of the same canvas keep the shared picture', async () => {
  const drawn: LaneView = { paint: input => ({ surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 }, content: 'drawn' })), fields: {}, motion: false }) };
  const { pipeline, clock, send, source, settle, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined });
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'), 'the parent installs its picture');
    const canvas = surface();
    pipeline.present(id, 'drawn');
    assert.equal(pipeline.view(id)?.mode, 'picture', 'a new painter holds the installed picture until its own installs');
    pipeline.bind(id, [canvas], { surfaces: [{ width: 1, height: 1, dpr: 1 }] });
    assert.equal(pipeline.view(id)?.mode, 'picture', 'the first canvas has no pixels to withdraw');
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture' && !pipeline.view(id)!.dim), JSON.stringify([pipeline.view(id), pipeline.page.stats, pipeline.page.widgets.get(id)?.binding]));
    pipeline.bind(id, [canvas], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    assert.deepEqual([pipeline.view(id)?.mode, canvas.cleared], ['picture', 0], 'a resize names the same canvas: held dimmed, never cleared');
    assert.equal(pipeline.held(id, 'drawn', () => surface()), pipeline.held(id, 'drawn', () => surface()), 'a remounted child gets the canvas the pipeline holds');
  } finally { close(); }
});

test('review 4 finding 3: a copy joining a registered subscription takes its route at once and so sees its resets', async () => {
  const { pipeline, clock, send, source, settle, close } = inProcess();
  try {
    const query = { product: 'series-state', params: { branchId: '1', reads: 'every' } };
    const one = pipeline.acquire('one|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(one, {}, source, '1', 0, query, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    const two = pipeline.acquire('two|0', { offset: 0, pin: null });
    assert.equal(pipeline.subscribe(two, {}, source, '1', 0, query, 'followed'), sub);
    assert.equal(pipeline.page.widgets.get(two)!.route, pipeline.page.widgets.get(one)!.route, 'the late copy is on the route of the subscription it shares');
    const seek = clock.getSnapshot().seekSequence;
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [5n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(two)?.mode === 'picture'));
    send(message(10, [], true, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(two)?.mode === 'reading'), 'an empty reset withdraws the late copy\'s picture');
  } finally { close(); }
});

test('review 4 finding 14: under audit each lane records the frames it served, and a paused install is sampled when it commits', async () => {
  const { capturePictures } = await import('../pulled.ts');
  const { pipeline, clock, send, source, settle, close } = inProcess();
  try {
    await capturePictures(true);
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const records = await capturePictures(false) as { t: string; lane?: number; seek?: number; head?: number; widget?: string; hour?: number | null }[];
    const frames = records.filter(record => record.t === 'workerFrame');
    assert.ok(frames.some(frame => frame.lane === 0 && frame.seek === clock.getSnapshot().seekSequence && frame.head === 10), JSON.stringify(frames.slice(-3)));
    assert.ok(records.some(record => record.t === 'picture' && record.widget === 'widget' && record.hour === 10), 'the picture installed while paused is sampled, under its widget id');
    const sampled = records.find(record => record.t === 'picture' && record.widget === 'widget' && record.hour === 10) as { entry?: number; parts?: string[]; eligibleHour?: number | null; binding?: number };
    assert.deepEqual([sampled.entry, sampled.parts?.length, sampled.binding], [pipeline.view(id)!.id, Object.keys(pipeline.page.widgets.get(id)!.content).length, pipeline.page.widgets.get(id)!.installed!.binding],
      'review 5 finding 19: the sample names the installed picture, its parts and the binding it was drawn for');
  } finally { close(); }
});

test('review 5 finding 7: a subscription dropped while its channel opens or its follow is answered is never left followed natively', async () => {
  let drop: (() => void) | null = null;
  const { pipeline, source, asked, settle, close } = inProcess({ follow: () => { drop?.(); drop = null; return { available: true }; } });
  try {
    const early = pipeline.acquire('early|0', { offset: 0, pin: null }), earlyToken = {};
    pipeline.subscribe(early, earlyToken, source, '1', 0, { product: 'series-state', params: { n: 1 } }, 'followed');
    pipeline.unsubscribe(early, earlyToken);
    const late = pipeline.acquire('late|0', { offset: 0, pin: null }), lateToken = {};
    const sub = pipeline.subscribe(late, lateToken, source, '1', 0, { product: 'series-state', params: { n: 2 } }, 'followed')!;
    drop = () => pipeline.unsubscribe(late, lateToken);
    await settle(() => asked.some(ask => ask.product === 'unfollow'));
    const ids = (product: string) => asked.filter(ask => ask.product === product).map(ask => ask.params.id);
    assert.deepEqual(ids('follow'), [sub], 'the copy dropped before the channel opened was never followed');
    assert.deepEqual(ids('unfollow'), [sub], 'the copy dropped while its follow was answered is unfollowed when the reply lands');
  } finally { close(); }
});

test('review 5 finding 14: a failed stream stays failed through a resubscription and a new canvas until it recovers', async () => {
  const drawn: LaneView = { paint: input => ({ surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 }, content: `drawn:${input.hour}` })), fields: {}, motion: false }) };
  const { pipeline, clock, send, fail, source, settle, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined });
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const query = { product: 'series-state' };
    const sub = pipeline.subscribe(id, {}, source, '1', 0, query, 'followed')!;
    pipeline.present(id, 'drawn');
    pipeline.bind(id, [surface()], { surfaces: [{ width: 1, height: 1, dpr: 1 }] });
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    fail('The pushed answers stopped');
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'failed'));
    const token = {};
    pipeline.subscribe(id, token, source, '1', 0, { product: 'series-state', params: { again: true } }, 'followed');
    pipeline.unsubscribe(id, token);
    pipeline.bind(id, [surface()], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    await settle(() => false, 20);
    assert.deepEqual([pipeline.view(id)?.mode, pipeline.view(id)?.reason], ['failed', 'The pushed answers stopped'], 'no rebind revives the picture from before the failure');
    clock.seek(12);
    send(message(12, [{ id: sub, answer: { keys: [1n], columns: 1, values: [8n] } }], true, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'), 'a new stream recovers it');
  } finally { close(); }
});

test('identity and statics are supplied apart from the binding: one snapshot per lane, no new generation, and pictures in flight are installed', async () => {
  const drawn: LaneView = { paint: input => ({ surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 },
    content: `snapshot:${JSON.stringify((input.model as { identitySnapshot?: unknown }).identitySnapshot ?? null)}` })), fields: {}, motion: false }) };
  let finishClock: (() => void) | undefined;
  const { pipeline, clock, settle, posted, source, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined,
    gate: product => product === 'follow-clock' ? new Promise<void>(resolve => { finishClock = resolve; }) : undefined });
  try {
    const slow = pipeline.acquire('slow|0', { offset: 0, pin: null });
    const other = pipeline.acquire('other|0', { offset: 0, pin: null });
    for (const id of [slow, other]) {
      pipeline.present(id, 'drawn');
      pipeline.bind(id, [surface()], { surfaces: [{ width: 1, height: 1, dpr: 1 }] }, {}, { family: 'test', kind: 'test' });
      pipeline.supply(id, { dated: { h: 0, c: 100 } });
    }
    clock.seek(10);
    assert.ok(await settle(() => pipeline.view(slow)?.mode === 'picture' && pipeline.view(other)?.mode === 'picture'));
    const installedSeek = pipeline.view(slow)?.gen?.seek;
    clock.seek(11);
    assert.equal(pipeline.view(slow)?.gen?.seek, installedSeek, 'the route-less picture still covers the new seek');
    assert.equal(pipeline.stripSettled(slow), true, 'an installed exact proof can settle the strip gate across that seek');
    const generation = pipeline.binding(slow);
    const installs = pipeline.page.stats.installs;
    const start = posted.length;
    const scope = { branchId: 'one' }, names = Array.from({ length: 100 }, (_, at) => `T${at}`);
    for (let notify = 0; notify < 10; notify++) {
      const snapshot = { scope, version: notify, names };
      pipeline.supply(other, { identity: snapshot });
      pipeline.supply(slow, { identity: snapshot });
      await new Promise(resolve => { setImmediate(resolve); });
    }
    assert.ok(await settle(() => pipeline.page.widgets.get(slow)?.content[0]?.includes('"version":9') === true), JSON.stringify(pipeline.page.widgets.get(slow)?.content));
    assert.equal(pipeline.binding(slow), generation, 'an identity notify from another widget is not a new binding');
    assert.equal(pipeline.page.stats.rejected.binding ?? 0, 0, 'no picture in flight is retired for the binding');
    assert.ok(pipeline.page.stats.installs > installs, 'the pictures drawn meanwhile are installed');
    const sent = posted.slice(start);
    const payloads = sent.map(item => item.message).filter(message => message.type === 'payloads');
    assert.equal(payloads.flatMap(message => message.put ?? []).length, 1);
    assert.deepEqual(payloads.flatMap(message => message.identity ? [message.identity.changes.set] : []),
      Array.from({ length: 9 }, (_, i) => [['version', i + 1]]));
    assert.equal(sent.filter(item => item.type === 'bind').length, 0);
    assert.ok(Math.max(...sent.filter(item => item.type === 'supply').map(item => item.bytes)) < 80, 'a widget names its snapshot; it does not carry a copy');
    const token = {};
    const sub = pipeline.subscribe(slow, token, source, '1', 0, { product: 'series-state' }, 'followed')!;
    assert.ok(await settle(() => pipeline.status(sub).status === 'followed'));
    assert.ok(await settle(() => finishClock !== undefined), 'the old route has an in-flight clock command');
    let stripResets = 0;
    const stopReset = pipeline.onStripReset(slow, () => { stripResets++; });
    pipeline.unsubscribe(slow, token);
    assert.equal(stripResets, 1, 'dropping the last subscription invalidates the old strip');
    stopReset();
    assert.ok(await settle(() => pipeline.view(slow)?.mode === 'picture' && !pipeline.view(slow)!.dim));
    const lastSeek = pipeline.view(slow)?.gen?.seek;
    clock.seek(12);
    assert.equal(pipeline.view(slow)?.gen?.seek, lastSeek, 'the installed interval still covers the seek');
    assert.equal(pipeline.stripSettled(slow), true, 'a stale subscription route cannot stall the strip gate');
    pipeline.unmount(slow);
    pipeline.unmount(other);
    assert.ok(posted.some(item => item.type === 'payloads' && ((item.message as { drop?: unknown[] }).drop?.length ?? 0) > 0), 'a snapshot no widget reads is dropped from the lane');
  } finally { finishClock?.(); close(); }
});

test('with strip building off, unsubscribing the last source retains the follow route', async () => {
  assert.equal(configuredStripPage(() => assert.fail('the shipped switch is off')), null);
  const { pipeline, clock, posted, asked, settle, source, close } = inProcess();
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const token = {};
    const sub = pipeline.subscribe(id, token, source, '1', 0, { product: 'series-state' }, 'followed')!;
    assert.ok(await settle(() => pipeline.status(sub).status === 'followed'));
    const widget = pipeline['widgets'].get(id)!;
    const route = widget.route;
    assert.ok(route);
    const wire = JSON.stringify(posted), requests = JSON.stringify(asked);
    clock.scrub(10);
    clock.scrub(11.5);
    clock.endScrub();
    assert.equal(JSON.stringify(posted), wire);
    assert.equal(JSON.stringify(asked), requests);
    pipeline.unsubscribe(id, token);
    assert.equal(widget.route, route);
  } finally { close(); }
});

test('a scrub frame broadcasts one hour and density to all lanes with no service requests', () => {
  const { pipeline, asked, posted, close } = inProcess({ lanes: 5 });
  try {
    const start = posted.length;
    const frame = { page: 3, frame: 17, hour: 300, density: 8 as const, gap: null };
    pipeline.scrub(frame.page, frame);
    assert.equal(asked.length, 0);
    const items = posted.slice(start);
    assert.equal(items.length, 5);
    assert.equal(new Set(items.map(item => item.lane)).size, 5);
    for (const item of items) assert.deepEqual(item.message, { type: 'scrub', page: 3, frame });
    pipeline.scrub(frame.page, null);
    assert.equal(posted.slice(start + 5).filter(item => item.message.type === 'scrub' && item.message.frame === null).length, 5);
  } finally { close(); }
});

test('review 5 finding 16: each pointer is answered with the row under its own position, never the one whose tooltip is installed', async () => {
  const drawn: LaneView = { paint: input => ({ surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 }, content: 'rows' })), fields: {}, motion: false,
    hit: point => point[0] < 5 ? 'race:A' : 'race:B' }) };
  const { pipeline, clock, send, source, settle, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined });
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    pipeline.present(id, 'drawn');
    pipeline.bind(id, [surface()], { surfaces: [{ width: 10, height: 1, dpr: 1 }] });
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const heard: (string | null)[] = [];
    pipeline.pointer(id, 0, [1, 0]);
    assert.ok(await settle(() => pipeline.inspecting(id) === 'race:A'), 'hovering A inspects A');
    pipeline.pointer(id, 0, [9, 0], key => heard.push(key));
    pipeline.pointer(id, 0, [1, 0]);
    pipeline.pointer(id, 0, [9, 0], key => heard.push(key));
    assert.ok(await settle(() => heard.length === 2));
    assert.deepEqual(heard, ['race:B', 'race:B'], 'a click on B acts on B though A was inspected in between');
  } finally { close(); }
});

test('R3-S7: a hit asked on A and answered after B installs reaches its callback, but never inspects B with A’s key; a page showing nothing misses', async () => {
  const drawn: LaneView = { paint: input => ({ surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 }, content: `h:${input.hour}` })), fields: {}, motion: false,
    hit: () => `row:${input.hour}` }) };
  const { pipeline, clock, send, source, settle, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined });
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    pipeline.present(id, 'drawn');
    pipeline.bind(id, [surface()], { surfaces: [{ width: 10, height: 1, dpr: 1 }] });
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const a = pipeline.view(id)!.id;
    const worker = (pipeline as unknown as { host: { lanes: { onmessage: ((event: MessageEvent) => void) | null }[] } }).host.lanes[0]!;
    const deliver = worker.onmessage!;
    const held: MessageEvent[] = [];
    worker.onmessage = event => { if ((event.data as { type: string }).type === 'hit') held.push(event); else deliver(event); };
    const heard: { key: string | null; installedId?: number }[] = [];
    pipeline.pointer(id, 0, [1, 0], (key, meta) => heard.push({ key, ...(meta?.installedId !== undefined ? { installedId: meta.installedId } : {}) }));
    assert.ok(await settle(() => held.length === 1));
    send(message(12, [{ id: sub, answer: { keys: [1n], columns: 1, values: [8n], changed: true } }], false, NATIVE, clock.getSnapshot().seekSequence));
    clock.seek(12);
    send(message(12, [{ id: sub, answer: { keys: [1n], columns: 1, values: [8n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.id !== a && pipeline.view(id)?.mode === 'picture'), 'B installs while A’s hit is in flight');
    worker.onmessage = deliver;
    for (const event of held) deliver(event);
    assert.deepEqual(heard, [{ key: 'row:10', installedId: a }], 'the stamped click is delivered to its callback with the picture it hit');
    assert.equal(pipeline.inspecting(id), null, 'live inspection is not moved onto B with A’s key');

    pipeline.page.name(id, 'reading');
    const blank: (string | null)[] = [];
    pipeline.pointer(id, 0, [1, 0], key => blank.push(key));
    assert.ok(await settle(() => blank.length === 1));
    assert.deepEqual([blank[0], pipeline.inspecting(id)], [null, null], 'with no picture on the page, the lane’s old picture is not hit');
  } finally { close(); }
});

test('review 6 C: a route nobody follows is sent no clock, so a widget scrolled back in installs on the same seek', async () => {
  const { pipeline, clock, send, source, asked, settle, close } = inProcess();
  try {
    const query = { product: 'series-state' };
    const first = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(first, {}, source, '1', 0, query, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    pipeline.drop('widget|0');
    const before = asked.filter(ask => ask.product === 'follow-clock').length;
    clock.step(1);
    await settle(() => false, 10);
    assert.equal(asked.filter(ask => ask.product === 'follow-clock').length, before, 'no clock command without a follower');
    const again = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const back = pipeline.subscribe(again, {}, source, '1', 0, query, 'followed')!;
    await settle(() => pipeline.status(back).status === 'followed');
    send(message(11, [{ id: back, answer: { keys: [1n], columns: 1, values: [3n] } }], false, NATIVE + 1, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(again)?.mode === 'picture'), JSON.stringify(pipeline.view(again)));
  } finally { close(); }
});

test('review 7 A: a component joining a shown widget keeps its picture, dimmed, until the joined picture installs', async () => {
  const { pipeline, clock, send, source, settle, close } = inProcess();
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const first = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state', params: { n: 1 } }, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(first).status === 'followed');
    send(message(10, [{ id: first, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const second = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state', params: { n: 2 } }, 'followed')!;
    assert.deepEqual([pipeline.view(id)?.mode, pipeline.view(id)?.dim], ['picture', true], 'the shown picture stays, dimmed, while the join is pending');
    await settle(() => pipeline.status(second).status === 'followed');
    send(message(10, [{ id: second, answer: { keys: [1n], columns: 1, values: [8n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture' && pipeline.view(id)?.dim === false), JSON.stringify(pipeline.view(id)));
  } finally { close(); }
});

test('review 7 D: a widget rejoining a route that native serves from a new host installs on the same seek', async () => {
  let route = NATIVE;
  const { pipeline, clock, send, source, settle, close } = inProcess({ follow: () => ({ available: true, route }) });
  try {
    const query = { product: 'series-state' };
    const first = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(first, {}, source, '1', 0, query, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    const seek = clock.getSnapshot().seekSequence;
    for (const hour of [9, 10]) send(message(hour, [{ id: sub, answer: { keys: [1n], columns: 1, values: [BigInt(hour)] } }], true, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(first)?.mode === 'picture' && pipeline.view(first)?.h === 10));
    pipeline.drop('widget|0');
    route = NATIVE + 1;
    const again = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const back = pipeline.subscribe(again, {}, source, '1', 0, query, 'followed')!;
    await settle(() => pipeline.status(back).status === 'followed');
    send(message(10, [{ id: back, answer: { keys: [1n], columns: 1, values: [3n] } }], false, route, seek));
    assert.ok(await settle(() => pipeline.view(again)?.mode === 'picture'), JSON.stringify(pipeline.view(again)));
  } finally { close(); }
});

test('review 7 F: a pinned placement that shows an asked answer stays sampled with the hour and rows it shows', async () => {
  const { capturePictures } = await import('../pulled.ts');
  const { pipeline, clock, close } = inProcess();
  try {
    await capturePictures(true);
    const id = pipeline.acquire('pinned|0', { offset: 0, pin: 40 });
    pipeline.answer(id, -1000, { hour: 40, rows: () => [{ row: 'a', key: 'a', value: '7' }] });
    clock.seek(50);
    const records = await capturePictures(false) as { t: string; placement?: number; owner?: number; surface?: string; hour?: number | null; fixedHour?: number; mode?: string; rows?: unknown[] }[];
    const sampled = records.filter(record => record.t === 'picture' && record.placement === id);
    assert.ok(sampled.length > 0 && sampled.every(record => record.hour === 40 && record.fixedHour === 40 && record.mode === 'picture' && record.rows?.length === 1), JSON.stringify(sampled.slice(-2)));

    await capturePictures(true);
    const table = pipeline.acquire('table|0', { offset: 0, pin: 40, view: 'table' });
    pipeline.audit(table, -1, () => [{ row: 'GER', key: 'GER', value: '1' }]);
    pipeline.answer(table, -1001, { hour: 40, rows: () => [{ row: 'x', key: 'x', value: '1' }] });
    pipeline.answer(table, -1002, { hour: 40, rows: () => [{ row: 'y', key: 'y', value: '2' }] });
    pipeline.answer(table, -1001, null);
    const owned = (await capturePictures(false) as typeof records).filter(record => record.t === 'picture' && record.placement === table);
    const last = owned.slice(-2);
    assert.deepEqual(last.map(record => [record.owner, record.surface]).sort(), [[-1, 'table'], [-1002, 'table']],
      'review 8 5: a changed pinned answer is sampled at once, each component under its own owner and the active renderer, and only the departing one is removed');

    await capturePictures(true);
    const chart = pipeline.acquire('chart|0', { offset: 0, pin: 40 });
    pipeline.present(chart, 'time-chart');
    pipeline.audit(chart, -2, () => [{ row: 'GER', key: 'GER', value: '2' }]);
    pipeline.answer(chart, -1003, { hour: 40, rows: () => [{ row: 'x', key: 'x', value: '2' }] });
    const chartPictures = (await capturePictures(false) as typeof records).filter(record => record.t === 'picture' && record.placement === chart);
    assert.deepEqual(chartPictures.slice(-2).map(record => [record.owner, record.surface]).sort(), [[-1003, 'time-chart'], [-2, 'time-chart']]);
  } finally { close(); }
});

test('review 8 1: every page command carries its session, and a clock command queued before the last follower left never reaches the route it rejoins', async () => {
  let open: (() => void) | null = null;
  const held = new Promise<void>(resolve => { open = resolve; });
  let gated = true;
  const { pipeline, clock, source, asked, settle, close } = inProcess({ session: 'page-2', gate: product => product === 'follow-clock' && gated ? held : undefined });
  try {
    const query = { product: 'series-state' };
    const first = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(first, {}, source, '1', 0, query, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed' && asked.some(ask => ask.product === 'follow-clock'));
    clock.step(1);
    pipeline.drop('widget|0');
    const again = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const back = pipeline.subscribe(again, {}, source, '1', 0, query, 'followed')!;
    await settle(() => pipeline.status(back).status === 'followed');
    gated = false;
    open!();
    await settle(() => false, 20);
    const clocks = asked.filter(ask => ask.product === 'follow-clock');
    assert.equal(clocks.length, 2, 'the first command and the rejoined route\'s own; the one queued for the left route is dropped');
    assert.ok(asked.every(ask => ask.params.session === 'page-2'), 'follow, unfollow and follow-clock all name the page session');
  } finally { close(); }
});

test('review 8 3: a numeric update keeps the structure React reads, while date readers see each painted hour', async () => {
  const { pipeline, clock, send, source, settle, close } = inProcess();
  try {
    const id = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    const seek = clock.getSnapshot().seekSequence;
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [7n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const structure = pipeline.view(id)!.structure, dated = datedStore(pipeline, id);
    let told = 0;
    const stop = dated.subscribe(() => told++);
    assert.equal(dated.read(), 10);
    for (let at = 0; at < 6; at++) clock.step(1);
    send(message(16, [{ id: sub, answer: { keys: [1n], columns: 1, values: [8n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => dated.read() === 16), String(dated.read()));
    assert.ok(told > 0, 'a date reader hears the later paint hour');
    assert.equal(pipeline.view(id)!.structure, structure, 'the value change keeps the structure React commits on');
    send(message(16, [{ id: sub, answer: { keys: [1n, 2n], columns: 1, values: [8n, 9n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(id)!.structure !== structure), 'a new row is a new structure');
    stop();
  } finally { close(); }
});

test('review 9: value-dependent output runs on every install, while React keeps one structure', async () => {
  const { pipeline, clock, send, source, settle, close } = inProcess();
  try {
    const id = pipeline.acquire('arsenal|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    const seek = clock.getSnapshot().seekSequence;
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [2n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture'));
    const totals: bigint[] = [];
    const readTotal = () => {
      if (pipeline.view(id)?.mode !== 'picture') return;
      const parsed = parseAnswer(pipeline.view(id)!.fields!['answer:' + sub] as Uint8Array);
      totals.push(parsed.kind === 'notRecorded' ? -1n : cellValue(parsed, 0) as bigint);
    };
    readTotal();
    const stop = pipeline.onInstall(id, readTotal);
    const structure = pipeline.view(id)!.structure;
    for (let at = 0; at < 2; at++) clock.step(1);
    send(message(12, [{ id: sub, answer: { keys: [1n], columns: 1, values: [3n] } }], false, NATIVE, seek));
    assert.ok(await settle(() => totals.at(-1) === 3n), JSON.stringify(totals.map(String)));
    assert.equal(pipeline.view(id)!.structure, structure, 'the stockpile going from 2 to 3 with the same rows is painted, not committed');
    stop();
    assert.deepEqual([heldFor({ key: 'parent|0', id: 4 }, 'parent|0|detail'), heldFor({ key: 'detail', id: 5 }, 'detail')], [null, 5],
      'R2-S5: a detail never times the id acquired for its parent');
  } finally { close(); }
});

test('review 9: a rejoin keeps its route\'s named failure, and a detail\'s samples name their widget', async () => {
  const { capturePictures } = await import('../pulled.ts');
  const { pipeline, clock, source, settle, fail, close } = inProcess();
  try {
    const query = { product: 'series-state' };
    const first = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(first, {}, source, '1', 0, query, 'followed')!;
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    fail('The followed stream broke');
    assert.ok(await settle(() => pipeline.view(first)?.mode === 'failed'));
    pipeline.drop('widget|0');
    const again = pipeline.acquire('widget|0', { offset: 0, pin: null });
    const back = pipeline.subscribe(again, {}, source, '1', 0, query, 'followed')!;
    await settle(() => pipeline.status(back).status === 'followed');
    assert.deepEqual([pipeline.view(again)?.mode, pipeline.view(again)?.reason], ['failed', 'The followed stream broke'], 'R3-S4');
    await capturePictures(true);
    const detail = pipeline.acquire('card|0|detail', { offset: 0, pin: 40, widget: 'card' });
    pipeline.answer(detail, -1000, { hour: 40, rows: () => [] });
    const named = (await capturePictures(false) as { t: string; placement?: number; widget?: string }[]).filter(record => record.t === 'picture' && record.placement === detail);
    assert.ok(named.length > 0 && named.every(record => record.widget === 'card'), 'R3-S7: ' + JSON.stringify(named.slice(-1)));
  } finally { close(); }
});

test('a single long main-thread frame does not spike speculative step far ahead of nominal frame step', () => {
  const { pipeline, clock, close } = inProcess();
  try {
    clock.setSpeed(1440);
    clock.setPlaying(true);
    const snap = clock.getSnapshot();
    (pipeline as unknown as { published(a: typeof snap, b: typeof snap): void }).published(
      { ...snap, hour: 100, position: 100, playing: true },
      { ...snap, hour: 272, position: 272, playing: true },
    );
    assert.ok(pipeline.page.step <= 36, `step spiked to ${pipeline.page.step} on a 172h frame stall at 1440 h/s / 60 fps`);
  } finally { clock.stop(); close(); }
});

test('flush visits only widgets with date listeners rather than all mounted widgets', () => {
  const { pipeline, clock, close } = inProcess();
  try {
    const ids = Array.from({ length: 114 }, (_, i) => pipeline.mount({ offset: 0, pin: null, key: `w${i}` }));
    const beforeNone = pipeline.stats.dateVisits;
    for (let h = 1; h <= 10; h++) clock.seek(h);
    assert.equal(pipeline.stats.dateVisits - beforeNone, 0, 'zero widgets visited when no date listeners exist');
    let heard = 0;
    const stop1 = pipeline.onDate(ids[0]!, () => heard++);
    const stop2 = pipeline.onDate(ids[1]!, () => heard++);
    const beforeTwo = pipeline.stats.dateVisits;
    clock.seek(20);
    assert.equal(pipeline.stats.dateVisits - beforeTwo, 2, 'only the 2 widgets with date listeners are visited');
    assert.equal(heard, 2);
    stop1(); stop2();
    const beforeAfter = pipeline.stats.dateVisits;
    clock.seek(21);
    assert.equal(pipeline.stats.dateVisits - beforeAfter, 0, 'unsubscribing removes widgets from date watch set');
  } finally { close(); }
});

test('lane placement balances measured widget cost rather than widget count and re-places on remount', () => {
  const { pipeline, close } = inProcess({ lanes: 2 });
  try {
    const heavy = pipeline.mount({ offset: 0, pin: null, key: 'inst-1|0', widget: 'inst-1', kind: 'heavy' });
    const heavyLane = pipeline.lane(heavy);
    const otherLane = 1 - heavyLane;
    (pipeline as unknown as { receive(lane: number, m: unknown): void }).receive(heavyLane, {
      type: 'served', seek: 0, target: 0, exact: false, changed: 1, ms: 9, costs: [[heavy, 9]],
    });
    const cheap1 = pipeline.mount({ offset: 0, pin: null, key: 'inst-c1|0', widget: 'inst-c1', kind: 'cheap' });
    const cheap2 = pipeline.mount({ offset: 0, pin: null, key: 'inst-c2|0', widget: 'inst-c2', kind: 'cheap' });
    const cheap3 = pipeline.mount({ offset: 0, pin: null, key: 'inst-c3|0', widget: 'inst-c3', kind: 'cheap' });
    assert.deepEqual([pipeline.lane(cheap1), pipeline.lane(cheap2), pipeline.lane(cheap3)], [otherLane, otherLane, otherLane],
      'cheap widgets go to the lower-cost lane even when it already holds more widgets by count');
    const secondHeavy = pipeline.mount({ offset: 0, pin: null, key: 'inst-2|0', widget: 'inst-2', kind: 'heavy' });
    assert.equal(pipeline.lane(secondHeavy), otherLane, 'a second instance with a distinct instance ID shares learned kind cost');
    const afterSecondHeavy = pipeline.mount({ offset: 0, pin: null, key: 'inst-c4|0', widget: 'inst-c4', kind: 'cheap' });
    assert.equal(pipeline.lane(afterSecondHeavy), heavyLane, 'secondHeavy charged learned heavy cost onto otherLane so the next widget returns to heavyLane');
    pipeline.unmount(heavy);
    const remountedHeavy = pipeline.mount({ offset: 0, pin: null, key: 'inst-1|0', widget: 'inst-1', kind: 'heavy' });
    assert.equal(pipeline.lane(remountedHeavy), heavyLane, 'remounting after load drops re-places onto the now-lighter lane');
  } finally { close(); }
});

test('sticky lane decode fault fails via typed Failure across seeks, surface remounts and same-sub rebinds, clears on subscription change or restartPipeline', async () => {
  const env = managedInProcess();
  try {
    const initialListeners = env.counts().clockListeners;
    env.clock.seek(10);
    const p1 = env.pipeline;
    const w1 = p1.acquire('w1|0', { offset: 0, pin: null });
    const w2 = p1.acquire('w2|0', { offset: 0, pin: null });
    const tok1 = {}, tok1b = {}, tok2 = {};
    const c1 = env.nextEvent('follow-clock');
    const s1 = p1.subscribe(w1, tok1, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    await c1;
    const f1b = env.nextEvent('follow');
    const s1b = p1.subscribe(w1, tok1b, env.source, '1', 0, { product: 'series-state', params: { k: 2 } }, 'followed')!;
    await f1b;
    const f2 = env.nextEvent('follow');
    const s2 = p1.subscribe(w2, tok2, env.source, '1', 0, { product: 'series-state', params: { k: 3 } }, 'followed')!;
    await f2;
    const seek0 = env.clock.getSnapshot().seekSequence;
    env.send(message(10, [
      { id: s1, answer: { keys: [1n], columns: 1, values: [10n] } },
      { id: s2, answer: { keys: [1n], columns: 1, values: [20n] } },
    ], true, NATIVE, seek0));
    await env.waitView(p1, w2, v => v?.mode === 'picture');
    assert.equal(p1.view(w1)?.mode, 'reading', 'w1 still waiting on s1b');

    env.send(message(15, [
      { id: s1, answer: { keys: [99n], columns: 1, values: [999n], changed: true } },
      { id: s2, answer: { keys: [1n], columns: 1, values: [25n], changed: true } },
    ], false, NATIVE, seek0));
    await env.waitView(p1, w1, v => v?.mode === 'failed');
    assert.equal(p1.status(s1).status, 'failed');
    const err1 = p1.status(s1).error ?? p1.widgetError(w1);
    assert.equal(p1.view(w1)?.mode, 'failed');
    assert.equal(classifyFailure(err1)?.kind, 'retryable');
    assert.match(err1?.message ?? '', /does not hold/);
    assert.equal(p1.view(w2)?.mode, 'picture', 'sibling w2 stays healthy');

    p1.bind(w1, [surface()], { surfaces: [{ width: 1, height: 1, dpr: 1 }] });
    p1.bind(w1, [surface()], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    assert.equal(p1.view(w1)?.mode, 'failed', 'surface remount does not clear a sticky lane fault');

    const extraTok = {};
    const fExtra = env.nextEvent('follow');
    p1.subscribe(w1, extraTok, env.source, '1', 0, { product: 'series-state', params: { k: 4 } }, 'followed');
    await fExtra;
    p1.unsubscribe(w1, extraTok);
    assert.equal(p1.view(w1)?.mode, 'failed', 'rebind on subscription change does not un-stick while the same failed subscription remains');

    const cSeek1 = env.nextEvent('follow-clock');
    env.clock.seek(20);
    await cSeek1;
    const seek1 = env.clock.getSnapshot().seekSequence;
    env.send(message(20, [
      { id: s1, answer: { keys: [1n], columns: 1, values: [100n] } },
      { id: s1b, answer: { keys: [1n], columns: 1, values: [101n] } },
      { id: s2, answer: { keys: [1n], columns: 1, values: [200n] } },
    ], true, NATIVE, seek1));
    await env.waitView(p1, w2, v => v?.mode === 'picture' && v?.h === 20);
    assert.equal(p1.view(w1)?.mode, 'failed', 'sticky lane fault persists across seeks');

    const wSwap = p1.acquire('swap|0', { offset: 0, pin: null });
    const swapTok = {};
    p1.subscribe(wSwap, swapTok, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed');
    assert.equal(p1.view(wSwap)?.mode, 'failed', 'joining an already-failed subscription shows failed immediately');
    const fHealthy = env.nextEvent('follow');
    const sHealthy = p1.subscribe(wSwap, swapTok, env.source, '1', 0, { product: 'series-state', params: { k: 5 } }, 'followed')!;
    await fHealthy;
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(p1.view(wSwap)?.mode, 'reading', 'replacing the failed subscription with a different subscription clears failed');
    env.send(message(20, [{ id: sHealthy, answer: { keys: [1n], columns: 1, values: [55n] } }], false, NATIVE, seek1));
    await env.waitView(p1, wSwap, v => v?.mode === 'picture');

    const restarted = env.nextEvent('restart');
    restartPipeline(p1);
    await restarted;

    const p2 = env.pipeline;
    assert.notEqual(p2, p1, 'Retry restarts into a fresh pipeline');
    assert.equal(p1.view(w1), null, 'old pipeline widgets are disposed');

    const clock2 = env.nextEvent('follow-clock');
    const w1Next = p2.acquire('w1|0', { offset: 0, pin: null });
    const s1Next = p2.subscribe(w1Next, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    await clock2;
    assert.equal(p2.status(s1Next).status, 'followed');
    env.send(message(20, [{ id: s1Next, answer: { keys: [1n], columns: 1, values: [77n] } }], true, NATIVE, env.clock.getSnapshot().seekSequence));
    await env.waitView(p2, w1Next, v => v?.mode === 'picture' && v?.h === 20);
    assert.equal(p1.view(w1), null, 'nothing from the old lane can act after restart');
    assert.deepEqual(env.counts(), {
      openCalls: 2,
      activeReaders: 1,
      activeLanes: 1,
      activeOwnedPorts: 1,
      clockListeners: initialListeners,
    }, 'restart leaves exactly one active reader, lane set, port set and clock listener set');
  } finally { env.close(); }
});

test('widget drawing fault reports via typed widget-failed, sticks across surface remounts, isolates siblings, and recovers on restartPipeline', async () => {
  let shouldThrow = true;
  const flaky: LaneView = {
    paint: input => {
      if (shouldThrow) throw new Error('boom');
      return { surfaces: input.binding.surfaces.map(() => ({ source: { width: 1, height: 1 }, content: `ok:${input.hour}` })), fields: {}, motion: false };
    },
  };
  const env = managedInProcess({ views: kind => kind === 'flaky' ? flaky : undefined });
  try {
    const p1 = env.pipeline;
    const w1 = p1.acquire('flaky|0', { offset: 0, pin: null, view: 'flaky' });
    const w2 = p1.acquire('ok|0', { offset: 0, pin: null });
    const c1 = env.nextEvent('follow-clock');
    const s1 = p1.subscribe(w1, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    const f2 = env.nextEvent('follow');
    const s2 = p1.subscribe(w2, {}, env.source, '1', 0, { product: 'series-state', params: { k: 2 } }, 'followed')!;
    p1.bind(w1, [surface()], { surfaces: [{ width: 1, height: 1, dpr: 1 }] });
    await Promise.all([c1, f2]);
    env.clock.seek(10);
    const seek = env.clock.getSnapshot().seekSequence;
    env.send(message(10, [
      { id: s1, answer: { keys: [1n], columns: 1, values: [1n] } },
      { id: s2, answer: { keys: [1n], columns: 1, values: [2n] } },
    ], true, NATIVE, seek));
    await Promise.all([
      env.waitView(p1, w1, v => v?.mode === 'failed'),
      env.waitView(p1, w2, v => v?.mode === 'picture'),
    ]);
    const err1 = p1.widgetError(w1);
    assert.equal(p1.view(w1)?.mode, 'failed');
    assert.equal(classifyFailure(err1)?.kind, 'retryable');
    assert.match(err1?.message ?? '', /drawing failed: boom/);
    assert.equal(p1.status(s1).status, 'followed', 'a widget drawing fault does not fail the subscription');
    p1.bind(w1, [surface()], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    assert.equal(p1.view(w1)?.mode, 'failed', 'widget drawing fault stays sticky across surface remounts');
    assert.equal(p1.view(w1)?.failureOrigin, 'local', 'widget drawing fault preserves local failureOrigin across remounts');

    shouldThrow = false;
    p1.retryWidget(w1);
    await env.waitView(p1, w1, v => v?.mode === 'picture');
    assert.equal(p1.widgetError(w1), undefined, 'local retryWidget clears widget error without restarting pipeline');

    shouldThrow = true;
    p1.bind(w1, [surface()], { surfaces: [{ width: 3, height: 1, dpr: 1 }] });
    await env.waitView(p1, w1, v => v?.mode === 'failed');

    shouldThrow = false;
    const restarted = env.nextEvent('restart');
    restartPipeline(p1);
    await restarted;
    const p2 = env.pipeline;
    const c2 = env.nextEvent('follow-clock');
    const w1Next = p2.acquire('flaky|0', { offset: 0, pin: null, view: 'flaky' });
    const s1Next = p2.subscribe(w1Next, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    p2.bind(w1Next, [surface()], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    await c2;
    env.send(message(10, [{ id: s1Next, answer: { keys: [1n], columns: 1, values: [3n] } }], true, NATIVE, env.clock.getSnapshot().seekSequence));
    await env.waitView(p2, w1Next, v => v?.mode === 'picture');
  } finally { env.close(); }
});

test('retry on any follow failure restarts the whole pipeline cleanly without leaking readers, lanes, ports or listeners', async () => {
  let openAttempts = 0;
  let followAttempts = 0;
  let clockAttempts = 0;
  let releaseLateFollow: (() => void) | null = null;
  const env = managedInProcess({
    open: async () => {
      openAttempts++;
      if (openAttempts === 1) throw new Error('IPC pipe disconnected');
      return `session-${openAttempts}`;
    },
    gate: (product, params) => {
      if (product === 'follow' && (params.query as { params?: { late?: boolean } })?.params?.late) {
        return new Promise<void>(resolve => { releaseLateFollow = resolve; });
      }
      return undefined;
    },
    follow: (_id, query) => {
      const mode = (query.params as { mode?: string } | undefined)?.mode;
      if (mode === 'refused') return { available: false, reason: 'Table absent in format 141' };
      followAttempts++;
      if (mode === 'fail-once' && followAttempts === 2) throw new Error('Host follow timed out');
      return { available: true, route: mode === 'route2' ? NATIVE + 1 : NATIVE };
    },
    followClock: () => {
      clockAttempts++;
      if (clockAttempts === 2) throw new ShownError('Host stream clock stalled.', 'clock rpc 504');
      return { available: true };
    },
  });
  try {
    const initialListeners = env.counts().clockListeners;
    env.clock.seek(10);

    const p1 = env.pipeline;
    const w1 = p1.acquire('w1|0', { offset: 0, pin: null });
    const s1 = p1.subscribe(w1, {}, env.source, '1', 0, { product: 'series-state' }, 'followed')!;
    await env.waitView(p1, w1, v => v?.mode === 'failed');
    assert.equal(p1.status(s1).status, 'failed');
    assert.equal(p1.status(s1).reason, 'IPC pipe disconnected');
    assert.equal(env.counts().activeReaders, 0);

    const restarted1 = env.nextEvent('restart');
    restartPipeline(p1);
    await restarted1;
    const p2 = env.pipeline;
    assert.notEqual(p2, p1, 'retry after open failure replaces the pipeline');
    const clock2 = env.nextEvent('follow-clock');
    const w2 = p2.acquire('w1|0', { offset: 0, pin: null });
    const s2 = p2.subscribe(w2, {}, env.source, '1', 0, { product: 'series-state' }, 'followed')!;
    await clock2;
    assert.equal(p2.status(s2).status, 'followed');
    assert.equal(env.send(message(10, [{ id: s2, answer: { keys: [1n], columns: 1, values: [9n] } }], true, NATIVE, env.clock.getSnapshot().seekSequence)), 1);
    await env.waitView(p2, w2, v => v?.mode === 'picture');

    const failW = p2.acquire('fail-sub|0', { offset: 0, pin: null });
    const failS = p2.subscribe(failW, {}, env.source, '1', 0, { product: 'series-state', params: { mode: 'fail-once' } }, 'followed')!;
    const refW = p2.acquire('ref-sub|0', { offset: 0, pin: null });
    const refS = p2.subscribe(refW, {}, env.source, '1', 0, { product: 'series-state', params: { mode: 'refused' } }, 'followed')!;
    const lateW = p2.acquire('late-sub|0', { offset: 0, pin: null });
    p2.subscribe(lateW, {}, env.source, '1', 0, { product: 'series-state', params: { late: true } }, 'followed');
    await Promise.all([
      env.waitView(p2, failW, v => v?.mode === 'failed'),
      env.waitView(p2, refW, v => v?.mode === 'refused'),
    ]);
    assert.equal(p2.status(failS).reason, 'Host follow timed out');
    assert.equal(p2.status(refS).status, 'refused');
    assert.equal(p2.status(refS).reason, 'Table absent in format 141');

    const restarted2 = env.nextEvent('restart');
    restartPipeline(p2);
    await restarted2;
    (releaseLateFollow as (() => void) | null)?.();
    const p3 = env.pipeline;
    assert.notEqual(p3, p2, 'retry after follow failure restarts the pipeline');
    const w3 = p3.acquire('clock-fail|0', { offset: 0, pin: null });
    const s3 = p3.subscribe(w3, {}, env.source, '1', 0, { product: 'series-state' }, 'followed')!;
    await env.waitView(p3, w3, v => v?.mode === 'failed');
    const clockErr = p3.status(s3).error ?? p3.widgetError(w3);
    assert.ok(clockErr);
    assert.deepEqual(classifyFailure(clockErr), { kind: 'retryable', reason: 'Host stream clock stalled.', detail: 'clock rpc 504' });

    const restarted3 = env.nextEvent('restart');
    restartPipeline(p3);
    await restarted3;
    const p4 = env.pipeline;
    const clock4 = env.nextEvent('follow-clock');
    const w4a = p4.acquire('route-a|0', { offset: 0, pin: null });
    p4.subscribe(w4a, {}, env.source, '1', 0, { product: 'series-state' }, 'followed');
    await clock4;
    const clock4b = env.nextEvent('follow-clock');
    const w4b = p4.acquire('route-b|24', { offset: 24, pin: null });
    p4.subscribe(w4b, {}, env.source, '1', 24, { product: 'series-state', params: { mode: 'route2' } }, 'followed');
    await clock4b;
    env.fail('The pushed answers stopped');
    await Promise.all([
      env.waitView(p4, w4a, v => v?.mode === 'failed'),
      env.waitView(p4, w4b, v => v?.mode === 'failed'),
    ]);

    const restarted4 = env.nextEvent('restart');
    restartPipeline(p4);
    await restarted4;
    const p5 = env.pipeline;
    const clock5a = env.nextEvent('follow-clock');
    const w5a = p5.acquire('route-a|0', { offset: 0, pin: null });
    const s5a = p5.subscribe(w5a, {}, env.source, '1', 0, { product: 'series-state' }, 'followed')!;
    await clock5a;
    const clock5b = env.nextEvent('follow-clock');
    const w5b = p5.acquire('route-b|24', { offset: 24, pin: null });
    const s5b = p5.subscribe(w5b, {}, env.source, '1', 24, { product: 'series-state', params: { mode: 'route2' } }, 'followed')!;
    await clock5b;
    const pinW = p5.acquire('pinned|0', { offset: 0, pin: 10 });
    const pinS = p5.subscribe(pinW, {}, env.source, '1', 0, { product: 'exact-panel' }, 'pinned', 'panel')!;
    p5.exact(pinS, 10, { ok: true }, null);
    assert.equal(env.send(message(10, [{ id: s5a, answer: { keys: [1n], columns: 1, values: [21n] } }], true, NATIVE, env.clock.getSnapshot().seekSequence)), 1,
      'fresh reader counts resets from zero on each open');
    assert.equal(env.send(message(34, [{ id: s5b, answer: { keys: [1n], columns: 1, values: [22n] } }], true, NATIVE + 1, env.clock.getSnapshot().seekSequence)), 1);
    await Promise.all([
      env.waitView(p5, w5a, v => v?.mode === 'picture'),
      env.waitView(p5, w5b, v => v?.mode === 'picture'),
      env.waitView(p5, pinW, v => v?.mode === 'picture'),
    ]);

    assert.deepEqual(env.counts(), {
      openCalls: 5,
      activeReaders: 1,
      activeLanes: 1,
      activeOwnedPorts: 1,
      clockListeners: initialListeners,
    }, 'after four restarts only one reader, one lane set, one port set and the original clock listener count remain');
  } finally { env.close(); }
});

test('derived failure on a shared widget points at the sibling subscription carrying ShownError', async () => {
  const shared = managedInProcess({
    follow: (_id, query) => {
      if ((query.params as { measure?: string } | undefined)?.measure === 'capacity') {
        throw new ShownError('Capacity stream unavailable right now.', 'cap rpc 503');
      }
      return { available: true };
    },
  });
  try {
    const widget = shared.pipeline.acquire('fuel|0', { offset: 0, pin: null });
    const fuelSub = shared.pipeline.subscribe(widget, {}, shared.source, '1', 0, { product: 'series-state', params: { measure: 'fuel' } }, 'followed')!;
    const capSub = shared.pipeline.subscribe(widget, {}, shared.source, '1', 0, { product: 'series-state', params: { measure: 'capacity' } }, 'followed')!;
    shared.clock.seek(10);
    await shared.waitView(shared.pipeline, widget, v => v?.mode === 'failed');
    assert.equal(shared.pipeline.status(capSub).status, 'failed');
    const widgetErr = shared.pipeline.widgetError(widget);
    assert.deepEqual(classifyFailure(widgetErr), { kind: 'retryable', reason: 'Capacity stream unavailable right now.', detail: 'cap rpc 503' },
      'derived failure on shared widget points at capacity cause');
    assert.equal(shared.pipeline.status(fuelSub).status, 'followed');
  } finally { shared.close(); }
});

test('close aborts in-flight follow requests, terminates a pending reader immediately, and disposes page bitmaps and widgets', async () => {
  let readerClosed = false;
  let resolveOpen!: () => void;
  const openPending = new Promise<void>(resolve => { resolveOpen = resolve; });
  const signals = new Map<string, AbortSignal[]>();
  const clock = new (await import('../playback.ts')).PlaybackController();
  clock.setBounds({ firstHour: 0, lastHour: 100 });
  clock.seek(10);
  const { Pipeline } = await import('./pipeline.ts');
  const client = {
    available: true,
    openFollow: () => ({
      ready: openPending.then(() => 'page1:1'),
      close: () => { readerClosed = true; },
    }),
    product: (req: { product: string }, signal: AbortSignal) => {
      const list = signals.get(req.product) ?? [];
      list.push(signal);
      signals.set(req.product, list);
      return new Promise(() => {});
    },
  } as never;
  const source = { id: 's', revision: 'r', path: 'p', metadata: {} } as never;
  const pendingPipe = new Pipeline(clock, client, {
    lanes: [acknowledgingLane()],
    open: ports => (client as { openFollow(p: readonly MessagePort[]): unknown }).openFollow(ports) as never,
    channel: () => new MessageChannel(),
    frameRate: () => 60,
    rate: () => 1,
  });
  const wPending = pendingPipe.acquire('p|0', { offset: 0, pin: null });
  pendingPipe.subscribe(wPending, {}, source, '1', 0, { product: 'series-state' }, 'followed');
  assert.equal(readerClosed, false);
  pendingPipe.close();
  assert.equal(readerClosed, true, 'closing while open is pending terminates the reader handle immediately');
  resolveOpen();

  const closedBitmaps: number[] = [];
  const makeBitmap = (tag: number) => ({ width: 1, height: 1, close() { closedBitmaps.push(tag); } }) as unknown as ImageBitmap;
  const env = managedInProcess();
  try {
    const p = env.pipeline;
    const w = p.acquire('disp|0', { offset: 0, pin: null });
    p.page.offer({ lane: 0, serial: 1, entries: [{ widget: w, id: 1, binding: 1, gen: { seek: p.page.seek, reset: 0 }, follows: true, h: 10, c: 10, lo: 10, hi: 10, mode: 'picture', surfaces: [makeBitmap(1)], content: {}, fields: {}, ms: 0 }] });
    p.page.offer({ lane: 0, serial: 2, entries: [{ widget: w, id: 2, binding: 1, gen: { seek: p.page.seek, reset: 0 }, follows: true, h: 20, c: 20, lo: 20, hi: 20, mode: 'picture', surfaces: [makeBitmap(2)], content: {}, fields: {}, ms: 0 }] });
    p.page.frame();
    p.page.offer({ lane: 0, serial: 3, entries: [{ widget: w, id: 3, binding: 1, gen: { seek: p.page.seek, reset: 0 }, follows: true, h: 30, c: 30, lo: 30, hi: 30, mode: 'picture', surfaces: [makeBitmap(3)], content: {}, fields: {}, ms: 0 }] });
    p.close();
    assert.deepEqual(closedBitmaps.sort((a, b) => a - b), [1, 2, 3], 'close disposes Page and closes queued, early and earlyMore bitmaps');
    assert.equal(p.page.widgets.size, 0, 'close clears Page widgets');
  } finally { env.close(); }

  let releaseFollow!: () => void;
  const followGate = new Promise<void>(resolve => { releaseFollow = resolve; });
  let notifyFollow!: () => void;
  const followStarted = new Promise<void>(resolve => { notifyFollow = resolve; });
  let notifyUnfollow!: () => void;
  const unfollowStarted = new Promise<void>(resolve => { notifyUnfollow = resolve; });
  const followSignals: AbortSignal[] = [];
  const unfollowSignals: AbortSignal[] = [];
  const abortClient = {
    available: true,
    openFollow: async (ports: readonly MessagePort[]) => {
      for (const p of ports) p.close();
      return 'page1:2';
    },
    product: async (req: { product: string }, signal: AbortSignal) => {
      if (req.product === 'follow') { followSignals.push(signal); notifyFollow(); await followGate; return { data: { available: true, route: NATIVE } }; }
      if (req.product === 'unfollow') { unfollowSignals.push(signal); notifyUnfollow(); return { data: { available: true } }; }
      return { data: { available: true } };
    },
  } as never;
  const abortPipe = new Pipeline(clock, abortClient, {
    lanes: [acknowledgingLane()],
    open: ports => (abortClient as { openFollow(p: readonly MessagePort[]): Promise<string> }).openFollow(ports),
    channel: () => new MessageChannel(),
    frameRate: () => 60,
    rate: () => 1,
  });
  const wAbort = abortPipe.acquire('a|0', { offset: 0, pin: null });
  abortPipe.subscribe(wAbort, {}, source, '1', 0, { product: 'series-state' }, 'followed');
  await followStarted;
  assert.equal(followSignals.length, 1);
  assert.equal(followSignals[0]!.aborted, false);
  abortPipe.close();
  assert.equal(followSignals[0]!.aborted, true, 'close aborts in-flight follow requests');
  releaseFollow();
  await unfollowStarted;
  assert.equal(unfollowSignals.length, 1);
  assert.equal(unfollowSignals[0]!.aborted, false, 'session unfollow cleanup stays independent of pipeline abort');
});

test('a superseded follow-clock rejection never overwrites a route failure cause accepted for a later command', async () => {
  let rejectOldClock!: (err: unknown) => void;
  let clockCall = 0;
  let notifyFirstClock!: () => void;
  const firstClockStarted = new Promise<void>(resolve => { notifyFirstClock = resolve; });
  const env = managedInProcess({
    gate: product => {
      if (product === 'follow-clock') {
        clockCall++;
        if (clockCall === 1) {
          notifyFirstClock();
          return new Promise<void>((_resolve, reject) => { rejectOldClock = reject; });
        }
      }
      return undefined;
    },
    followClock: () => ({ available: true }),
  });
  try {
    env.clock.seek(10);
    const p = env.pipeline;
    const w = p.acquire('r6|0', { offset: 0, pin: null });
    const s = p.subscribe(w, {}, env.source, '1', 0, { product: 'series-state' }, 'followed')!;
    await firstClockStarted;
    const secondClock = env.nextEvent('follow-clock');
    env.clock.seek(20);
    rejectOldClock(new ShownError('Superseded clock error.', 'old clock 504'));
    await secondClock;
    assert.equal(p.status(s).error, undefined, 'superseded clock rejection must not write route.error for a later command');
    env.fail('Current stream failed');
    await env.waitView(p, w, v => v?.mode === 'failed');
    assert.equal(p.view(w)?.mode, 'failed');
    assert.equal((p.status(s).error ?? p.widgetError(w))?.message, 'Current stream failed', 'superseded clock rejection did not overwrite the current failure');
  } finally {
    env.close();
  }
});

test('F6: a late successful registration reply after sub-failed never overwrites failed status and unfollows natively', async () => {
  let releaseFollow!: () => void;
  let notifyFollow!: () => void;
  const followStarted = new Promise<void>(resolve => { notifyFollow = resolve; });
  const env = managedInProcess({
    gate: product => {
      if (product === 'follow') {
        notifyFollow();
        return new Promise<void>(resolve => { releaseFollow = resolve; });
      }
      return undefined;
    },
  });
  try {
    env.clock.seek(10);
    const p = env.pipeline;
    const w = p.acquire('f6|0', { offset: 0, pin: null });
    const s = p.subscribe(w, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    await followStarted;
    env.send(message(10, [{ id: s, answer: { keys: [1n], columns: 1, values: [1n], changed: true } }], false, NATIVE, env.clock.getSnapshot().seekSequence));
    await env.waitView(p, w, v => v?.mode === 'failed');
    assert.equal(p.status(s).status, 'failed');

    const unfollowed = env.nextEvent('unfollow');
    releaseFollow();
    await unfollowed;
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(p.status(s).status, 'failed', 'late follow reply must not overwrite terminal failed status');
    p.bind(w, [surface()], { surfaces: [{ width: 2, height: 1, dpr: 1 }] });
    assert.equal(p.view(w)?.mode, 'failed', 'rebinding after late registration reply keeps widget failed instead of Reading');
  } finally {
    env.close();
  }
});

test('F7: releasing one of two tokens sharing a subscription on the same widget keeps the widget notified on later faults', async () => {
  const env = managedInProcess();
  try {
    env.clock.seek(10);
    const p = env.pipeline;
    const w = p.acquire('f7|0', { offset: 0, pin: null });
    const tokA = {}, tokB = {};
    const c1 = env.nextEvent('follow-clock');
    const sA = p.subscribe(w, tokA, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    const sB = p.subscribe(w, tokB, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    assert.equal(sA, sB);
    await c1;
    p.unsubscribe(w, tokB);
    const seek = env.clock.getSnapshot().seekSequence;
    env.send(message(10, [{ id: sA, answer: { keys: [1n], columns: 1, values: [10n] } }], true, NATIVE, seek));
    await env.waitView(p, w, v => v?.mode === 'picture');
    env.send(message(15, [{ id: sA, answer: { keys: [99n], columns: 1, values: [99n], changed: true } }], false, NATIVE, seek));
    await env.waitView(p, w, v => v?.mode === 'failed');
    assert.equal(p.view(w)?.mode, 'failed');
    assert.match(p.view(w)?.reason ?? '', /does not hold/);
  } finally {
    env.close();
  }
});

test('replacing a mounted PageSurface with a new equal-size PageSurface installs cleanly on the first frame', async () => {
  const drawn: LaneView = {
    paint: input => ({
      surfaces: input.binding.surfaces.map(() => ({ source: { width: 80, height: 40 }, content: `h:${input.hour}` })),
      fields: {},
      motion: false,
    }),
  };
  const { pipeline, clock, send, source, settle, close } = inProcess({ views: kind => kind === 'drawn' ? drawn : undefined });
  try {
    const id = pipeline.acquire('remount-eq|0', { offset: 0, pin: null });
    const sub = pipeline.subscribe(id, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
    pipeline.present(id, 'drawn');
    const first = { installs: 0, install() { this.installs++; }, clear() {} };
    pipeline.bind(id, [first], { surfaces: [{ width: 80, height: 40, dpr: 1 }] });
    clock.seek(10);
    await settle(() => pipeline.status(sub).status === 'followed');
    send(message(10, [{ id: sub, answer: { keys: [1n], columns: 1, values: [42n] } }], false, NATIVE, clock.getSnapshot().seekSequence));
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture' && first.installs === 1));

    const second = { installs: 0, install() { this.installs++; }, clear() {} };
    const laneChannel = (pipeline as unknown as { host: { lanes: { postMessage: (m: unknown) => void }[] } }).host.lanes[0]!;
    const origPost = laneChannel.postMessage.bind(laneChannel);
    const refusedReasons: string[] = [];
    laneChannel.postMessage = (m: unknown) => {
      const msg = m as { type?: string; refused?: { reason?: string }[] };
      if (msg.type === 'frame' && msg.refused?.length) {
        for (const r of msg.refused) if (r.reason) refusedReasons.push(r.reason);
      }
      origPost(m);
    };
    pipeline.bind(id, [second], { surfaces: [{ width: 80, height: 40, dpr: 1 }] });
    pipeline.bind(id, [second], { surfaces: [{ width: 80, height: 40, dpr: 1 }] });
    assert.ok(await settle(() => pipeline.view(id)?.mode === 'picture' && second.installs === 1), 'back-to-back binds on an equal-size replacement surface install a fresh bitmap');
    assert.ok(!refusedReasons.includes('kept content'), `must never refuse kept content after remount, got ${JSON.stringify(refusedReasons)}`);
  } finally {
    close();
  }
});


test('the lanes of every pipeline on a clock are asked for what the dashboard needs, a restarted pipeline is asked again, and a dead lane counts as answered', async () => {
  const clock = new PlaybackController();
  const asked: string[] = [];
  const lanes: (LaneWorker & { onerror: ((event: ErrorEvent) => void) | null })[] = [];
  const lane = (index: number): LaneWorker & { onerror: ((event: ErrorEvent) => void) | null } => {
    const worker = { onmessage: null, onerror: null, postMessage(m: { type: string; ask?: number; families?: readonly string[]; port?: MessagePort }) {
      if (m.type === 'init') m.port!.close();
      if (m.type === 'prepare') asked.push(`${index}:${m.ask}:${m.families!.join(',')}`);
    } } as LaneWorker & { onerror: ((event: ErrorEvent) => void) | null };
    lanes.push(worker);
    return worker;
  };
  const host = () => { const first = lanes.length; return { lanes: [lane(first), lane(first + 1)], open: () => new Promise<never>(() => {}), channel: () => new MessageChannel(), frameRate: () => 60, rate: () => 1 }; };
  prepareLanes(clock, { families: ['table', 'bars'], projections: [['table', 'kind', 'main']] });
  const pipeline = pipelineFor(clock, { available: true, product: () => new Promise(() => {}) } as never, host as never);
  assert.deepEqual(asked, ['0:1:table,bars', '1:1:table,bars'], 'a pipeline made after the dashboard said what it needs is asked as it is made');
  assert.equal(pipeline.prepared(), false);
  lanes[0]!.onmessage!({ data: { type: 'prepared', lane: 0, ask: 1 } } as MessageEvent);
  assert.equal(pipeline.prepared(), false, 'one lane of two has answered');
  const ready = pipeline.whenPrepared();
  lanes[1]!.onerror!({ message: 'chunk missing' } as ErrorEvent);
  await ready;
  assert.equal(pipeline.prepared(), true, 'a lane whose worker never started counts as answered, so nothing waits on it');
  prepareLanes(clock, { families: ['card'], projections: [] });
  assert.deepEqual(asked.slice(2), ['0:2:card'], 'a dead lane is not asked again while the healthy lane still answers each later ask');
  assert.equal(pipeline.prepared(), false);
  lanes[0]!.onmessage!({ data: { type: 'prepared', lane: 0, ask: 2 } } as MessageEvent);
  assert.equal(pipeline.prepared(), true);
  prepareLanes(clock, { families: ['meter'], projections: [] });
  assert.equal(pipeline.prepared(), false);
  const readyAfterCrash = pipeline.whenPrepared();
  lanes[0]!.onerror!({ message: 'late' } as ErrorEvent);
  await readyAfterCrash;
  assert.equal(pipeline.prepared(), true, 'a lane that fails after starting also stops blocking readiness');
  const next = restartPipeline(pipeline)!;
  assert.deepEqual(asked.slice(4), ['2:1:meter', '3:1:meter'], 'the restarted pipeline is asked for what the dashboard needs now');
  prepareLanes(clock, { families: [], projections: [] });
  assert.deepEqual(asked.slice(6), ['2:2:', '3:2:'], 'removing the last family widget is still the latest ask');
  for (const [at, worker] of lanes.slice(2).entries()) worker.onmessage!({ data: { type: 'prepared', lane: at, ask: 2 } } as MessageEvent);
  assert.equal(next.prepared(), true, 'the empty ask is answered, so readiness does not hang on the earlier one');
  next.close();
  assert.equal(next.prepared(), true);
});

test('#1071: lanesHost installs an error handler at worker construction, isolates construction failures, and reports failure at mount when every worker fails', async () => {
  const prevConcurrency = Object.getOwnPropertyDescriptor(globalThis.navigator, 'hardwareConcurrency');
  Object.defineProperty(globalThis.navigator, 'hardwareConcurrency', { configurable: true, value: 4 });
  try {
    let created = 0;
    let prevented = 0;
    const rawWorkers: { onmessage: ((ev: MessageEvent) => void) | null; onerror: ((ev: ErrorEvent) => void) | null; terminated: boolean; postMessage(m: { type: string; port?: MessagePort }): void; terminate(): void }[] = [];
    const client = { available: true, openFollow: async () => 'sess', product: async () => ({ data: { available: true, route: NATIVE } }) } as never;

    const hostPartial = lanesHost(client, () => {
      const idx = created++;
      if (idx === 0) throw new Error('blocked by CSP');
      const raw = {
        onmessage: null as ((ev: MessageEvent) => void) | null,
        onerror: null as ((ev: ErrorEvent) => void) | null,
        terminated: false,
        postMessage(m: { type: string; port?: MessagePort }) { if (m.type === 'init') m.port?.close(); },
        terminate() { this.terminated = true; },
      };
      rawWorkers.push(raw);
      return raw as unknown as Worker;
    });
    assert.equal(hostPartial.laneCapacity, 2);
    assert.equal(rawWorkers.length, 0, 'lanesHost does not construct workers eagerly before a widget mounts');
    const clock = new PlaybackController();
    clock.setBounds({ firstHour: 0, lastHour: 100 });
    clock.seek(10);
    const { Pipeline } = await import('./pipeline.ts');
    const pPartial = new Pipeline(clock, client, hostPartial);
    try {
      const wA = pPartial.acquire('a|0', { offset: 0, pin: null });
      assert.equal(typeof rawWorkers[0]!.onerror, 'function', 'parent error handler is installed immediately at worker construction');
      const wB = pPartial.acquire('b|0', { offset: 0, pin: null });
      assert.deepEqual([pPartial.lane(wA), pPartial.lane(wB)], [1, 1], 'new placements exclude the lane that threw at construction');
      assert.equal(pPartial.view(wA)?.mode, 'reading');
      assert.equal(pPartial.stripSettled(wA), false, 'a widget without a route still waits for its exact picture');
      assert.equal(pPartial.stripLaneAvailable(wA), true);
      rawWorkers[0]!.onerror!({ message: 'script 404', preventDefault() { prevented++; } } as unknown as ErrorEvent);
      assert.equal(prevented, 1, 'error handler handles the worker error event');
      assert.equal(rawWorkers[0]!.terminated, true, 'failed worker is terminated');
      assert.equal(pPartial.view(wA)?.mode, 'failed');
      assert.equal(pPartial.stripLaneAvailable(wA), false, 'a terminated lane can no longer hold a strip');
      assert.match(pPartial.view(wA)?.reason ?? '', /Lane 1 failed: script 404/);
      assert.equal(pPartial.view(wA)?.failureOrigin, 'stream');
    } finally {
      pPartial.close();
    }

    let openCalls = 0;
    let followCalls = 0;
    const trackingClient = {
      available: true,
      openFollow: async () => { openCalls++; return 'sess-all'; },
      product: async (req: { product: string }) => {
        if (req.product === 'follow') followCalls++;
        return { data: { available: true, route: NATIVE } };
      },
    } as never;
    const source = { id: 's', revision: 'r', path: 'p', metadata: {} } as never;
    let failIdx = 0;
    const hostPreError = lanesHost(trackingClient, () => {
      throw new Error(`load error ${failIdx++}`);
    });
    const pAll = pipelineFor(clock, trackingClient, () => hostPreError);
    try {
      assert.equal(pAll.prepared(), true, 'all-dead pipeline does not hang readiness');
      const wMount = pAll.acquire('all-dead|0', { offset: 0, pin: null });
      assert.equal(pAll.view(wMount)?.mode, 'failed', 'when every worker failed, failure is reported immediately at mount');
      assert.match(pAll.view(wMount)?.reason ?? '', /Lane 0 failed: load error 0/);
      assert.equal(pAll.view(wMount)?.failureOrigin, 'stream');
      assert.match(pAll.widgetError(wMount)?.message ?? '', /Lane 0 failed: load error 0/);
      const wOutside = pAll.acquire('all-dead-bounds|0', { offset: 0, pin: null, bounds: [20, 100] });
      assert.equal(pAll.view(wOutside)?.mode, 'failed', 'terminal lane failure takes precedence over branch bounds');
      assert.equal(pAll.view(wOutside)?.reason, pAll.widgetError(wOutside)?.message);
      const sub = pAll.subscribe(wMount, {}, source, '1', 0, { product: 'series-state' }, 'followed')!;
      assert.equal(pAll.status(sub).status, 'failed');
      assert.equal(openCalls, 0, 'dead lane does not open push channel');
      assert.equal(followCalls, 0, 'dead lane does not register native follows');
      pAll.supply(wMount, { statics: { staticPanels: { a: 1 } }, stamp: 5 });
      pAll.bind(wMount, [surface()], { surfaces: [{ width: 10, height: 10, dpr: 1 }] });
      pAll.retime(wMount, { offset: 2 });
      pAll.retryWidget(wMount);
      assert.equal(pAll.view(wMount)?.mode, 'failed', 'all-dead failure stays sticky across supply, bind, retime and local retry');
    } finally {
      pAll.close();
    }
  } finally {
    if (prevConcurrency) Object.defineProperty(globalThis.navigator, 'hardwareConcurrency', prevConcurrency);
    else delete (globalThis.navigator as { hardwareConcurrency?: number }).hardwareConcurrency;
  }
});

test('#1071: a lane failing after starting isolates siblings on the same route, credits held and in-transit bytes, preserves lane reason across route failure, stays sticky, retires follows, excludes dead placements, and recovers on restart', async () => {
  let releasePendingFollow!: () => void;
  let notifyPendingFollow!: () => void;
  const pendingFollowStarted = new Promise<void>(resolve => { notifyPendingFollow = resolve; });
  let failClock = false;
  const asked: { product: string; id: number }[] = [];
  const activeSubs = new Set<number>();
  const unfollowWaiters = new Set<() => void>();
  const waitForUnfollow = (id: number) => new Promise<void>(resolve => {
    if (asked.some(a => a.product === 'unfollow' && a.id === id)) { resolve(); return; }
    const check = () => {
      if (asked.some(a => a.product === 'unfollow' && a.id === id)) { unfollowWaiters.delete(check); resolve(); }
    };
    unfollowWaiters.add(check);
  });

  const env = managedInProcess({
    lanes: 2,
    gate: (product, params) => {
      if (product === 'follow' && (params.query as { params?: { slow?: boolean } })?.params?.slow) {
        notifyPendingFollow();
        return new Promise<void>(resolve => { releasePendingFollow = resolve; });
      }
      return undefined;
    },
    follow: (id) => {
      asked.push({ product: 'follow', id });
      activeSubs.add(id);
      return { available: true, route: NATIVE };
    },
    followClock: () => failClock ? { available: false, reason: 'Clock stalled on route' } : { available: true },
  });
  const origProduct = env.client.product.bind(env.client);
  env.client.product = (async (request: { product: string; params: Record<string, unknown> }, signal: AbortSignal) => {
    const reply = await origProduct(request as never, signal);
    if (request.product === 'unfollow') {
      const id = request.params.id as number;
      asked.push({ product: 'unfollow', id });
      activeSubs.delete(id);
      for (const waiter of [...unfollowWaiters]) waiter();
    }
    return reply;
  }) as typeof env.client.product;

  try {
    env.clock.seek(10);
    const p1 = env.pipeline;
    const w0 = p1.acquire('w0|0', { offset: 0, pin: null });
    const w1 = p1.acquire('w1|0', { offset: 0, pin: null });
    const deadLane = p1.lane(w0);
    const liveLane = p1.lane(w1);
    assert.notEqual(deadLane, liveLane, 'two widgets start on distinct lanes');

    const c0 = env.nextEvent('follow-clock');
    const s0 = p1.subscribe(w0, {}, env.source, '1', 0, { product: 'series-state', params: { k: 0 } }, 'followed')!;
    const f1 = env.nextEvent('follow');
    const s1 = p1.subscribe(w1, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    await Promise.all([c0, f1]);

    const seek0 = env.clock.getSnapshot().seekSequence;
    env.send(message(10, [
      { id: s0, answer: { keys: [1n], columns: 1, values: [10n] } },
      { id: s1, answer: { keys: [1n], columns: 1, values: [20n] } },
    ], true, NATIVE, seek0));
    await Promise.all([
      env.waitView(p1, w0, v => v?.mode === 'picture'),
      env.waitView(p1, w1, v => v?.mode === 'picture'),
    ]);
    assert.ok(env.credit().laneOutstanding(deadLane) > 0, 'the dead lane holds byte credit for its installed record before failing');

    const sPending = p1.subscribe(w0, {}, env.source, '1', 0, { product: 'series-state', params: { slow: true } }, 'followed')!;
    await pendingFollowStarted;

    const hitResults: (string | null)[] = [];
    p1.pointer(w0, 0, [1, 1], key => hitResults.push(key));

    const workers1 = (p1 as unknown as { host: { lanes: LaneWorker[] } }).host.lanes;
    const unfollowS0 = waitForUnfollow(s0);
    workers1[deadLane]!.onerror!({ message: 'worker crashed after start' } as ErrorEvent);
    await unfollowS0;

    const expectedLaneReason = `Lane ${deadLane} failed: worker crashed after start`;
    assert.equal(p1.view(w0)?.mode, 'failed', 'dead lane widget is marked failed by name');
    assert.equal(p1.view(w0)?.reason, expectedLaneReason);
    assert.equal(p1.view(w0)?.failureOrigin, 'stream');
    assert.equal(p1.status(s0).status, 'failed');
    assert.equal(p1.status(sPending).status, 'failed');
    assert.equal(env.credit().laneOutstanding(deadLane), 0, 'outstanding byte credit on the terminated lane is released immediately');
    assert.deepEqual(hitResults, [null], 'pending pointer hit on the dead lane settles instead of hanging');
    assert.equal(p1.widgetError(w0)?.message, expectedLaneReason);

    const unfollowPending = waitForUnfollow(sPending);
    releasePendingFollow();
    await unfollowPending;
    assert.equal(p1.status(sPending).status, 'failed', 'late follow ack on dead lane stays failed');
    assert.deepEqual([...activeSubs], [s1], 'only the healthy lane subscription remains active natively');

    p1.supply(w0, { statics: { staticPanels: { fresh: true } }, stamp: 1 });
    p1.bind(w0, [surface()], { surfaces: [{ width: 20, height: 10, dpr: 1 }] });
    p1.present(w0, 'fields');
    p1.retime(w0, { offset: 0 });
    p1.retryWidget(w0);
    assert.equal(p1.view(w0)?.mode, 'failed', 'stamped statics, rebinds and local retry never clear a dead lane');

    const creditAfterInTransit = env.nextEvent('credit');
    env.clock.seek(12);
    const seek1 = env.clock.getSnapshot().seekSequence;
    env.send(message(12, [
      { id: s0, answer: { keys: [1n], columns: 1, values: [99n] } },
      { id: s1, answer: { keys: [1n], columns: 1, values: [22n] } },
    ], true, NATIVE, seek1));
    await Promise.all([
      creditAfterInTransit,
      env.waitView(p1, w1, v => v?.mode === 'picture' && v?.h === 12),
    ]);
    assert.equal(env.credit().laneOutstanding(deadLane), 0, 'in-transit records for the dead lane are credited immediately without holding shared budget');
    assert.equal(p1.view(w0)?.mode, 'failed', 'dead lane stays failed across seeks while sibling on same route keeps working');

    failClock = true;
    const cFail = env.nextEvent('follow-clock');
    env.clock.seek(13);
    await cFail;
    await env.waitView(p1, w1, v => v?.mode === 'failed');
    assert.equal(p1.view(w1)?.reason, 'Clock stalled on route', 'route failure marks healthy sibling with the route error');
    assert.equal(p1.view(w0)?.reason, expectedLaneReason, 'route failure never overwrites a dead lane widget’s terminal lane error');
    assert.equal(p1.widgetError(w0)?.message, expectedLaneReason, 'view().reason and widgetError() stay in agreement on the dead lane');
    failClock = false;
    const cRecover = env.nextEvent('follow-clock');
    env.clock.seek(14);
    await cRecover;
    const seekRecover = env.clock.getSnapshot().seekSequence;
    env.send(message(14, [
      { id: s1, answer: { keys: [1n], columns: 1, values: [24n] } },
    ], true, NATIVE, seekRecover));
    await env.waitView(p1, w1, v => v?.mode === 'picture' && v?.h === 14);

    p1.drop('w0|0');
    const w0Remount = p1.acquire('w0|0', { offset: 0, pin: null });
    const w2 = p1.acquire('w2|0', { offset: 0, pin: null });
    assert.deepEqual([p1.lane(w0Remount), p1.lane(w2)], [liveLane, liveLane], 'new and remounted placements skip the dead lane');
    const f2 = env.nextEvent('follow');
    const s2 = p1.subscribe(w2, {}, env.source, '1', 0, { product: 'series-state', params: { k: 2 } }, 'followed')!;
    await f2;
    env.send(message(14, [
      { id: s2, answer: { keys: [1n], columns: 1, values: [30n] } },
    ], false, NATIVE, seekRecover));
    await env.waitView(p1, w2, v => v?.mode === 'picture');

    const unfollowS1 = waitForUnfollow(s1);
    const unfollowS2 = waitForUnfollow(s2);
    workers1[liveLane]!.onerror!({ message: '' } as ErrorEvent);
    await Promise.all([unfollowS1, unfollowS2]);
    assert.equal(activeSubs.size, 0, 'all subscriptions retired when every lane has failed');
    assert.equal(env.credit().outstanding, 0, 'all byte credit is returned when every lane has failed');
    assert.equal(env.credit().credited, env.credit().charged, 'every charged byte across all packets was credited back');
    assert.equal(p1.view(w1)?.reason, `Lane ${liveLane} failed`);

    const wAllDead = p1.acquire('w3|0', { offset: 0, pin: null });
    assert.equal(p1.view(wAllDead)?.mode, 'failed', 'mounting when every lane is dead fails immediately at mount');

    const restarted = env.nextEvent('restart');
    restartPipeline(p1);
    await restarted;
    const p2 = env.pipeline;
    assert.notEqual(p2, p1);
    const r0 = p2.acquire('w0|0', { offset: 0, pin: null });
    const r1 = p2.acquire('w1|0', { offset: 0, pin: null });
    assert.notEqual(p2.lane(r0), p2.lane(r1), 'restart restores placement across both lanes');
    const rc0 = env.nextEvent('follow-clock');
    const rs0 = p2.subscribe(r0, {}, env.source, '1', 0, { product: 'series-state', params: { k: 0 } }, 'followed')!;
    const rf1 = env.nextEvent('follow');
    const rs1 = p2.subscribe(r1, {}, env.source, '1', 0, { product: 'series-state', params: { k: 1 } }, 'followed')!;
    await Promise.all([rc0, rf1]);
    const seek2 = env.clock.getSnapshot().seekSequence;
    env.send(message(12, [
      { id: rs0, answer: { keys: [1n], columns: 1, values: [100n] } },
      { id: rs1, answer: { keys: [1n], columns: 1, values: [200n] } },
    ], true, NATIVE, seek2));
    await Promise.all([
      env.waitView(p2, r0, v => v?.mode === 'picture' && v?.h === 12),
      env.waitView(p2, r1, v => v?.mode === 'picture' && v?.h === 12),
    ]);
    assert.equal(p2.widgetError(r0), undefined, 'no retired lane error remains after restart');
    assert.equal(p2.widgetError(r1), undefined);
  } finally {
    env.close();
  }
});


test('worker bar playback reports its own repaints without changing installed picture accounting', () => {
  const env = inProcess();
  const repaints: unknown[][] = [];
  let installed = 4;
  let expired = ['late widget'];
  measureWith({
    barPlayback: (...args: unknown[]) => repaints.push(args),
    pictures: (changed: number, names: string[]) => { installed += changed; expired = names; },
  } as unknown as PerfRecorder);
  try {
    env.pipeline.page.playing = true;
    const diagnostics = { recorded: 1, skipped: { unproven: 1 } };
    env.workers[0]!.onmessage!({
      data: { type: 'served', seek: 1, target: 13, exact: false, changed: 2, ms: 1, motionDiagnostics: diagnostics },
    } as MessageEvent<FromLane>);
    assert.deepEqual(repaints, [[0, 13, 2, 1, diagnostics]]);
    assert.equal(installed, 4);
    assert.deepEqual(expired, ['late widget']);
  } finally {
    measureWith(null);
    env.close();
  }
});
