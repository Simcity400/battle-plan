import type { MotionClock } from '../../families/motion.ts';
import { flushSync } from 'react-dom';
import { engine } from '../../engine/frame.ts';
import type { DashboardClient, DashboardSource, FollowOpen } from '../client';
import type { PlaybackController, PlaybackSnapshot } from '../playback';
import { canonical } from './canonical.ts';
import { Page, type PageMessage, type PageSurface, type RebindChange, type WidgetView } from './page.ts';
import type { Channel } from './facts';
import type { Binding, Dated, Supply } from '../../draw/lane';
import { PayloadSender } from './payloads.ts';
import type { FromLane, LaneSlotsPayload, ProjectionKey, ToLane } from '../../draw/laneMessages';
import { auditing, noteFrame, recordPicture } from '../pulled.ts';
import { perfLog } from '../../shell/perf.ts';
import type { AuditRow } from '../../draw/notifications';
import { productRequest } from '../request.ts';
import type { FollowRequest, GatedParams } from '../regionTime';
import { asFailure, RefusedError, type Failure } from '../queryError.ts';
import type { StripReceipt, StripUpdate } from '../strip/store.ts';
import type { StripFrame } from '../strip/scrub.ts';

export interface LaneWorker {
  postMessage(message: ToLane, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<FromLane>) => void) | null;
  onerror?: ((event: ErrorEvent) => void) | null;
  terminate?(): void;
  readonly failed?: string;
}

export function workerErrorDetail(event: unknown): string | undefined {
  if (event instanceof Error) return event.message.trim() || undefined;
  if (event && typeof event === 'object') {
    const error = (event as { error?: unknown }).error;
    if (error instanceof Error && error.message.trim()) return error.message.trim();
    const message = (event as { message?: unknown }).message;
    if (typeof message === 'string' && message.trim()) return message.trim();
  }
  return undefined;
}

const laneFailureMessage = (lane: number, detail: string | undefined) =>
  detail ? `Lane ${lane} failed: ${detail}` : `Lane ${lane} failed`;

export interface PipelineHost {
  lanes: readonly LaneWorker[];
  readonly laneCapacity?: number;
  createLane?: (index: number) => LaneWorker;
  open(ports: readonly MessagePort[]): FollowOpen;
  channel(): { port1: MessagePort; port2: MessagePort };
  frameRate(): number;
  rate(): number;
  close?(): void;
}

export type SubStatus = 'pending' | 'followed' | 'refused' | 'failed';

interface SubState {
  id: number; key: string; lane: number; channel: Channel; route: RouteState; holders: number; status: SubStatus; reason?: string; error?: Failure;
  widgets: Map<number, number>; released: boolean; waiting?: boolean;
  bind?: (seek: number | null) => void;
}

interface RouteState {
  page: number; native: number | null; key: string; source: DashboardSource; branch: string; offset: number;
  sending: boolean; pending: RouteAnchor | null; last: RouteAnchor | null; told: string | null; error?: { seek: number; failure: Failure };
}

interface RouteAnchor {
  snapshot: PlaybackSnapshot; page: number; command: number; composedAt: number; frameRate: number; initial: boolean;
}

interface WidgetState {
  id: number; lane: number; kind?: string; cost: number; subs: Map<object, number>; view: string; model?: unknown; mounted: boolean; route: RouteState | null;
  stripReset?: () => void;
  installListeners: Set<() => void>; dateListeners: Set<() => void>; playbackHoverListeners?: Set<() => void>;
  binding: Binding; surfaces: readonly PageSurface<ImageBitmap>[];
  held: Map<string, unknown>; error?: Failure; wire?: unknown; stamp: number | undefined; payloads: { own: ReadonlySet<number>; statics: ReadonlySet<number>; identity: ReadonlySet<number> };
}

const NO_PAYLOADS: ReadonlySet<number> = new Set();

export interface SupplyChange {
  readonly stamp?: number;
  readonly statics?: { readonly staticPanels?: unknown; readonly staticAnswers?: unknown; readonly staticShifts?: unknown; readonly tiledBySource?: unknown; readonly hovers?: unknown; readonly pagedRows?: unknown; readonly artResolutions?: unknown } | null;
  readonly identity?: object | null;
  readonly dated?: Dated | null;
}

export interface MountOptions { offset: number; pin: number | null; owner?: string; bounds?: readonly [number, number] | null; view?: string; model?: unknown; key?: string; widget?: string; kind?: string; preferredLane?: number }

export interface PointerHitMeta {
  readonly installedId?: number;
  readonly owner?: string;
}

const NO_CHANNEL = 'This connection has no channel for pushed answers';
const DEFAULT_COST = 0.5;

export class Pipeline {
  private nextStripReceipt = 0;
  private readonly stripReceipts = new Map<number, { lane: number; resolve: (held: StripReceipt) => void; reject: (error: Error) => void }>();

  scrub = (page: number, frame: StripFrame | null) => {
    for (let lane = 0; lane < this.workers.length; lane++) this.postLane(lane, { type: 'scrub', page, frame });
  };

  storeStrip(id: number, build: number, update: StripUpdate): Promise<StripReceipt> {
    const widget = this.widgets.get(id);
    if (this.closed || !widget || this.laneErrors[widget.lane]) return Promise.reject(new Error('Dashboard strip lane is unavailable'));
    const { lane } = widget;
    const sender = this.payloads[lane]!;
    const encoded = update.kind === 'identities' && typeof update.snapshot === 'object'
      ? sender.acquire({ identitySnapshot: update.snapshot }, widget.payloads.identity.values().next().value) : undefined;
    const sent = update.kind === 'identities' && encoded ? { ...update, snapshot: encoded.ids.values().next().value! } : update;
    const receipt = this.nextStripReceipt++;
    const stored = new Promise<StripReceipt>((resolve, reject) => {
      this.stripReceipts.set(receipt, { lane, resolve, reject });
      try { this.postLane(lane, { type: 'strip', widget: id, build, receipt, update: sent }, update.kind === 'append' ? [update.bytes.buffer as ArrayBuffer] : []); }
      catch (error) { this.stripReceipts.delete(receipt); reject(error); }
    });
    return encoded ? stored.finally(() => sender.release(encoded.ids)) : stored;
  }

  stripSettled(id: number): boolean {
    const widget = this.widgets.get(id);
    if (!widget || !this.prepared()) return false;
    const route = widget.subs.size ? widget.route : null;
    if (route?.pending || route?.sending) return false;
    const view = this.page.view(id);
    const interval = view.validInterval;
    const current = route ? view.gen?.seek === this.page.seek : interval && interval[0] <= view.target && view.target <= interval[1];
    return view.mode === 'failed' || view.mode === 'outside' || (view.mode === 'refused' && !view.gen) ||
      (view.mode !== 'reading' && current === true && !view.dim);
  }

  stripLaneAvailable(id: number): boolean {
    const widget = this.widgets.get(id);
    return !!widget && !this.laneErrors[widget.lane];
  }

  onStripReset(id: number, reset: () => void) {
    const widget = this.widgets.get(id);
    if (widget) widget.stripReset = reset;
    return () => { if (widget?.stripReset === reset) widget.stripReset = undefined; };
  }

  private failStrips(error: string, lane?: number) {
    for (const [id, receipt] of this.stripReceipts) if (lane === undefined || lane === receipt.lane) {
      this.stripReceipts.delete(id);
      receipt.reject(new Error(error));
    }
  }
  readonly page: Page<ImageBitmap>;
  readonly stats = { dateVisits: 0 };
  readonly generation: number;
  private readonly host: PipelineHost;
  private readonly clock: PlaybackController;
  private readonly client: DashboardClient;
  private readonly abort = new AbortController();
  private readonly subs = new Map<string, SubState>();
  private readonly byId = new Map<number, SubState>();
  private readonly routes = new Map<string, RouteState>();
  private readonly widgets = new Map<number, WidgetState>();
  private readonly dateWatchers = new Set<WidgetState>();
  private readonly loads: number[];
  private readonly costLoads: number[];
  private readonly kindCost = new Map<string, number>();
  private readonly placed = new Map<string, number>();
  private readonly payloads: PayloadSender[];
  private readonly nextSub: number[];
  private nextWidget = 1;
  private nextRoute = 1;
  private opened: Promise<void> | null = null;
  private readonly ports: readonly MessagePort[];
  private changed = new Set<WidgetState>();
  private redated = new Set<WidgetState>();
  private dated = false;
  private framing = false;
  private closed = false;
  private closeReader: (() => void) | null = null;
  private retireReaderLane: ((lane: number) => void) | null = null;
  private stopPublish: (() => void) | null = null;
  private stopClock: (() => void) | null = null;
  private rate: number;
  private readonly workers: (LaneWorker | null)[];
  private readonly lanePorts: (MessagePort | null)[];
  private activeLaneCount = 0;
  private lastPrepare: { ask: number; families: readonly string[]; projections: readonly ProjectionKey[] } | null = null;
  private prepareAsk = 0;
  private preparedAt: number[];
  private readonly laneErrors: (Failure | undefined)[];
  private motionClock: MotionClock | null = null;
  private readonly preparedWaiters = new Set<() => void>();

  constructor(clock: PlaybackController, client: DashboardClient, host: PipelineHost, generation = 0) {
    this.clock = clock; this.client = client; this.host = host; this.rate = host.rate(); this.generation = generation;
    const count = host.laneCapacity ?? host.lanes.length;
    this.workers = Array.from({ length: count }, () => null);
    this.loads = Array.from({ length: count }, () => 0);
    this.preparedAt = Array.from({ length: count }, () => 0);
    this.laneErrors = Array.from({ length: count }, () => undefined);
    this.costLoads = Array.from({ length: count }, () => 0);
    this.nextSub = Array.from({ length: count }, () => 1);
    this.payloads = Array.from({ length: count }, (_v, index) => new PayloadSender(message => this.postLane(index, message)));
    this.page = new Page<ImageBitmap>({ lanes: count, seek: clock.getSnapshot().seekSequence, post: (lane, message) => this.post(lane, message) });
    const channels = Array.from({ length: count }, () => host.channel());
    this.lanePorts = channels.map(channel => channel.port1);
    this.ports = channels.map(channel => channel.port2);
    host.lanes.forEach((lane, index) => {
      if (index < count) this.attachLane(index, lane);
    });
    if (this.activeLaneCount === 0) perfLog?.lanes(0);
    this.stopPublish = clock.onPublish((previous, next) => this.published(previous, next), true);
    this.stopClock = clock.subscribe(() => this.anchor());
    const s = clock.getSnapshot();
    this.page.publish(s.hour, { playing: s.playing, speed: s.hoursPerSecond, step: 0 });
  }

  private attachLane(index: number, lane: LaneWorker) {
    this.workers[index] = lane;
    this.activeLaneCount++;
    perfLog?.lanes(this.activeLaneCount);
    const port = this.lanePorts[index];
    this.lanePorts[index] = null;
    if (lane.failed !== undefined) {
      const failure = asFailure(new Error(laneFailureMessage(index, lane.failed || undefined)));
      this.laneErrors[index] = failure;
      this.preparedAt[index] = Infinity;
      lane.onmessage = null;
      lane.onerror = null;
      lane.terminate?.();
      port?.close();
      this.page.failLane(index, failure.message);
      this.settlePrepared();
      return;
    }
    lane.onmessage = ({ data }) => this.receive(index, data);
    lane.onerror = event => this.failLane(index, laneFailureMessage(index, workerErrorDetail(event)));
    if (port) lane.postMessage({ type: 'init', lane: index, lanes: this.workers.length, port, rate: this.rate }, [port]);
    if (this.lastPrepare) {
      lane.postMessage({ type: 'prepare', ask: this.lastPrepare.ask, families: this.lastPrepare.families, projections: this.lastPrepare.projections });
    }
    if (this.motionClock) lane.postMessage({ type: 'clock', clock: this.motionClock });
  }

  private ensureLane(lane: number): LaneWorker | null {
    if (this.closed || lane < 0 || lane >= this.workers.length || this.laneErrors[lane]) return null;
    const existing = this.workers[lane];
    if (existing) return existing;
    if (!this.host.createLane) return null;
    const worker = this.host.createLane(lane);
    this.attachLane(lane, worker);
    return this.laneErrors[lane] ? null : worker;
  }

  get lanes() { return this.workers.length; }

  get laneCount() { return this.workers.length; }

  get createdLanes() { return this.activeLaneCount; }

  private sendMotionClock(next: PlaybackSnapshot) {
    const speed = next.playing ? next.hoursPerSecond : 0;
    const previous = this.motionClock;
    if (previous && speed === previous.speed && next.seekSequence === previous.seek
      && next.bounds?.lastHour === previous.lastHour && (next.playing || next.hour === previous.exactHour)) return;
    const clock: MotionClock = {
      playing: next.playing, speed, seek: next.seekSequence, exactHour: next.hour,
      anchorPosition: next.position,
      anchorTimeMs: performance.timeOrigin + (next.composedAt ?? performance.now()),
      ...(next.bounds ? { lastHour: next.bounds.lastHour } : {}),
      ...(perfLog ? { diagnostics: true } : {}),
    };
    this.motionClock = clock;
    for (let lane = 0; lane < this.workers.length; lane++) this.postLane(lane, { type: 'clock', clock });
  }

  transferCanvas(id: number, canvas: OffscreenCanvas, opaque: boolean) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    this.postLane(widget.lane, { type: 'canvas', widget: id, canvas, opaque }, [canvas]);
  }

  clearCanvas(id: number) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    this.postLane(widget.lane, { type: 'clear-canvas', widget: id });
  }

  useCanvas(id: number) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    this.postLane(widget.lane, { type: 'use-canvas', widget: id });
  }

  private readonly laneSurfaceFailedListeners = new Set<(lane: number) => void>();

  onLaneSurfaceFailed(listener: (lane: number) => void): () => void {
    this.laneSurfaceFailedListeners.add(listener);
    return () => { this.laneSurfaceFailedListeners.delete(listener); };
  }

  transferLaneSurface(lane: number, canvas: OffscreenCanvas, opaque: boolean) {
    if (this.closed || lane < 0 || lane >= this.workers.length) return;
    this.ensureLane(lane);
    this.postLane(lane, { type: 'lane-surface', canvas, opaque }, [canvas]);
  }

  sendLaneSlots(lane: number, payload: LaneSlotsPayload) {
    if (this.closed || lane < 0 || lane >= this.workers.length) return;
    this.postLane(lane, { type: 'lane-slots', ...payload });
  }

  disposeLaneSurface(lane: number) {
    if (this.closed || lane < 0 || lane >= this.workers.length) return;
    this.postLane(lane, { type: 'lane-surface-dispose' });
  }

  prepare(families: readonly string[], projections: readonly ProjectionKey[]) {
    if (this.closed) return;
    const ask = ++this.prepareAsk;
    this.lastPrepare = { ask, families, projections };
    for (let lane = 0; lane < this.workers.length; lane++) {
      if (this.workers[lane]) this.postLane(lane, { type: 'prepare', ask, families, projections });
    }
    this.settlePrepared();
  }

  prepared(): boolean {
    return this.closed || this.workers.every((worker, lane) => !worker || this.preparedAt[lane]! >= this.prepareAsk);
  }

  whenPrepared(): Promise<void> {
    return this.prepared() ? Promise.resolve() : new Promise(resolve => { this.preparedWaiters.add(resolve); });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.abort.abort();
    this.stopPublish?.(); this.stopPublish = null;
    this.stopClock?.(); this.stopClock = null;
    for (const lane of this.workers) { if (lane) { lane.onmessage = null; lane.onerror = null; } }
    for (const port of this.lanePorts) port?.close();
    this.lanePorts.fill(null);
    this.closeReader?.(); this.closeReader = null;
    this.retireReaderLane = null;
    this.host.close?.();
    for (const port of this.ports) port.close();
    this.page.dispose();
    this.failStrips('Dashboard strip pipeline closed');
    perfLog?.dropGeneration(this.generation);
    for (const sub of this.subs.values()) sub.bind?.(null);
    this.widgets.clear(); this.shared.clear(); this.subs.clear(); this.byId.clear(); this.routes.clear();
    for (const resolve of this.preparedWaiters) resolve();
    this.preparedWaiters.clear();
    this.dateWatchers.clear(); this.placed.clear(); this.kindCost.clear();
    this.audits.clear(); this.answered.clear(); this.hits.clear(); this.changed.clear(); this.redated.clear();
    this.laneSurfaceFailedListeners.clear();
  }

  private postLane(lane: number, message: ToLane, transfer?: Transferable[]) {
    if (this.closed || this.laneErrors[lane]) return;
    const worker = this.workers[lane];
    if (!worker) return;
    worker.postMessage(message, transfer);
    if (message.type === 'bind') perfLog?.count('page', 'rebind');
  }

  private post(lane: number | 'all', message: PageMessage) {
    if (this.closed) return;
    if (lane === 'all') {
      for (let i = 0; i < this.workers.length; i++) this.postLane(i, message);
    } else this.postLane(lane, message);
  }

  private open(): Promise<void> {
    if (!this.opened) {
      const started = this.host.open(this.ports);
      const ready = 'ready' in started ? started.ready : started;
      if ('close' in started) {
        if (this.closed) started.close();
        else this.closeReader = started.close;
      }
      if ('retireLane' in started && started.retireLane && !this.closed) {
        this.retireReaderLane = started.retireLane;
        this.laneErrors.forEach((error, lane) => { if (error) this.retireReaderLane!(lane); });
      }
      this.opened = ready.then(opened => {
        const session = typeof opened === 'string' ? opened : opened?.session;
        const close = typeof opened === 'object' && opened ? opened.close : undefined;
        const retireLane = typeof opened === 'object' && opened ? opened.retireLane : undefined;
        if (this.closed) { close?.(); return; }
        if (close && !this.closeReader) this.closeReader = close;
        if (retireLane && !this.retireReaderLane) {
          this.retireReaderLane = retireLane;
          this.laneErrors.forEach((error, lane) => { if (error) this.retireReaderLane!(lane); });
        }
        if (typeof session === 'string') this.session = session;
      });
    }
    return this.opened;
  }

  private session: string | undefined;

  private holds(route: RouteState, pending = false) {
    for (const sub of this.subs.values()) if (sub.route === route && sub.channel !== 'pinned' && (sub.status === 'followed' || pending && sub.status === 'pending')) return true;
    return false;
  }

  private published(previous: PlaybackSnapshot, next: PlaybackSnapshot) {
    if (this.closed) return;
    const rate = this.host.rate();
    if (rate !== this.rate) {
      this.rate = rate;
      for (let lane = 0; lane < this.workers.length; lane++) this.postLane(lane, { type: 'options', rate });
    }
    this.sendMotionClock(next);
    const fps = Math.max(1, this.host.frameRate());
    const nominal = Math.max(1, Math.ceil(next.hoursPerSecond / fps * 1.5));
    const rawStep = next.playing && previous.playing && next.seekSequence === previous.seekSequence ? Math.max(0, next.hour - previous.hour) : this.page.step;
    const step = next.playing ? Math.min(rawStep, nominal) : rawStep;
    this.page.publish(next.hour, { playing: next.playing, seek: next.seekSequence, step, speed: next.hoursPerSecond });
    const pictures = this.page.takePictures();
    if (perfLog && next.playing) perfLog.pictures(pictures, this.expiredNames());
    this.dated = true;
    this.flush();
    if (next.playing) noteFrame(performance.now());
    if (auditing()) this.sample(next.seekSequence);
  }

  private readonly audits = new Map<number, Map<number, (fields: Readonly<Record<string, unknown>>) => readonly AuditRow[] | undefined>>();

  audit(id: number, owner: number, rows: ((fields: Readonly<Record<string, unknown>>) => readonly AuditRow[] | undefined) | null) {
    if (this.closed) return;
    let owners = this.audits.get(id);
    if (rows) { if (!owners) this.audits.set(id, owners = new Map()); owners.set(owner, rows); }
    else { owners?.delete(owner); if (owners && !owners.size) this.audits.delete(id); }
  }

  private readonly answered = new Map<number, Map<number, { hour: number; entry: number; rows: () => readonly AuditRow[] | undefined }>>();
  private answers = 0;

  answer(id: number, owner: number, shown: { hour: number; rows: () => readonly AuditRow[] | undefined } | null) {
    if (this.closed) return;
    let owners = this.answered.get(id);
    if (shown) { if (!owners) this.answered.set(id, owners = new Map()); owners.set(owner, { ...shown, entry: -++this.answers }); }
    else { owners?.delete(owner); if (owners && !owners.size) this.answered.delete(id); }
    if (auditing()) this.sample(this.clock.getSnapshot().seekSequence);
  }

  private sample(seek: number) {
    const at = performance.timeOrigin + performance.now();
    for (const held of this.shared.values()) {
      const view = this.page.view(held.id), widget = this.widgets.get(held.id), shown = this.page.widgets.get(held.id);
      if (!widget || !shown) continue;
      const offset = shown.offset, clock = (hour: number) => hour - offset;
      const name = held.widget, surface = widget.view === 'fields' || widget.view === 'panel' ? 'page' : widget.view;
      const answers = [...this.answered.get(held.id) ?? []];
      for (const [owner, answer] of answers) {
        const hour = clock(answer.hour), rows = answer.rows();
        recordPicture({ t: 'picture', at, shownAt: at, seek, widget: name, surface, placement: held.id, owner, generation: this.generation, renderer: 'page', head: clock(view.target), hour, offset, fixedHour: hour,
          since: hour, through: hour, eligibleHour: hour, entry: answer.entry, parts: [], drawMs: 0, ...rows ? { rows } : {},
          mode: 'picture', dim: false, reset: 0, binding: shown.binding, inspect: shown.inspect, tooltip: false });
      }
      const hour = view.mode === 'picture' && view.through !== undefined ? clock(view.through) : null;
      const owners = [...this.audits.get(held.id) ?? []];
      if (!owners.length && answers.length) continue;
      for (const [owner, audit] of owners.length ? owners : [[0, undefined] as const]) {
      const rows = view.mode === 'picture' && view.fields ? audit?.(view.fields) : undefined;
      const auditMissing = audit !== undefined && view.mode === 'picture' && rows === undefined;
      const rawAudit = view.mode === 'picture' && view.fields ? view.fields.audit : undefined;
      const auditId = rawAudit && typeof rawAudit === 'object' && typeof (rawAudit as { id?: unknown }).id === 'number' && (rawAudit as { id: number }).id > 0
        ? (rawAudit as { id: number }).id
        : undefined;
      if (auditId !== undefined) perfLog?.pinPicture(this.generation, held.id, owner, auditId);
      recordPicture({ t: 'picture', at, shownAt: at, seek, widget: name,
        surface, placement: held.id, owner, generation: this.generation, renderer: 'page',
        head: clock(view.target), hour, offset, ...shown.pin !== null ? { fixedHour: clock(shown.pin) } : {},
        ...view.h !== undefined ? { since: clock(view.h) } : {}, ...shown.installed ? { through: clock(shown.installed.c) } : {},
        eligibleHour: shown.installed && view.mode === 'picture' ? clock(Math.min(view.target, shown.installed.c)) : null,
        ...shown.installed ? { entry: shown.installed.id, parts: Object.values(shown.content),
          face: Object.entries(shown.content).filter(([part]) => part !== 'hour').map(([part, id]) => `${part}=${id}`).join('|') } : {},
        drawMs: shown.installed?.ms ?? 0, ...rows ? { rows } : {}, ...auditId !== undefined && view.fields ? { auditId, fields: view.fields } : {}, ...auditMissing ? { auditMissing: true } : {},
        mode: view.mode, ...view.reason !== undefined ? { reason: view.reason } : {}, dim: view.dim, reset: shown.installed?.gen.reset ?? 0, binding: shown.installed?.binding ?? shown.binding,
        inspect: shown.inspect, tooltip: view.tooltip !== null });
      }
    }
  }

  private flush() {
    if (this.closed) return;
    const changed = this.changed, dated = this.dated, redated = this.redated;
    this.changed = new Set(); this.redated = new Set();
    this.dated = false;
    if (changed.size) flushSync(() => { for (const widget of changed) for (const listener of [...widget.installListeners]) listener(); });
    if (dated) {
      for (const widget of this.dateWatchers) {
        this.stats.dateVisits++;
        for (const listener of [...widget.dateListeners]) listener();
      }
    } else if (redated.size) {
      for (const widget of redated) if (widget.dateListeners.size) {
        this.stats.dateVisits++;
        for (const listener of [...widget.dateListeners]) listener();
      }
    }
  }

  private failLane(lane: number, reason: string) {
    if (this.closed || this.laneErrors[lane]) return;
    this.failStrips(reason, lane);
    const failure = asFailure(new Error(reason));
    this.laneErrors[lane] = failure;
    this.preparedAt[lane] = Infinity;
    const worker = this.workers[lane];
    if (worker) {
      worker.onmessage = null;
      worker.onerror = null;
      worker.terminate?.();
    }
    this.retireReaderLane?.(lane);
    this.settlePrepared();
    for (const [key, placedLane] of [...this.placed]) {
      if (placedLane === lane) this.placed.delete(key);
    }
    const routesToCheck = new Set<RouteState>();
    for (const sub of [...this.subs.values()]) {
      if (sub.lane !== lane) continue;
      if (sub.channel !== 'pinned') routesToCheck.add(sub.route);
      sub.bind?.(null);
      const wasFollowed = sub.status === 'followed';
      sub.status = 'failed';
      sub.reason = failure.message;
      sub.error = failure;
      if (sub.channel !== 'pinned' && wasFollowed) {
        void this.ask('unfollow', { id: sub.id }, sub.route, new AbortController().signal).catch(() => {});
      }
    }
    for (const route of routesToCheck) {
      if (!this.holds(route, true)) {
        route.told = null;
        const page = this.nextRoute++;
        this.page.carry(route.page, page);
        route.page = page;
      }
    }
    const sender = this.payloads[lane]!;
    for (const widget of this.widgets.values()) {
      if (widget.lane !== lane) continue;
      for (const ids of Object.values(widget.payloads)) sender.release(ids);
      widget.payloads = { own: NO_PAYLOADS, statics: NO_PAYLOADS, identity: NO_PAYLOADS };
      widget.error = failure;
      const pendingHits = this.hits.get(widget.id);
      if (pendingHits?.length) {
        this.hits.delete(widget.id);
        for (const told of pendingHits) told.hit?.(null);
      }
    }
    this.page.failLane(lane, failure.message);
    this.flush();
  }

  private settlePrepared() {
    if (!this.prepared()) return;
    const waiters = [...this.preparedWaiters];
    this.preparedWaiters.clear();
    for (const resolve of waiters) resolve();
  }

  private receive(lane: number, message: FromLane) {
    if (this.closed || this.laneErrors[lane]) return;
    if (message.type === 'strip') {
      const receipt = this.stripReceipts.get(message.receipt);
      if (receipt?.lane !== lane) return;
      this.stripReceipts.delete(message.receipt);
      if ('error' in message) receipt.reject(new Error(message.error)); else receipt.resolve(message.held);
      return;
    }
    if (message.type === 'prepared') {
      this.preparedAt[lane] = Math.max(this.preparedAt[lane] ?? 0, message.ask);
      this.settlePrepared();
      return;
    }
    if (message.type === 'route-bound') {
      const sub = this.byId.get(message.sub);
      if (sub?.lane === lane && sub.route.page === message.route) sub.bind?.(message.seek);
      return;
    }
    if (message.type === 'lane-surface-failed') {
      for (const listener of [...this.laneSurfaceFailedListeners]) listener(message.lane);
      return;
    }
    if (message.type === 'failure') {
      const routes = new Set<number>();
      for (const widget of this.widgets.values()) if (widget.lane === lane) routes.add(this.page.widgets.get(widget.id)!.route);
      const failure = asFailure(new Error(message.reason));
      for (const route of this.routes.values()) {
        if (routes.has(route.page)) route.error = { seek: this.page.seek, failure };
      }
      for (const route of routes) this.page.fail(route, this.page.command(route), message.reason);
      this.flush();
      return;
    }
    if (message.type === 'sub-failed') {
      const held = this.byId.get(message.sub);
      if (held && held.status !== 'failed' && held.status !== 'refused') {
        if (held.channel !== 'pinned' && held.status === 'followed') {
          void this.ask('unfollow', { id: held.id }, held.route, new AbortController().signal).catch(() => {});
        }
        this.fail(held, asFailure(new Error(message.reason)));
        if (!this.holds(held.route, true)) {
          held.route.told = null;
          const page = this.nextRoute++;
          this.page.carry(held.route.page, page);
          held.route.page = page;
        }
      }
      return;
    }
    if (message.type === 'sub-waiting') {
      const held = this.byId.get(message.sub);
      if (!held) return;
      held.waiting = message.waiting;
      for (const id of held.widgets.keys()) { const widget = this.widgets.get(id); if (widget) this.markWaiting(widget); }
      this.flush();
      return;
    }
    if (message.type === 'widget-failed') {
      if (this.widgets.has(message.widget)) {
        const failure = asFailure(new Error(message.reason));
        this.page.name(message.widget, 'failed', failure.message, 'local');
        this.failedLocally(message.widget, message.stamp, failure);
        this.flush();
      }
      return;
    }
    if (message.type === 'hit') {
      const told = this.hits.get(message.widget)?.shift();
      const pageWidget = this.page.widgets.get(message.widget);
      const staleOwner = message.owner !== undefined && pageWidget?.owner !== undefined && message.owner !== pageWidget.owner;
      const staleInstall = told !== undefined && message.installedId !== told.installed;
      const acceptedKey = staleOwner || staleInstall ? null : message.key;
      if (!staleOwner && !staleInstall && pageWidget?.installed?.id === message.installedId) {
        this.page.inspect(message.widget, acceptedKey);
        this.flush();
      }
      told?.hit?.(acceptedKey, {
        ...(message.installedId !== undefined ? { installedId: message.installedId } : {}),
        ...(message.owner !== undefined ? { owner: message.owner } : {}),
      });
      return;
    }
    if (message.type === 'playback-hover') {
      this.widgets.get(message.widget)?.playbackHoverListeners?.forEach(listener => listener());
      return;
    }
    if (message.type === 'scrubbed') {
      perfLog?.strip({ status: 'scrub frame', generation: this.generation, lane, ...message.frame, widgets: message.samples });
      return;
    }
    if (message.type === 'served') {
      if (message.costs) {
        for (const [id, cost] of message.costs) {
          const widget = this.widgets.get(id);
          if (!widget || !Number.isFinite(cost)) continue;
          const clamped = Math.max(0.05, cost);
          this.costLoads[widget.lane] = Math.max(0, this.costLoads[widget.lane]! + clamped - widget.cost);
          widget.cost = clamped;
          if (widget.kind !== undefined) {
            const prev = this.kindCost.get(widget.kind);
            this.kindCost.set(widget.kind, prev === undefined ? clamped : prev * 0.7 + clamped * 0.3);
          }
        }
      }
      if (message.motionDiagnostics) {
        perfLog?.barPlayback(lane, message.target, message.changed, message.ms, message.motionDiagnostics);
        return;
      }
      if (auditing()) recordPicture({ t: 'workerFrame', at: performance.timeOrigin + performance.now(), lane, lanes: this.lanes, seek: message.seek, head: message.target,
        playing: !message.exact && this.page.playing, changed: message.changed, workMs: message.ms, ...message.cancelled ? { cancelled: true } : {} });
      return;
    }
    this.page.offer(message);
    if (!this.page.playing) this.commitSoon();
  }

  private commitSoon() {
    if (this.closed || this.framing) return;
    this.framing = true;
    engine.next('commit', () => { this.framing = false; if (this.closed) return; this.page.frame(); const pictures = this.page.takePictures(); if (perfLog && this.page.playing) perfLog.pictures(pictures, this.expiredNames()); for (const [id, stamp] of this.page.takeFailures()) this.failedLocally(id, stamp, asFailure(new Error('drawing failed'))); this.flush(); if (auditing()) this.sample(this.page.seek); });
  }

  private place(key: string | undefined, kind: string | undefined, preferredLane?: number): { lane: number; cost: number } {
    const cost = (kind !== undefined ? this.kindCost.get(kind) : undefined) ?? DEFAULT_COST;
    const healthy: number[] = [];
    for (let at = 0; at < this.costLoads.length; at++) {
      if (!this.laneErrors[at]) healthy.push(at);
    }
    if (!healthy.length) return { lane: 0, cost };
    if (preferredLane !== undefined && preferredLane >= 0 && preferredLane < this.costLoads.length && !this.laneErrors[preferredLane]) {
      if (key !== undefined) this.placed.set(key, preferredLane);
      return { lane: preferredLane, cost };
    }
    let total = 0;
    for (const at of healthy) total += this.costLoads[at]!;
    const average = total / healthy.length;
    const held = key === undefined ? undefined : this.placed.get(key);
    if (held !== undefined && !this.laneErrors[held] && this.costLoads[held]! + cost <= average + Math.max(cost, 1)) return { lane: held, cost };
    let lane = healthy[0]!;
    for (let i = 1; i < healthy.length; i++) {
      const at = healthy[i]!;
      if (this.costLoads[at]! < this.costLoads[lane]! - 1e-6
        || (Math.abs(this.costLoads[at]! - this.costLoads[lane]!) <= 1e-6 && this.loads[at]! < this.loads[lane]!)) lane = at;
    }
    if (key !== undefined) this.placed.set(key, lane);
    return { lane, cost };
  }

  mount(options: MountOptions) {
    const kind = options.kind;
    let { lane, cost } = this.place(options.key, kind, options.preferredLane);
    while (!this.workers[lane] && !this.laneErrors[lane] && this.host.createLane) {
      this.ensureLane(lane);
      if (!this.laneErrors[lane]) break;
      const next = this.place(options.key, kind);
      if (next.lane === lane) break;
      lane = next.lane;
    }
    const id = this.nextWidget++;
    this.loads[lane]!++;
    this.costLoads[lane]! += cost;
    const laneFailure = this.laneErrors[lane];
    const widget: WidgetState = { id, lane, kind, cost, subs: new Map(), view: options.view ?? 'fields', model: options.model, mounted: false, route: null,
      installListeners: new Set(), dateListeners: new Set(), binding: { generation: 1, ...(options.owner !== undefined ? { owner: options.owner } : {}), surfaces: [], offset: options.offset, pin: options.pin }, surfaces: [], held: new Map(), ...(laneFailure ? { error: laneFailure } : {}), stamp: undefined,
      payloads: { own: NO_PAYLOADS, statics: NO_PAYLOADS, identity: NO_PAYLOADS } };
    if (options.model !== undefined && !laneFailure) this.own(widget, options.model);
    this.widgets.set(id, widget);
    this.page.mount(id, { route: 0, lane, ...(options.owner !== undefined ? { owner: options.owner } : {}), offset: options.offset, pin: options.pin, bounds: options.bounds, surfaces: [], changed: () => this.changed.add(widget),
      dated: () => this.redated.add(widget) });
    if (laneFailure) {
      this.page.name(id, 'failed', laneFailure.message, 'stream');
      this.flush();
    }
    return id;
  }

  private readonly shared = new Map<string, { id: number; holders: number; widget: string }>();

  private expiredNames(): string[] {
    const expired = this.page.expired();
    if (!expired.length) return [];
    const ids = new Set(expired), names = new Set<string>();
    for (const held of this.shared.values()) if (ids.has(held.id)) names.add(held.widget);
    return [...names];
  }

  acquire(key: string, options: MountOptions): number {
    let held = this.shared.get(key);
    if (!held) this.shared.set(key, held = { id: this.mount({ ...options, key: options.key ?? key }), holders: 0,
      widget: options.widget ?? (key.includes('|') ? key.slice(0, key.lastIndexOf('|')) : key) });
    held.holders++;
    return held.id;
  }

  drop(key: string) {
    const held = this.shared.get(key);
    if (!held || --held.holders > 0) return;
    this.shared.delete(key);
    this.unmount(held.id);
  }

  unmount(id: number) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    this.audits.delete(id);
    this.hits.delete(id);
    this.dateWatchers.delete(widget);
    for (const sub of widget.subs.values()) this.release(sub, id);
    const sender = this.payloads[widget.lane]!;
    for (const ids of Object.values(widget.payloads)) sender.release(ids);
    this.widgets.delete(id);
    this.loads[widget.lane]!--;
    this.costLoads[widget.lane] = Math.max(0, this.costLoads[widget.lane]! - widget.cost);
    this.page.unmount(id);
    this.postLane(widget.lane, { type: 'unmount', widget: id });
  }

  lane(id: number) { return this.widgets.get(id)?.lane ?? 0; }

  status(sub: number): { status: SubStatus; reason?: string; error?: Failure } {
    const held = this.byId.get(sub);
    if (!held) return { status: 'pending' };
    const laneError = this.laneErrors[held.lane];
    if (laneError) return { status: 'failed', reason: laneError.message, error: laneError };
    const routeError = held.route.error?.seek === this.page.seek ? held.route.error.failure : undefined;
    const error = held.error ?? routeError;
    return { status: held.status, ...held.reason !== undefined ? { reason: held.reason } : {}, ...error !== undefined ? { error } : {} };
  }

  widgetError(id: number): Failure | undefined {
    const widget = this.widgets.get(id);
    if (!widget) return undefined;
    const laneError = this.laneErrors[widget.lane];
    if (laneError) return laneError;
    if (widget.error) return widget.error;
    for (const sub of widget.subs.values()) {
      const error = this.status(sub).error;
      if (error) return error;
    }
    return undefined;
  }

  binding(id: number): number { return this.page.widgets.get(id)?.binding ?? 0; }

  private failedLocally(id: number, stamp: number | undefined, failure: Failure) {
    const widget = this.widgets.get(id);
    if (!widget || this.laneErrors[widget.lane]) return;
    widget.error = failure;
    if (stamp !== widget.stamp) this.retryWidget(id);
  }

  retryWidget(id: number) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget || this.laneErrors[widget.lane]) return;
    widget.error = undefined;
    this.page.retryLocal(id);
    widget.binding = this.timed(widget, widget.binding);
    this.postLane(widget.lane, { type: 'bind', widget: id, binding: widget.binding, retry: true, ...widget.wire !== undefined ? { model: widget.wire } : {} });
    this.flush();
  }

  view(id: number): WidgetView | null { return this.widgets.has(id) ? this.page.view(id) : null; }

  onInstall(id: number, listener: () => void) {
    const widget = this.widgets.get(id);
    widget?.installListeners.add(listener);
    return () => { widget?.installListeners.delete(listener); };
  }

  onDate(id: number, listener: () => void) {
    const widget = this.widgets.get(id);
    if (widget) {
      widget.dateListeners.add(listener);
      this.dateWatchers.add(widget);
    }
    return () => {
      if (!widget) return;
      widget.dateListeners.delete(listener);
      if (!widget.dateListeners.size) this.dateWatchers.delete(widget);
    };
  }

  private routeOf(source: DashboardSource, branch: string, offset: number): RouteState {
    const key = JSON.stringify([source.id, source.revision, branch, offset]);
    let route = this.routes.get(key);
    if (!route) this.routes.set(key, route = { page: this.nextRoute++, native: null, key, source, branch, offset, sending: false, pending: null, last: null, told: null });
    return route;
  }

  subscribe(widgetId: number, token: object, source: DashboardSource, branch: string, offset: number, query: FollowRequest, channel: Channel, product?: string) {
    if (this.closed) return;
    const widget = this.widgets.get(widgetId);
    if (!widget) return;
    const route = this.routeOf(source, branch, offset);
    const key = JSON.stringify([widget.lane, route.key, channel, canonical(query)]);
    let sub = this.subs.get(key);
    if (!sub) {
      const id = this.nextSub[widget.lane]!++ * this.lanes + widget.lane;
      sub = { id, key, lane: widget.lane, channel, route, holders: 0, status: 'pending', widgets: new Map(), released: false };
      this.subs.set(key, sub); this.byId.set(id, sub);
      this.register(sub, query, product);
    }
    sub.holders++;
    sub.widgets.set(widgetId, (sub.widgets.get(widgetId) ?? 0) + 1);
    const was = widget.subs.get(token);
    widget.subs.set(token, sub.id);
    if (was !== undefined) this.release(was, widgetId);
    widget.route = route;
    this.remount(widget);
    return sub.id;
  }

  unsubscribe(widgetId: number, token: object) {
    if (this.closed) return;
    const widget = this.widgets.get(widgetId);
    const id = widget?.subs.get(token);
    if (!widget || id === undefined) return;
    widget.subs.delete(token);
    this.release(id, widgetId);
    if (!widget.subs.size) widget.stripReset?.();
    this.remount(widget);
  }

  private release(id: number, widget: number) {
    const sub = this.byId.get(id);
    if (!sub) return;
    const count = sub.widgets.get(widget);
    if (count !== undefined) {
      if (count <= 1) sub.widgets.delete(widget);
      else sub.widgets.set(widget, count - 1);
    }
    if (--sub.holders > 0) return;
    this.subs.delete(sub.key); this.byId.delete(id);
    sub.released = true;
    sub.bind?.(null);
    this.postLane(sub.lane, { type: 'unfollow', sub: id });
    if (sub.channel !== 'pinned' && sub.status === 'followed') void this.ask('unfollow', { id }, sub.route, new AbortController().signal).catch(() => {});
    if (!this.holds(sub.route, true)) { sub.route.told = null; const page = this.nextRoute++; this.page.carry(sub.route.page, page); sub.route.page = page; }
  }

  private markWaiting(widget: WidgetState) {
    this.page.waiting(widget.id, [...widget.subs.values()].some(sub => this.byId.get(sub)?.waiting));
  }

  private remount(widget: WidgetState) {
    const subs = [...new Set(widget.subs.values())];
    this.markWaiting(widget);
    const laneFailure = this.laneErrors[widget.lane];
    this.postLane(widget.lane, { type: 'mount', widget: widget.id, subs, view: widget.view, model: widget.wire });
    this.page.rebind(widget.id, { subscription: true });
    const followed = subs.map(id => this.byId.get(id)).find(sub => sub?.status === 'followed' || sub?.status === 'pending' && sub.channel !== 'pinned');
    if (followed) this.page.mountRoute(widget.id, followed.route.page);
    widget.binding = this.timed(widget, widget.binding);
    this.postLane(widget.lane, { type: 'bind', widget: widget.id, binding: widget.binding });
    widget.mounted = true;
    const subStates = subs.map(id => this.byId.get(id));
    const refused = subStates.find(sub => sub?.status === 'refused');
    const failed = subStates.find(sub => sub?.status === 'failed');
    if (laneFailure) this.page.name(widget.id, 'failed', laneFailure.message, 'stream');
    else if (refused) this.page.name(widget.id, 'refused', refused.reason);
    else if (failed) this.page.name(widget.id, 'failed', failed.reason, 'stream');
    else if (widget.error) this.page.name(widget.id, 'failed', widget.error.message, 'local');
    this.flush();
  }

  private timed(widget: WidgetState, binding: Omit<Binding, 'generation' | 'offset' | 'pin'>): Binding {
    const shown = this.page.widgets.get(widget.id);
    const { dated: _, ...own } = binding;
    const was = widget.binding;
    const owner = shown?.owner;
    const kept = was.owner === undefined || owner === undefined || was.owner === owner;
    if (!kept) widget.stamp = undefined;
    return { ...own, ...kept && was.dated ? { dated: was.dated } : {}, generation: shown?.binding ?? 1, ...(shown?.owner !== undefined ? { owner: shown.owner } : {}), offset: shown?.offset ?? 0, pin: shown?.pin ?? null };
  }

  bind(id: number, surfaces: readonly PageSurface<ImageBitmap>[], binding: Omit<Binding, 'generation' | 'offset' | 'pin'>, change: RebindChange = {}, model?: unknown) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    const laneFailure = this.laneErrors[widget.lane];
    const same = surfaces.length === widget.surfaces.length && surfaces.every((surface, at) => surface === widget.surfaces[at]);
    if (!same) { widget.surfaces = surfaces; this.page.remount(id, surfaces); }
    else this.page.rebind(id, change);
    if (laneFailure) {
      this.page.name(id, 'failed', laneFailure.message, 'stream');
      widget.binding = this.timed(widget, binding);
      this.flush();
      return;
    }
    if (!same || change.subscription) {
      const subStates = [...widget.subs.values()].map(sub => this.byId.get(sub));
      const refused = subStates.find(sub => sub?.status === 'refused');
      const failed = subStates.find(sub => sub?.status === 'failed');
      if (refused) this.page.name(id, 'refused', refused.reason);
      else if (failed) this.page.name(id, 'failed', failed.reason, 'stream');
      else if (widget.error) this.page.name(id, 'failed', widget.error.message, 'local');
    }
    const release = model !== undefined ? this.own(widget, model) : NO_PAYLOADS;
    widget.binding = this.timed(widget, binding);
    this.postLane(widget.lane, {
      type: 'bind',
      widget: id,
      binding: widget.binding,
      ...model !== undefined ? { model: widget.wire } : {},
      ...!same ? { remounted: true } : {},
    });
    this.payloads[widget.lane]!.release(release);
    this.flush();
  }

  present(id: number, view: string, model?: unknown) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget || widget.view === view && model === undefined) return;
    widget.view = view;
    const laneFailure = this.laneErrors[widget.lane];
    if (laneFailure) {
      this.page.rebind(id);
      this.page.name(id, 'failed', laneFailure.message, 'stream');
      widget.binding = this.timed(widget, widget.binding);
      this.flush();
      return;
    }
    const release = model !== undefined ? this.own(widget, model) : NO_PAYLOADS;
    this.postLane(widget.lane, { type: 'mount', widget: id, subs: [...new Set(widget.subs.values())], view, model: widget.wire });
    this.payloads[widget.lane]!.release(release);
    this.page.rebind(id);
    widget.binding = this.timed(widget, widget.binding);
    this.postLane(widget.lane, { type: 'bind', widget: id, binding: widget.binding });
    this.flush();
  }

  private own(widget: WidgetState, model: unknown): ReadonlySet<number> {
    const was = widget.payloads.own;
    const encoded = this.payloads[widget.lane]!.acquire(model);
    widget.model = model;
    widget.wire = encoded.wire;
    widget.payloads = { ...widget.payloads, own: encoded.ids };
    return was;
  }

  supply(id: number, change: SupplyChange) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    if (this.laneErrors[widget.lane]) {
      if (change.statics !== undefined) widget.stamp = change.stamp;
      return;
    }
    const sender = this.payloads[widget.lane]!;
    const released: ReadonlySet<number>[] = [];
    let statics: unknown;
    if (change.statics !== undefined) {
      const encoded = sender.acquire(change.statics);
      released.push(widget.payloads.statics);
      widget.payloads = { ...widget.payloads, statics: encoded.ids };
      statics = encoded.wire;
    }
    let identity: number | null | undefined;
    if (change.identity !== undefined) {
      const [held] = widget.payloads.identity;
      const encoded = sender.acquire(change.identity === null ? {} : { identitySnapshot: change.identity }, held);
      const [next] = encoded.ids;
      if (next === held) sender.release(encoded.ids);
      else {
        released.push(widget.payloads.identity);
        widget.payloads = { ...widget.payloads, identity: encoded.ids };
        identity = next ?? null;
      }
    }
    if (statics !== undefined) widget.stamp = change.stamp;
    const dated = change.dated !== undefined && (statics !== undefined || change.stamp === widget.stamp) ? change.dated : undefined;
    if (dated !== undefined) {
      const { dated: _, ...rest } = widget.binding;
      widget.binding = dated ? { ...rest, dated } : rest;
    }
    if (statics !== undefined || identity !== undefined || dated !== undefined) {
      this.postLane(widget.lane, {
        type: 'supply',
        widget: widget.id,
        ...widget.stamp !== undefined && (statics !== undefined || dated !== undefined) ? { stamp: widget.stamp } : {},
        ...statics !== undefined ? { statics: statics as Supply['statics'] } : {},
        ...identity !== undefined ? { identity } : {},
        ...dated !== undefined ? { dated } : {},
      });
    }
    for (const ids of released) sender.release(ids);
    if (statics !== undefined && widget.error) this.retryWidget(id);
  }

  held<T>(id: number, kind: string, make: () => T): T | null {
    const widget = this.widgets.get(id);
    if (!widget) return null;
    if (!widget.held.has(kind)) widget.held.set(kind, make());
    return widget.held.get(kind) as T;
  }

  retime(id: number, change: RebindChange) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    this.page.rebind(id, change);
    const laneFailure = this.laneErrors[widget.lane];
    if (laneFailure) this.page.name(id, 'failed', laneFailure.message, 'stream');
    widget.binding = this.timed(widget, widget.binding);
    this.postLane(widget.lane, { type: 'bind', widget: id, binding: widget.binding });
    this.flush();
  }

  inspect(id: number, key: string | null) { if (this.closed) return; this.page.inspect(id, key); this.flush(); }

  inspecting(id: number) { return this.page.widgets.get(id)?.inspect ?? null; }

  private readonly hits = new Map<number, { readonly hit?: (key: string | null, meta?: PointerHitMeta) => void; readonly installed?: number }[]>();

  pointer(id: number, surface: number, point: [number, number] | null, hit?: (key: string | null, meta?: PointerHitMeta) => void, trackHover = false) {
    if (this.closed) return;
    const widget = this.widgets.get(id);
    if (!widget) return;
    if (this.laneErrors[widget.lane]) { hit?.(null); return; }
    let waiting = this.hits.get(id);
    if (!waiting) this.hits.set(id, waiting = []);
    const installed = this.page.widgets.get(id)?.installed?.id;
    waiting.push({ ...(hit ? { hit } : {}), ...(installed !== undefined ? { installed } : {}) });
    this.postLane(widget.lane, { type: 'pointer', widget: id, surface, point, ...(installed !== undefined ? { installed } : {}), ...(trackHover ? { trackHover: true } : {}) });
  }

  onPlaybackHover(id: number, listener: () => void) {
    const widget = this.widgets.get(id);
    if (!widget) return () => {};
    const listeners = widget.playbackHoverListeners ??= new Set();
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  }

  exact(sub: number, hour: number, data: unknown, refused: string | null) {
    if (this.closed) return;
    const held = this.byId.get(sub);
    if (held) this.postLane(held.lane, { type: 'exact', sub, hour, data, refused });
  }

  private ask(product: 'follow' | 'unfollow' | 'follow-clock', params: Record<string, unknown>, route: RouteState, signal = this.abort.signal) {
    const sessioned = this.session ? { ...params, session: this.session } : params;
    return this.client.product(productRequest({ sourceId: route.source.id, revision: route.source.revision, product, params: sessioned as GatedParams }),
      signal).then(reply => reply.data);
  }

  private register(sub: SubState, query: FollowRequest, product?: string) {
    if (this.closed) return;
    const laneFailure = this.laneErrors[sub.lane];
    if (laneFailure) {
      sub.status = 'failed';
      sub.reason = laneFailure.message;
      sub.error = laneFailure;
      return;
    }
    const follow = { type: 'follow' as const, sub: sub.id, channel: sub.channel, ...product !== undefined ? { product } : {} };
    if (sub.channel === 'pinned') { this.postLane(sub.lane, follow); sub.status = 'followed'; return; }
    if (!this.client.openFollow) { this.refuse(sub, NO_CHANNEL); return; }
    const route = sub.route.page;
    const bind = () => new Promise<number | null>(resolve => {
      sub.bind = seek => { delete sub.bind; resolve(seek); };
      this.postLane(sub.lane, { ...follow, route: { id: route, seek: this.page.seek, offset: sub.route.offset } });
    });
    const bound = bind();
    void this.open().then(async () => {
      for (let seek = await bound; seek !== null && !this.closed && !sub.released && sub.status === 'pending'; seek = await bind()) {
        if (seek === this.page.seek) return this.ask('follow', { id: sub.id, query, branchId: sub.route.branch, clock: sub.route.offset, lane: sub.lane, lanes: this.lanes, route }, sub.route);
      }
      return null;
    })
      .then(reply => {
        const data = reply as { available?: boolean; reason?: string; route?: number } | null;
        if (this.closed || sub.released || sub.status === 'failed' || sub.status === 'refused' || this.laneErrors[sub.lane]) {
          if (data?.available === true) void this.ask('unfollow', { id: sub.id }, sub.route, new AbortController().signal).catch(() => {});
          return;
        }
        if (data?.available !== true || data.route !== route) { this.refuse(sub, data?.reason ?? 'The service did not follow the reserved route'); return; }
        const first = !this.holds(sub.route), moved = sub.route.native !== null && sub.route.native !== data.route;
        sub.status = 'followed';
        sub.route.native = data.route!;
        for (const widget of sub.widgets.keys()) this.page.mountRoute(widget, sub.route.page);
        if (first || moved) sub.route.told = null;
        this.anchor();
      }, error => {
        if (this.closed || sub.released || sub.status === 'failed' || sub.status === 'refused' || this.laneErrors[sub.lane]) return;
        const failure = asFailure(error);
        if (error instanceof RefusedError) this.refuse(sub, failure.message);
        else this.fail(sub, failure);
      });
  }

  private refuse(sub: SubState, reason: string) {
    if (this.closed) return;
    this.postLane(sub.lane, { type: 'unfollow', sub: sub.id });
    sub.status = 'refused'; sub.reason = reason; sub.error = undefined;
    for (const widget of sub.widgets.keys()) this.page.name(widget, 'refused', reason);
    this.flush();
  }

  private fail(sub: SubState, failure: Failure) {
    if (this.closed) return;
    this.postLane(sub.lane, { type: 'unfollow', sub: sub.id });
    sub.bind?.(null);
    sub.status = 'failed'; sub.reason = failure.message; sub.error = failure;
    for (const widget of sub.widgets.keys()) this.page.name(widget, 'failed', failure.message, 'stream');
    this.flush();
  }

  private anchor() {
    if (this.closed) return;
    const s = this.clock.getSnapshot();
    const held = new Set([...this.subs.values()].filter(sub => sub.status === 'followed' && sub.channel !== 'pinned').map(sub => sub.route));
    for (const route of this.routes.values()) {
      if (route.native === null || !held.has(route)) { route.told = null; route.pending = null; continue; }
      const told = JSON.stringify([s.playing, s.hoursPerSecond, s.seekSequence, s.playing ? null : s.hour, this.host.frameRate()]);
      if (route.told === told) continue;
      route.pending = { snapshot: s, page: route.page, command: this.page.command(route.page),
        composedAt: performance.timeOrigin + (s.composedAt ?? performance.now()), frameRate: this.host.frameRate(), initial: route.told === null || route.pending?.initial === true };
      route.told = told;
      void this.sendAnchors(route);
    }
  }

  private async sendAnchors(route: RouteState) {
    if (route.sending) return;
    route.sending = true;
    try {
      while (route.pending !== null) {
        const target = route.pending;
        route.pending = null;
        const { snapshot: s, page, command, composedAt, frameRate } = target;
        if (this.closed || !this.holds(route) || route.page !== page) continue;
        const last = route.last;
        const sought = target.initial || !last || last.page !== page || last.snapshot.seekSequence !== s.seekSequence
          || !s.playing && (last.snapshot.playing || last.snapshot.hour !== s.hour);
        route.last = target;
        let reason: string | null = null;
        let failure: Failure | undefined;
        try {
          const data = await this.ask('follow-clock', { branchId: route.branch, route: page, clock: route.offset, hour: s.position + route.offset, playing: s.playing, speed: s.hoursPerSecond,
            frameRate, seek: s.seekSequence, sought, composedAt, sentAt: performance.timeOrigin + performance.now() }, route) as { available?: boolean; reason?: string } | null;
          if (data?.available !== true) {
            reason = data?.reason ?? 'The followed stream is no longer available';
            failure = asFailure(new Error(reason));
          }
        } catch (error) {
          if (route.page === page && route.pending === null) route.told = null;
          failure = asFailure(error);
          reason = failure.message;
        }
        if (!this.closed && reason !== null && this.holds(route) && route.page === page) {
          if (this.page.fail(page, command, reason)) {
            route.error = { seek: this.page.seek, failure: failure! };
            this.flush();
          }
        }
      }
    } finally { route.sending = false; }
  }
}

interface PipelineSlot {
  pipeline: Pipeline;
  client: DashboardClient;
  host: () => PipelineHost;
  listeners: Set<() => void>;
}

const pipelines = new WeakMap<PlaybackController, WeakMap<DashboardClient, PipelineSlot>>();
const byClock = new WeakMap<PlaybackController, PipelineSlot>();
const clockGeneration = new WeakMap<PlaybackController, number>();
const clockListeners = new WeakMap<PlaybackController, Set<() => void>>();
const ownerOf = new WeakMap<Pipeline, { clock: PlaybackController; slot: PipelineSlot }>();

export const pipelineGeneration = (clock: PlaybackController) => clockGeneration.get(clock) ?? 1;

export interface LaneNeeds { readonly families: readonly string[]; readonly projections: readonly ProjectionKey[] }
const laneNeeds = new WeakMap<PlaybackController, LaneNeeds>();

export function prepareLanes(clock: PlaybackController, needs: LaneNeeds) {
  laneNeeds.set(clock, needs);
  byClock.get(clock)?.pipeline.prepare(needs.families, needs.projections);
}

function prepareFrom(clock: PlaybackController, pipeline: Pipeline) {
  const needs = laneNeeds.get(clock);
  if (needs?.families.length) pipeline.prepare(needs.families, needs.projections);
}

export function pipelineFor(clock: PlaybackController, client: DashboardClient, host: () => PipelineHost): Pipeline {
  let byClient = pipelines.get(clock);
  if (!byClient) pipelines.set(clock, byClient = new WeakMap());
  let slot = byClient.get(client);
  if (!slot) {
    const existing = byClock.get(clock);
    const generation = existing ? (clockGeneration.get(clock) ?? 1) + 1 : (clockGeneration.get(clock) ?? 1);
    clockGeneration.set(clock, generation);
    const pipeline = new Pipeline(clock, client, host(), generation);
    prepareFrom(clock, pipeline);
    slot = { pipeline, client, host, listeners: new Set() };
    byClient.set(client, slot);
    ownerOf.set(pipeline, { clock, slot });
  }
  byClock.set(clock, slot);
  return slot.pipeline;
}

export function subscribeClockPipeline(clock: PlaybackController, listener: () => void): () => void {
  let set = clockListeners.get(clock);
  if (!set) clockListeners.set(clock, set = new Set());
  set.add(listener);
  return () => { set.delete(listener); };
}

export function restartPipeline(pipeline: Pipeline): Pipeline | null {
  const owned = ownerOf.get(pipeline);
  if (!owned) return null;
  const { clock, slot } = owned;
  if (slot.pipeline !== pipeline) return slot.pipeline;
  pipeline.close();
  const generation = (clockGeneration.get(clock) ?? 0) + 1;
  clockGeneration.set(clock, generation);
  const next = new Pipeline(clock, slot.client, slot.host(), generation);
  prepareFrom(clock, next);
  slot.pipeline = next;
  byClock.set(clock, slot);
  ownerOf.set(next, { clock, slot });
  for (const listener of [...slot.listeners]) listener();
  for (const listener of [...clockListeners.get(clock) ?? []]) listener();
  return next;
}
