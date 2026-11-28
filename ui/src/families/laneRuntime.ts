import type { Fact } from '../dashboard/follow/facts.ts';
import type { FollowedAnswer } from '../dashboard/follow/message';
import { sameRevision, type Revisions } from '../dashboard/follow/payloads.ts';
import type { PlaybackPaint, PlaybackPicture, PlaybackRenderer, LaneView, Painted, PaintInput, Scene, Surface, Value as LaneValue } from '../draw/lane.ts';
import { markCanvasPainted } from '../draw/laneDispatch.ts';
import { setContextSlot, snapCanvasText } from '../draw/kit.ts';
import { LaneArtCache, LaneArtPool, type LaneArtFetcher } from './art/laneCache.ts';
import { createLaneArtProvider, type ArtResolutions } from './art/provider.ts';
import { TrailingFlushScheduler, type CadenceTimers } from './cadence.ts';
import type { ArtProvider, BarsPlaybackForm, BoundedReadView, FamilyProjection, FamilyTheme, IdentityProvider } from './contract.ts';
import type { FamilyTooltipEntry, FamilyTooltipField } from './host/tooltips.ts';
import { IdentityIndex } from './identity/index.ts';
import { createSnapshotIdentityProvider } from './identity/provider.ts';
import type { IdentitySnapshot } from './identity/types.ts';
import type { MotionSampleDiagnostic } from './motion.ts';
import type { FamilyId } from './plan.ts';
import { ensureBoundProjection, familyProjectionFailure, forgetProjectionWaiter, retryFamilyProjection, unknownProjectionReason } from './registry.ts';
import { projectScene, samePictures, sceneReadView, snapshotPictures, type SceneProjection, type SceneReaders, type SceneSources } from './scene.ts';
import { SceneTiles, type TiledSources } from './sceneTiles.ts';
import type { PagedRows, RowWindow } from './table/rowSource.ts';

export interface LaneMotion {
  readonly easeValues?: boolean;
  readonly glideDurationMs?: number;
  readonly nowMs?: number;
  readonly speed?: number;
}

export interface LaneBinding<TOptions = unknown> extends TiledSources {
  readonly hovers?: Readonly<Record<string, { readonly hour: number; readonly data: unknown }>>;
  readonly family: string;
  readonly kind: string;
  readonly view?: string;
  readonly options: TOptions;
  readonly sources: Readonly<Record<string, number>>;
  readonly staticPanels?: Readonly<Record<string, unknown>>;
  readonly staticShifts?: Readonly<Record<string, number>>;
  readonly staticAnswers?: Readonly<Record<string, FollowedAnswer>>;
  readonly scrollTop: number;
  readonly scrollLeft?: number;
  readonly theme?: FamilyTheme;
  readonly identitySnapshot?: IdentitySnapshot | null;
  readonly artResolutions?: ArtResolutions;
  readonly selectedKey?: string | null;
  readonly focusedKey?: string | null;
  readonly focusInside?: boolean;
  readonly mirrorRequest?: number;
  readonly audit?: boolean;
  readonly auditId?: number;
  readonly revisions?: Revisions;
  readonly pagedRows?: PagedRows;
  readonly window?: RowWindow;
  readonly motion?: LaneMotion;
}

export interface LanePrevious<TModel, TLayout> {
  readonly model: TModel;
  readonly layout: TLayout;
  readonly hour: number;
}

export interface LaneStep<TModel, TLayout> {
  readonly previous: LanePrevious<TModel, TLayout> | null;
  readonly now: number;
  readonly speed: number;
}

export interface LaneDrawn<TBinding extends LaneBinding, TModel, TLayout> {
  readonly binding: TBinding;
  readonly model: TModel;
  readonly layout: TLayout;
  readonly hour: number;
}

export interface LaneFrame<TBinding extends LaneBinding, TModel, TLayout> {
  readonly binding: TBinding;
  readonly theme: FamilyTheme;
  readonly model: TModel;
  readonly layout: TLayout;
  readonly hour: number;
  readonly surface: Surface;
  readonly inspect: string | null;
  readonly now: number;
  readonly speed: number;
  readonly art: LaneArtCache;
  readonly artProvider: ArtProvider;
  readonly index: IdentityIndex | null;
  readonly projection: FamilyProjection<TModel, unknown>;
  readonly projected: BoundedReadView;
  readonly before: LaneDrawn<TBinding, TModel, TLayout> | null;
  readonly changedAt: number;
  readonly continuity: number;
  readonly fullRepaint?: boolean;
  readonly exactPlayback?: boolean;
  readNow(): BoundedReadView;
}

export type LaneStatus = 'picture' | 'scene' | 'waiting' | { readonly refused: string };

type Undated<T> = T extends unknown ? Omit<T, 'hour'> & { readonly hour?: number } : never;

export type LaneTooltip = Undated<FamilyTooltipEntry>;

export interface LaneFamily<TBinding extends LaneBinding, TModel, TLayout> {
  readonly family: FamilyId;
  boundary(model: TModel, hour: number): number;
  redraw(model: TModel, hour: number): number;
  status(model: TModel): LaneStatus;
  readonly view: readonly (keyof TBinding & string)[];
  readonly playback?: {
    enabled(binding: TBinding): boolean;
    supports(model: TModel, form: BarsPlaybackForm): boolean;
    unavailable(model: TModel): boolean;
    frame(frame: LaneFrame<TBinding, TModel, TLayout>, next: TModel, fraction: number, nextLayout: () => TLayout, form: BarsPlaybackForm, diagnostic?: MotionSampleDiagnostic): LaneFrame<TBinding, TModel, TLayout> | false | null;
  };
  layout(model: TModel, binding: TBinding, surface: Surface, theme: FamilyTheme, measure: OffscreenCanvasRenderingContext2D, hour: number, step: LaneStep<TModel, TLayout>): TLayout;
  keepsCanvas?(ctx: OffscreenCanvasRenderingContext2D, frame: LaneFrame<TBinding, TModel, TLayout>): boolean;
  onSpeed?(layout: TLayout, speed: number, continuity: number): void;
  paint(ctx: OffscreenCanvasRenderingContext2D, frame: LaneFrame<TBinding, TModel, TLayout>): boolean | number | { drew: boolean; next: boolean | number };
  hit(frame: LaneFrame<TBinding, TModel, TLayout>, point: readonly [number, number]): string | null;
  tooltip(frame: LaneFrame<TBinding, TModel, TLayout>, key: string, focus: boolean): LaneTooltip | null;
  structure(frame: LaneFrame<TBinding, TModel, TLayout>): Readonly<Record<string, unknown>>;
  readonly mirror: ((frame: LaneFrame<TBinding, TModel, TLayout>) => unknown) | null;
  audit(frame: LaneFrame<TBinding, TModel, TLayout>): readonly unknown[];
  visibleArt(frame: LaneFrame<TBinding, TModel, TLayout>): Iterable<string>;
}

export interface LaneRuntimeOptions {
  readonly fetchBitmap?: LaneArtFetcher;
  readonly artMaxEntries?: number;
  readonly timers?: CadenceTimers;
}

type Field = { readonly value: unknown; readonly content: string };

interface Tip {
  field: Field | null;
  frame: number;
  key: string;
  entry: LaneTooltip | null;
  hour: number;
}

const noTip = (): Tip => ({ field: null, frame: -1, key: '', entry: null, hour: -Infinity });

interface Painting {
  readonly seek: number;
  readonly speed: number;
  readonly hour: number;
  readonly owner: string | undefined;
  readonly pin: number | null;
  readonly kind: string;
  readonly view: string | undefined;
  readonly revisions: Revisions | undefined;
}

interface Held<TBinding extends LaneBinding, TModel, TLayout> {
  readonly widgetKey: object;
  canvas: OffscreenCanvas;
  ctx: OffscreenCanvasRenderingContext2D;
  workerCanvas: OffscreenCanvas | null;
  sharedPainted: boolean;
  lastTarget: 'bitmap' | 'worker' | 'shared' | null;
  opaque: boolean;
  surfaceColor: string;
  readonly art: LaneArtCache;
  wake: () => void;
  artRevision: number;
  readers: Revisions | null;
  snapshot: number | null;
  artResolutions: ArtResolutions | undefined;
  index: IdentityIndex | null;
  identity: IdentityProvider | undefined;
  artProvider: ArtProvider;
  projected: SceneProjection<TModel> | null;
  projectedArtRevision: number;
  kind: string;
  viewName: string | undefined;
  owner: string | undefined;
  inputs: Revisions | undefined;
  readonly tiles: SceneTiles;
  parts: readonly (readonly Fact<LaneValue>[])[];
  validFrom: number;
  validUntil: number;
  drawFrom: number;
  drawUntil: number;
  drawn: LaneDrawn<TBinding, TModel, TLayout> | null;
  before: LaneDrawn<TBinding, TModel, TLayout> | null;
  painting: Painting | null;
  changedAt: number;
  continuity: number;
  demandFields: Record<string, Field>;
  layout: TLayout | null;
  layoutKey: readonly unknown[];
  expiry: number;
  windowKey: string;
  phases: { serves: number; projMs: number; layoutMs: number; paintMs: number; fieldsMs: number; auditMs: number };
  phasesField: Field | null;
  drawKey: readonly unknown[];
  content: string;
  hit: (point: readonly [number, number]) => string | null;
  motion: boolean;
  artShown: string | null;
  artSeen: readonly string[] | null;
  artUrls: readonly string[] | null;
  artUrlsKey: string;
  speed: number;
  motionOverride: boolean;
  structureKey: readonly unknown[];
  structureFields: Record<string, Field>;
  kept: Map<string, Field>;
  frameKey: readonly unknown[];
  frameRevision: number;
  mirrorField: Field | null;
  mirrorFrame: number;
  mirrorView: string;
  mirrorSeek: number;
  mirrorRequest: number;
  mirrorHasRows: boolean;
  tip: Tip;
  focusTip: Tip;
  flusher: TrailingFlushScheduler;
  auditKey: readonly unknown[];
  auditField: Field | null;
}

const PROJECTION_INPUTS = ['options', 'sources', 'staticPanels', 'staticAnswers', 'staticShifts', 'hovers', 'tiledBySource', 'headSourceBySeries', 'pagedRows'] as const;

function sameInputRevisions(a: Revisions | undefined, b: Revisions | undefined): boolean {
  if (!a || !b) return false;
  if (a === b) return true;
  for (const field of PROJECTION_INPUTS) if (!sameRevision(a[field], b[field])) return false;
  return true;
}

function readerRevisions(binding: LaneBinding): Revisions | null {
  const revisions = binding.revisions;
  if (!revisions) return null;
  return {
    ...(revisions.identitySnapshot !== undefined ? { identitySnapshot: revisions.identitySnapshot } : {}),
    ...(revisions.artResolutions !== undefined ? { artResolutions: revisions.artResolutions } : {}),
  };
}

type Indexed = { readonly index: IdentityIndex; readonly identity: IdentityProvider; users: number };
const indexes = new Map<number, Indexed>();

function acquireIndex(revision: number, snapshot: IdentitySnapshot, previous?: IdentityIndex | null): Indexed {
  let held = indexes.get(revision);
  if (!held) {
    let base = previous ?? null;
    if (!base && snapshot.epoch !== undefined) {
      for (const existing of indexes.values()) {
        if (existing.index.epoch === snapshot.epoch
          && existing.index.scope.sourceId === snapshot.scope.sourceId
          && existing.index.scope.revision === snapshot.scope.revision
          && existing.index.scope.branchId === snapshot.scope.branchId
          && existing.index.scope.content === snapshot.scope.content
          && existing.index.scope.language === snapshot.scope.language) {
          base = existing.index;
          break;
        }
      }
    }
    const index = IdentityIndex.fromSnapshot(snapshot, base);
    indexes.set(revision, held = { index, identity: createSnapshotIdentityProvider(index), users: 0 });
  }
  held.users++;
  return held;
}

function releaseIndex(revision: number | null): void {
  if (revision === null) return;
  const held = indexes.get(revision);
  if (held && --held.users <= 0) indexes.delete(revision);
}

const EMPTY: Painted = { surfaces: [], fields: {}, motion: false };
const WAITING: Painted = { ...EMPTY, waiting: true };

function hasMirrorRows(value: unknown): boolean {
  if (!value || typeof value !== 'object') return false;
  if (Array.isArray(value)) return value.length > 0;
  const rows = (value as { readonly rows?: readonly unknown[] }).rows;
  return Array.isArray(rows) && rows.length > 0;
}

function hasStructureRows(fields: Record<string, Field>): boolean {
  if (Array.isArray(fields.nav?.value) && fields.nav.value.length > 0) return true;
  const range = fields.range?.value as { readonly count?: number } | undefined;
  if (typeof range?.count === 'number' && range.count > 0) return true;
  const extent = fields.extent?.value as { readonly rows?: readonly unknown[] } | undefined;
  return Array.isArray(extent?.rows) && extent.rows.length > 0;
}

function resetMirror<TBinding extends LaneBinding, TModel, TLayout>(held: Held<TBinding, TModel, TLayout> | undefined): void {
  if (!held) return;
  held.mirrorField = null;
  held.mirrorFrame = -1;
  held.mirrorView = '';
  held.mirrorSeek = -1;
  held.mirrorHasRows = false;
}

function sameKey(a: readonly unknown[], b: readonly unknown[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export function sameFieldValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a === 'number' && typeof b === 'number') return Number.isNaN(a) && Number.isNaN(b);
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!sameFieldValue(a[i], b[i])) return false;
    return true;
  }
  if (Array.isArray(b)) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  let leftCount = 0;
  for (const key in left) {
    if (!Object.prototype.hasOwnProperty.call(left, key)) continue;
    const lv = left[key];
    if (lv === undefined) continue;
    leftCount++;
    if (!Object.prototype.hasOwnProperty.call(right, key) || !sameFieldValue(lv, right[key])) return false;
  }
  let rightCount = 0;
  for (const key in right) {
    if (!Object.prototype.hasOwnProperty.call(right, key) || right[key] === undefined) continue;
    if (++rightCount > leftCount) return false;
  }
  return rightCount === leftCount;
}

function field(held: Held<LaneBinding, unknown, unknown>, slot: string, value: unknown, content: () => string): Field {
  const prev = held.kept.get(slot);
  if (prev && prev.value === value) return prev;
  const made: Field = { value, content: prev && sameFieldValue(prev.value, value) ? prev.content : content() };
  held.kept.set(slot, made);
  return made;
}

function windowKeyOf(binding: LaneBinding): string {
  return binding.window ? `${binding.window.first}:${binding.window.count}` : '';
}

function painting(input: PaintInput, binding: LaneBinding): Painting {
  return {
    seek: input.seek, speed: binding.motion?.speed ?? input.speed, hour: input.hour, owner: input.binding.owner, pin: input.binding.pin,
    kind: binding.kind, view: binding.view, revisions: binding.revisions,
  };
}

type Continuity = 'continues' | 'tick' | 'cut';

function samePlacement(last: Painting, next: Painting): boolean {
  return next.seek === last.seek
    && next.pin === last.pin
    && next.owner === last.owner
    && next.kind === last.kind
    && next.view === last.view
    && next.revisions !== undefined && last.revisions !== undefined
    && sameRevision(next.revisions.options, last.revisions.options)
    && sameRevision(next.revisions.sources, last.revisions.sources);
}

function follows(last: Painting | null, next: Painting): Continuity {
  if (last === null || !samePlacement(last, next)) return 'cut';
  if (next.hour === last.hour) return 'tick';
  return next.hour > last.hour && next.speed > 0 && last.speed > 0 && next.pin === null ? 'continues' : 'cut';
}

function viewKey(binding: LaneBinding, fields: readonly string[]): unknown[] {
  if (!binding.revisions) return [{}];
  return fields.map(field => binding.revisions![field] ?? (binding as unknown as Record<string, unknown>)[field]);
}

function backing(surface: Surface): readonly [number, number, number] {
  const dpr = surface.dpr > 0 ? surface.dpr : 1;
  return [Math.max(1, Math.round(surface.width * dpr)), Math.max(1, Math.round(surface.height * dpr)), dpr];
}

function retryOrThrow(failed: Map<number, number>, input: PaintInput, reason: string, retry: () => Painted): Painted {
  const failedAt = failed.get(input.widget);
  if (failedAt !== undefined && failedAt !== input.binding.generation) {
    failed.delete(input.widget);
    return retry();
  }
  failed.set(input.widget, input.binding.generation);
  throw new Error(reason);
}

const laneArt = new LaneArtPool();

export type LaneFamilyLoad = () => Promise<LaneFamily<any, any, any>>;
const ignoreProjectionReady = () => {};

export function lazyLaneView(family: string, load: LaneFamilyLoad, wake: (widget: number) => void, options?: LaneRuntimeOptions): LaneView {
  let view: LaneView | null = null;
  let loading: Promise<void> | null = null;
  let failure: string | null = null;
  const waiting = new Set<number>();
  const failedGenerations = new Map<number, number>();
  const start = (): Promise<void> => loading ??= load().then(
    lane => { view = createLaneView(lane, wake, options); },
    (error: unknown) => { failure = error instanceof Error ? error.message : String(error); },
  ).finally(() => {
    loading = null;
    const woken = [...waiting];
    waiting.clear();
    for (const widget of woken) wake(widget);
  });
  const ready = (widget: number): LaneView | null => {
    if (view || failure !== null) return view;
    waiting.add(widget);
    void start();
    return null;
  };
  return {
    prepare: () => (view || failure !== null ? Promise.resolve() : start()),
    onSpeed(speed, state) {
      view?.onSpeed?.(speed, state);
    },
    next(scene, hour, state) {
      const loaded = ready(scene.widget);
      return loaded?.next ? loaded.next(scene, hour, state) : null;
    },
    playbackEligible(widget, model) {
      return view?.playbackEligible?.(widget, model) ?? false;
    },
    unmount(widget, state) {
      waiting.delete(widget);
      failedGenerations.delete(widget);
      view?.unmount?.(widget, state);
    },
    paint(input, state, content) {
      const loaded = ready(input.widget);
      if (loaded) return loaded.paint(input, state, content);
      if (failure === null) return WAITING;
      return retryOrThrow(failedGenerations, input, `The ${family} lane did not load: ${failure}`, () => {
        failure = null;
        return ready(input.widget)?.paint(input, state, content) ?? WAITING;
      });
    },
    scrub(scene, statics, state, readers) {
      if (!view && readers) throw new Error('Dashboard strip projection is not ready');
      return view?.scrub?.(scene, statics, state, readers) ?? { mode: 'waiting' };
    },
  };
}

function sceneSources(binding: LaneBinding, validateTail: SceneSources['validateTail']): SceneSources {
  return {
    sources: binding.sources,
    ...(binding.staticPanels ? { staticPanels: binding.staticPanels } : {}),
    ...(binding.staticShifts ? { staticShifts: binding.staticShifts } : {}),
    ...(binding.staticAnswers ? { staticAnswers: binding.staticAnswers } : {}),
    ...(binding.hovers ? { hovers: binding.hovers } : {}),
    kind: binding.kind,
    ...(binding.pagedRows ? { pagedRows: binding.pagedRows } : {}),
    ...(binding.window ? { window: binding.window } : {}),
    ...(validateTail ? { validateTail } : {}),
  };
}

export function createLaneView<TBinding extends LaneBinding, TModel, TLayout>(
  family: LaneFamily<TBinding, TModel, TLayout>,
  wake: (widget: number) => void,
  options?: LaneRuntimeOptions,
): LaneView {
  const stateKey = `lane:${family.family}`;
  const isBinding = (value: unknown): value is TBinding => !!value && typeof value === 'object'
    && (value as { family?: unknown }).family === family.family
    && typeof (value as { kind?: unknown }).kind === 'string';
  const wakers = new Map<number, () => void>();
  const failedGenerations = new Map<number, number>();
  const artPool = options?.fetchBitmap ? new LaneArtPool(options.fetchBitmap) : laneArt;
  const wakerOf = (widget: number): (() => void) => {
    let waker = wakers.get(widget);
    if (!waker) wakers.set(widget, waker = () => wake(widget));
    return waker;
  };
  const projectionOf = (binding: TBinding, widget: number) =>
    ensureBoundProjection<TModel, unknown>(binding, wakerOf(widget)) as FamilyProjection<TModel, unknown> | 'loading' | 'unknown';
  const invalidPlayback = new WeakSet<TBinding>();
  const playbackForm = (binding: TBinding, projection: FamilyProjection<TModel, unknown>) =>
    !invalidPlayback.has(binding) && family.playback?.enabled(binding) === true && projection.singleValueBars?.(binding.options);
  const reportInvalidPlayback = (binding: TBinding, form: BarsPlaybackForm) => {
    if (invalidPlayback.has(binding)) return;
    invalidPlayback.add(binding);
    console.error(`Bars projection ${binding.kind}/${binding.view ?? 'main'} produced a form outside its ${form === true ? 'single-value' : form === 'target-markers' ? 'target-marker' : 'ordered-segment'} guarantee`);
  };
  const sameInputs = (held: Held<TBinding, TModel, TLayout>, binding: TBinding, scene: Scene, hour: number): boolean =>
    held.projected !== null
    && held.kind === binding.kind
    && held.viewName === binding.view
    && held.owner === scene.owner
    && sameInputRevisions(held.inputs, binding.revisions)
    && (!held.projected.reads.window || held.windowKey === windowKeyOf(binding))
    && hour >= held.validFrom
    && hour < held.validUntil
    && samePictures(held.parts, scene.pictures);
  const readersOf = (held: Held<TBinding, TModel, TLayout>): SceneReaders => ({
    ...(held.identity ? { identity: held.identity } : {}),
    art: held.artProvider,
    index: held.index,
    artCacheRevision: held.art.failureVersion,
    ...(held.artResolutions ? { artResolutions: held.artResolutions } : {}),
    artRevision: held.artRevision,
  });
  const projectAt = (scene: Scene, hour: number, binding: TBinding, projection: FamilyProjection<TModel, unknown>,
    base: SceneSources, readers: SceneReaders, tiles: SceneTiles) => {
    const series = tiles.series(scene, base, binding, hour);
    const projected = projectScene(scene, series ? { ...base, series } : base, hour, readers, view => projection.project(view, binding.options));
    let until = Math.min(
      projection.nextChange?.(projected.view, binding.options) ?? Infinity,
      family.boundary(projected.model, hour),
      projected.boundary,
    );
    for (const lines of series?.values() ?? []) for (const line of lines.values()) until = Math.min(until, line.validInterval[1] + 1);
    return { projected, until: until > hour ? until : hour + 1 };
  };

  const clearColorGetters = new WeakMap<OffscreenCanvasRenderingContext2D, () => string>();

  function configureContext(
    canvas: OffscreenCanvas,
    opaque: boolean,
    getSurfaceColor: () => string,
  ): OffscreenCanvasRenderingContext2D {
    const ctx = canvas.getContext('2d', opaque ? { alpha: false } : undefined);
    if (!ctx) throw new Error('Could not acquire 2D canvas context');
    snapCanvasText(ctx);
    if (opaque) {
      const wrapped = clearColorGetters.has(ctx);
      clearColorGetters.set(ctx, getSurfaceColor);
      if (!wrapped) {
        const rawClearRect = ctx.clearRect.bind(ctx);
        ctx.clearRect = (x: number, y: number, w: number, h: number) => {
          const color = clearColorGetters.get(ctx)?.() ?? '';
          if (!color) { rawClearRect(x, y, w, h); return; }
          const prev = ctx.fillStyle;
          ctx.fillStyle = color;
          ctx.fillRect(x, y, w, h);
          ctx.fillStyle = prev;
        };
      }
    }
    return ctx;
  }

  function create(surface: Surface, widget: number, workerCanvas?: PaintInput['workerCanvas']): Held<TBinding, TModel, TLayout> {
    const [width, height] = backing(surface);
    const opaque = workerCanvas?.opaque ?? false;
    const canvas = workerCanvas ? workerCanvas.canvas : new OffscreenCanvas(width, height);
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
    const art = new LaneArtCache({
      ...(options?.artMaxEntries !== undefined ? { maxEntries: options.artMaxEntries } : {}),
      pool: artPool,
    });
    const held: Held<TBinding, TModel, TLayout> = {
      widgetKey: {},
      canvas,
      ctx: null as unknown as OffscreenCanvasRenderingContext2D,
      workerCanvas: workerCanvas ? workerCanvas.canvas : null,
      sharedPainted: false,
      lastTarget: null,
      opaque,
      surfaceColor: '',
      art,
      wake: wakerOf(widget),
      artRevision: 0,
      readers: null, snapshot: null, artResolutions: undefined, index: null, identity: undefined,
      artProvider: createLaneArtProvider(art),
      projected: null, projectedArtRevision: 0,
      kind: '', viewName: undefined, owner: undefined, inputs: undefined,
      parts: [], validFrom: Infinity, validUntil: -Infinity, drawFrom: Infinity, drawUntil: -Infinity, drawn: null, before: null, painting: null, changedAt: -Infinity, continuity: 0,
      demandFields: {},
      layout: null, layoutKey: [], expiry: Infinity,
      windowKey: '', phases: { serves: 0, projMs: 0, layoutMs: 0, paintMs: 0, fieldsMs: 0, auditMs: 0 }, phasesField: null,
      drawKey: [], content: '', hit: () => null, motion: false, artShown: null, artSeen: null, artUrls: null, artUrlsKey: '', speed: 0, motionOverride: false,
      structureKey: [], structureFields: {},
      kept: new Map(),
      frameKey: [], frameRevision: 0,
      mirrorField: null, mirrorFrame: -1, mirrorView: '', mirrorSeek: -1, mirrorRequest: 0, mirrorHasRows: false,
      tip: noTip(), focusTip: noTip(),
      flusher: new TrailingFlushScheduler([], options?.timers),
      auditKey: [], auditField: null,
      tiles: new SceneTiles(),
    };
    held.ctx = configureContext(canvas, opaque, () => held.surfaceColor);
    art.setOnSettle(() => {
      held.artRevision++;
      held.wake();
    });
    return held;
  }

  return {
    playbackEligible(widget, binding) {
      if (!family.playback || !isBinding(binding)) return false;
      const projection = projectionOf(binding, widget);
      return typeof projection !== 'string' && Boolean(playbackForm(binding, projection));
    },
    scrub(scene, statics, state, readers) {
      const binding = scene.model;
      if (!isBinding(binding)) {
        if (readers) throw new Error('Dashboard strip projection is not bound');
        return { mode: 'waiting' };
      }
      const projection = ensureBoundProjection<TModel, unknown>(binding, ignoreProjectionReady);
      if (projection === 'loading' || projection === 'unknown') {
        if (readers) throw new Error('Dashboard strip projection is not ready');
        return { mode: 'waiting' };
      }
      const input = statics(projection.validateTail, binding.window);
      if (input.mode !== 'picture') return input;
      const held = state.get(stateKey) as Held<TBinding, TModel, TLayout> | undefined;
      const options = input.options === undefined ? binding.options : input.options;
      const scrubScene = input.options === undefined ? scene : { ...scene, model: { ...binding, options } };
      const picture = projectScene(scrubScene, { sources: {}, exact: input.statics }, input.statics.hour,
        { ...(held ? readersOf(held) : {}), ...(readers ? { identity: readers.identity } : {}) }, view => projection.project(view, options));
      const status = family.status(picture.model);
      if (status === 'waiting') return { mode: 'waiting' };
      if (typeof status === 'object') return { mode: 'refused', reason: status.refused };
      return { mode: 'picture', picture };
    },

    next(scene: Scene, hour: number, state?: Map<string, unknown>): number | null {
      const binding = scene.model;
      if (!isBinding(binding)) return Infinity;
      const projection = projectionOf(binding, scene.widget);
      if (projection === 'loading' || projection === 'unknown') return null;
      const held = state?.get(stateKey) as Held<TBinding, TModel, TLayout> | undefined;
      return held && sameInputs(held, binding, scene, hour) ? Math.min(held.validUntil, Math.max(hour + 1, family.redraw(held.projected!.model, hour))) : null;
    },

    unmount(widget: number, state: Map<string, unknown>): void {
      const waker = wakers.get(widget);
      if (waker) forgetProjectionWaiter(waker);
      wakers.delete(widget);
      failedGenerations.delete(widget);
      const held = state.get(stateKey) as Held<TBinding, TModel, TLayout> | undefined;
      if (!held) return;
      held.flusher.cancel();
      held.art.dispose();
      releaseIndex(held.snapshot);
      state.delete(stateKey);
    },

    onSpeed(speed: number, state: Map<string, unknown>): void {
      const held = state.get(stateKey) as Held<TBinding, TModel, TLayout> | undefined;
      if (!held?.layout || held.motionOverride) return;
      family.onSpeed?.(held.layout, speed, held.continuity);
    },

    paint(input: PaintInput, state: Map<string, unknown>, content: () => string): Painted {
      const binding = input.model;
      const surface = input.binding.surfaces[0];
      const existing = state.get(stateKey) as Held<TBinding, TModel, TLayout> | undefined;
      if (!surface || surface.width <= 0 || surface.height <= 0) {
        resetMirror(existing);
        return WAITING;
      }
      const waiting: Painted = { ...EMPTY, nextChangeAt: input.hour + 1 };
      if (!isBinding(binding) || !binding.theme) return waiting;
      const theme = binding.theme;
      const projection = projectionOf(binding, input.widget);
      if (projection === 'loading') {
        resetMirror(existing);
        return WAITING;
      }
      if (projection === 'unknown') {
        resetMirror(existing);
        const failure = familyProjectionFailure(family.family, binding.kind, binding.view);
        if (!failure) return { ...EMPTY, mode: 'refused', reason: unknownProjectionReason(binding) };
        return retryOrThrow(failedGenerations, input, unknownProjectionReason(binding), () => {
          retryFamilyProjection(family.family, binding.kind, binding.view);
          return projectionOf(binding, input.widget) === 'loading' ? WAITING : EMPTY;
        });
      }

      let held = existing;
      if (!held) {
        held = create(surface, input.widget, input.workerCanvas);
        state.set(stateKey, held);
      }
      const keep = held;
      const activeTarget: 'bitmap' | 'worker' | 'shared' = input.sharedSurface ? 'shared' : input.workerCanvas ? 'worker' : 'bitmap';
      let canvasSwitched = keep.lastTarget !== null && keep.lastTarget !== activeTarget;
      if (input.workerCanvas && keep.workerCanvas !== input.workerCanvas.canvas) {
        keep.workerCanvas = input.workerCanvas.canvas;
        keep.canvas = input.workerCanvas.canvas;
        keep.opaque = input.workerCanvas.opaque;
        keep.ctx = configureContext(keep.canvas, keep.opaque, () => keep.surfaceColor);
        keep.drawKey = [];
        keep.sharedPainted = false;
        canvasSwitched = true;
      } else if (!input.sharedSurface && !input.workerCanvas && keep.workerCanvas !== null) {
        const [bw, bh] = backing(surface);
        keep.workerCanvas = null;
        keep.canvas = new OffscreenCanvas(bw, bh);
        keep.opaque = false;
        keep.ctx = configureContext(keep.canvas, false, () => keep.surfaceColor);
        keep.drawKey = [];
        canvasSwitched = true;
      }
      if (canvasSwitched) keep.drawKey = [];
      keep.wake = wakerOf(input.widget);

      const [width, height, dpr] = backing(surface);
      keep.surfaceColor = theme.surface;
      if (keep.canvas.width !== width || keep.canvas.height !== height) {
        keep.canvas.width = width;
        keep.canvas.height = height;
      }

      const readerKey = readerRevisions(binding);
      const readersChanged = readerKey === null || keep.readers === null || !sameRevision(readerKey, keep.readers);
      if (readersChanged) {
        keep.readers = readerKey;
        const snapshot = binding.identitySnapshot;
        const revision = binding.revisions?.identitySnapshot;
        const indexed = snapshot && typeof revision === 'number' ? acquireIndex(revision, snapshot, keep.index) : null;
        releaseIndex(keep.snapshot);
        keep.snapshot = indexed ? revision as number : null;
        keep.index = indexed?.index ?? null;
        keep.identity = indexed?.identity;
        keep.artResolutions = binding.artResolutions;
        keep.artProvider = createLaneArtProvider(keep.art, {
          index: keep.index,
          ...(binding.artResolutions ? { resolutions: binding.artResolutions } : {}),
        });
      }

      const hour = input.hour;
      const effectiveNow = binding.motion?.nowMs ?? input.now;
      const effectiveSpeed = binding.motion?.speed ?? input.speed;
      keep.motionOverride = binding.motion?.speed !== undefined;
      const timed = binding.audit === true;
      const t0 = timed ? performance.now() : 0;
      const current = painting(input, binding);
      const continuity = follows(keep.painting, current);
      keep.painting = current;
      if (continuity === 'cut') {
        const hadEphemeral = keep.motion || Number.isFinite(keep.expiry);
        keep.before = null;
        keep.motion = false;
        keep.expiry = Infinity;
        if (hadEphemeral) keep.continuity++;
      }
      const readers = readersOf(keep);
      let reuse = sameInputs(keep, binding, input, hour);
      if (reuse && (readersChanged || keep.projectedArtRevision !== keep.artRevision)) {
        reuse = keep.projected!.reads.unchanged(readers);
        keep.projectedArtRevision = keep.artRevision;
      }
      if (reuse && !samePictures(keep.parts, input.pictures, true)) keep.parts = snapshotPictures(input.pictures);
      if (!reuse) {
        const base = sceneSources(binding, projection.validateTail);
        const { projected, until } = projectAt(input, hour, binding, projection, base, readers, keep.tiles);
        if (keep.kind !== binding.kind || keep.viewName !== binding.view || keep.owner !== input.owner) {
          resetMirror(keep);
        }
        keep.projected = projected;
        keep.projectedArtRevision = keep.artRevision;
        keep.kind = binding.kind;
        keep.viewName = binding.view;
        keep.owner = input.owner;
        keep.inputs = binding.revisions;
        keep.windowKey = windowKeyOf(binding);
        keep.parts = snapshotPictures(input.pictures);
        keep.validFrom = hour;
        keep.validUntil = until;
        const demands: Record<string, Field> = {};
        if (projected.identityDemands.length) demands.identityDemands = field(keep, 'identityDemands', projected.identityDemands, content);
        if (projected.artAsks.length) demands.artDemands = field(keep, 'artDemands', projected.artAsks, content);
        keep.demandFields = demands;
        keep.drawUntil = -Infinity;
        keep.before = continuity === 'continues' ? keep.drawn : null;
        keep.changedAt = effectiveNow;
        keep.continuity++;
      }
      const projected = keep.projected!;
      const model = projected.model;
      const tProj = timed ? performance.now() : 0;
      if (hour < keep.drawFrom || hour >= keep.drawUntil) {
        keep.drawFrom = hour;
        keep.drawUntil = Math.min(keep.validUntil, Math.max(hour + 1, family.redraw(model, hour)));
      }
      const drawn = keep.drawFrom;
      const nextChangeAt = Number.isFinite(keep.drawUntil) ? { nextChangeAt: keep.drawUntil } : {};

      const shared = input.sharedSurface;
      const playbackOwnsPixels = input.playbackOwnsPixels === true;
      const direct = (shared || keep.workerCanvas) && input.binding.pin === null;
      const form = playbackOwnsPixels ? projection.singleValueBars?.(binding.options) : direct && playbackForm(binding, projection);
      let animation = form ? family.playback : undefined;
      if (form && animation && !animation.supports(model, form)) {
        reportInvalidPlayback(binding, form);
        animation = undefined;
      }
      const metadataOnly = playbackOwnsPixels || (animation !== undefined && input.deferPlaybackDraw === 'handover');
      const playbackUnavailable = animation?.unavailable(model) === true;
      const status = family.status(model);
      const settlePlayback = input.restorePlayback;
      if (status === 'waiting') {
        resetMirror(keep);
        return WAITING;
      }
      if (typeof status === 'object') {
        keep.flusher.cancel();
        keep.drawn = null;
        keep.drawKey = [];
        keep.hit = () => null;
        resetMirror(keep);
        return { ...EMPTY, mode: 'refused', reason: status.refused, ...nextChangeAt };
      }

      const layoutKey = [model, drawn, surface.width, surface.height, ...viewKey(binding, ['theme', ...family.view])];
      if (!keep.layout || !sameKey(keep.layoutKey, layoutKey)) {
        const previous = continuity === 'continues' ? keep.drawn : null;
        keep.layout = family.layout(model, binding, surface, theme, keep.ctx, drawn, { previous, now: effectiveNow, speed: effectiveSpeed });
        keep.layoutKey = layoutKey;
      }
      const layout = keep.layout;
      const tLayout = timed ? performance.now() : 0;
      let readNow: BoundedReadView | null = null;
      const slotDirty = canvasSwitched || (shared ? (shared.dirty || !keep.sharedPainted) : Boolean(input.workerCanvas?.dirty));
      const frame: LaneFrame<TBinding, TModel, TLayout> = {
        binding, theme, model, layout, surface,
        hour: drawn,
        inspect: input.inspect,
        now: effectiveNow,
        speed: effectiveSpeed,
        art: keep.art,
        artProvider: keep.artProvider,
        index: keep.index,
        projection,
        projected: projected.view,
        before: keep.before,
        changedAt: keep.changedAt,
        continuity: keep.continuity,
        ...(slotDirty ? { fullRepaint: true } : {}),
        ...(animation ? { exactPlayback: true } : {}),
        readNow: () => readNow ??= sceneReadView(projected.scene, projection.validateTail ? { ...binding, validateTail: projection.validateTail } : binding, drawn, readers),
      };

      if (shared) {
        const { lane: sharedLane, slot } = shared;
        const sdpr = sharedLane.dpr > 0 ? sharedLane.dpr : dpr;
        setContextSlot(sharedLane.ctx, {
          widgetKey: keep.widgetKey,
          slotBw: Math.max(1, Math.round(slot.w * sdpr)),
          slotBh: Math.max(1, Math.round(slot.h * sdpr)),
        });
      }
      const drawKey = [layout, keep.continuity, binding.scrollTop, binding.scrollLeft ?? 0, input.inspect, binding.selectedKey, binding.focusedKey, keep.artRevision, width, height];
      const speedChanged = effectiveSpeed !== keep.speed;
      if (speedChanged) family.onSpeed?.(layout, effectiveSpeed, keep.continuity);
      const speedRedraw = speedChanged && keep.content !== '' && family.keepsCanvas?.(shared ? shared.lane.ctx : keep.ctx, frame) === false;
      keep.speed = effectiveSpeed;
      const drawChanged = !sameKey(keep.drawKey, drawKey);
      const shouldPaint = settlePlayback || slotDirty || keep.motion || effectiveNow >= keep.expiry || speedRedraw || drawChanged;
      const drawCanvas = (shown = frame, sharedTarget = shared) => {
        if (sharedTarget) {
          const { lane: sharedLane, slot } = sharedTarget;
          const sctx = sharedLane.ctx;
          const sdpr = sharedLane.dpr > 0 ? sharedLane.dpr : dpr;
          sharedLane.activeSlot.ox = Math.round(slot.x * sdpr);
          sharedLane.activeSlot.oy = Math.round(slot.y * sdpr);
          sharedLane.activeSlot.w = slot.w;
          sharedLane.activeSlot.h = slot.h;
          sharedLane.activeSlot.dpr = sdpr;
          sharedLane.activeSlot.surfaceColor = theme.surface;
          sharedLane.activeSlot.widgetKey = keep.widgetKey;
          setContextSlot(sctx, {
            widgetKey: keep.widgetKey,
            slotBw: Math.max(1, Math.round(slot.w * sdpr)),
            slotBh: Math.max(1, Math.round(slot.h * sdpr)),
          });
          sctx.save();
          try {
            if (animation) {
              const left = Math.round((slot.bx ?? slot.x) * sdpr);
              const top = Math.round((slot.by ?? slot.y) * sdpr);
              const right = Math.round(((slot.bx ?? slot.x) + (slot.bw ?? slot.w)) * sdpr);
              const bottom = Math.round(((slot.by ?? slot.y) + (slot.bh ?? slot.h)) * sdpr);
              sharedLane.rawSetTransform(1, 0, 0, 1, 0, 0);
              sharedLane.rawClearRect(left, top, right - left, bottom - top);
              sctx.beginPath();
              sctx.rect(left, top, right - left, bottom - top);
              sctx.clip();
            }
            sctx.setTransform(sdpr, 0, 0, sdpr, 0, 0);
            const bx = (slot.bx ?? slot.x) - slot.x;
            const by = (slot.by ?? slot.y) - slot.y;
            const bw = slot.bw ?? slot.w;
            const bh = slot.bh ?? slot.h;
            const cr = Math.min(sharedLane.cornerRadius ?? 0, bw / 2, bh / 2);
            if (cr > 0) {
              sctx.beginPath();
              sctx.moveTo(bx, by);
              sctx.lineTo(bx + bw, by);
              sctx.lineTo(bx + bw, by + bh - cr);
              sctx.arcTo(bx + bw, by + bh, bx + bw - cr, by + bh, cr);
              sctx.lineTo(bx + cr, by + bh);
              sctx.arcTo(bx, by + bh, bx, by + bh - cr, cr);
              sctx.closePath();
              sctx.clip();
            } else {
              sctx.beginPath();
              sctx.rect(bx, by, bw, bh);
              sctx.clip();
            }
            const paintW = Math.min(surface.width, slot.w);
            const paintH = Math.min(surface.height, slot.h);
            if (sharedLane.opaque) {
              sctx.fillStyle = theme.surface;
              if (family.family !== 'table') {
                sctx.fillRect(bx, by, bw, bh);
              } else {
                if (bx < 0) sctx.fillRect(bx, by, -bx, bh);
                if (bx + bw > paintW) sctx.fillRect(paintW, by, bx + bw - paintW, bh);
                if (by < 0) sctx.fillRect(bx, by, bw, -by);
                if (by + bh > paintH) sctx.fillRect(bx, paintH, bw, by + bh - paintH);
              }
            }
            sctx.beginPath();
            sctx.rect(0, 0, paintW, paintH);
            sctx.clip();
            const result = family.paint(sctx, shown);
            keep.sharedPainted = true;
            return result;
          } finally {
            sctx.restore();
          }
        }
        keep.ctx.save();
        try {
          keep.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
          if (animation) keep.ctx.clearRect(0, 0, surface.width, surface.height);
          if (keep.opaque && family.family !== 'table') {
            keep.ctx.fillStyle = theme.surface;
            keep.ctx.fillRect(0, 0, surface.width, surface.height);
          }
          return family.paint(keep.ctx, shown);
        } finally {
          keep.ctx.restore();
        }
      };
      let repainted = false;
      if (shouldPaint) {
        const shown = [...family.visibleArt(frame)];
        const shownKey = shown.join('\u0000');
        let offscreenChanged = false;
        if (projected.artUrls !== keep.artSeen) {
          const urlsKey = projected.artUrls.join('\u0000');
          keep.artSeen = projected.artUrls;
          if (keep.artUrls === null || urlsKey !== keep.artUrlsKey) {
            offscreenChanged = true;
            keep.artUrls = projected.artUrls;
            keep.artUrlsKey = urlsKey;
          }
        }
        if (offscreenChanged || shownKey !== keep.artShown) {
          keep.artShown = shownKey;
          keep.art.requestVisible(shown, keep.artUrls ?? projected.artUrls);
        }
        const canKeep = metadataOnly || (input.deferPlaybackDraw === 'expired-refusal' && playbackUnavailable)
          || (!settlePlayback && !input.playbackOwnsPixels && !slotDirty && keep.content && family.keepsCanvas?.(shared ? shared.lane.ctx : keep.ctx, frame));
        const painted = canKeep ? { drew: false, next: false } : drawCanvas();
        const incremental = typeof painted === 'object';
        repainted = !incremental || painted.drew;
        if (!incremental && !keep.workerCanvas && !shared) markCanvasPainted(keep.canvas, () => drawCanvas());
        const next = incremental ? painted.next : painted;
        keep.motion = next === true;
        keep.expiry = typeof next === 'number' ? next : Infinity;
        keep.drawn = { binding, model, layout, hour: drawn };
        keep.drawKey = drawKey;
        keep.hit = point => family.hit(frame, point);
        if (!incremental || painted.drew || !keep.content || (metadataOnly && drawChanged)) keep.content = content();
      }
      keep.lastTarget = activeTarget;

      let playback: PlaybackRenderer | undefined;
      if (form && animation && !playbackOwnsPixels) {
        const base = sceneSources(binding, projection.validateTail);
        const tiles = new SceneTiles();
        type Visual = { parts: ReturnType<typeof snapshotPictures>; dates: readonly number[]; start: number; from: number; until: number; model: TModel; projected: typeof frame.projected; layout?: TLayout };
        const cache: Visual[] = [];
        let artRevision = keep.artRevision;
        const layoutOf = (visual: Visual) => visual.layout ??= family.layout(
          visual.model, binding, surface, theme, keep.ctx, visual.from, { previous: null, now: effectiveNow, speed: 0 },
        );
        const modelAt = (sample: PlaybackPicture): Visual | null => {
          if (invalidPlayback.has(binding)) return null;
          if (artRevision !== keep.artRevision) {
            cache.length = 0;
            artRevision = keep.artRevision;
          }
          let found: Visual | undefined;
          const dates = binding.tiledBySource ? sample.pictures.map(picture => picture.c) : [];
          for (const item of cache) {
            if (item.start === sample.from && item.from <= sample.hour && (!found || item.from > found.from)
              && sameKey(item.dates, dates) && samePictures(item.parts, sample.pictures, true)) found = item;
          }
          if (found && sample.hour < found.until) return found;
          let hour = found?.until ?? sample.from;
          try {
            while (hour <= sample.hour) {
              const { projected, until } = projectAt({ ...input, pictures: sample.pictures }, hour, binding, projection, base, readersOf(keep), tiles);
              if (!animation.supports(projected.model, form)) {
                reportInvalidPlayback(binding, form);
                return null;
              }
              if (sample.hour < until) {
                const visual = { parts: snapshotPictures(sample.pictures), dates, start: sample.from, from: hour, until, model: projected.model, projected: projected.view };
                cache.unshift(visual);
                if (cache.length > 3) cache.pop();
                return visual;
              }
              hour = until;
            }
          } catch {
            return null;
          }
          return null;
        };
        const paint: PlaybackPaint = (current, next, position, target, inspect, diagnostic) => {
          const exact = modelAt(current);
          if (invalidPlayback.has(binding)) return 'release';
          if (!exact) {
            if (diagnostic) diagnostic.reason = 'modelValidity';
            return 'empty';
          }
          const unavailable = animation.unavailable(exact.model);
          const begin = exact.from;
          const to = !unavailable && next && position > begin && position < next.hour && exact.until >= next.hour ? modelAt(next) : null;
          if (invalidPlayback.has(binding)) return 'release';
          const fraction = to && next ? (position - begin) / (next.hour - begin) : 0;
          const recorded = { ...frame, hour: exact.from, model: exact.model, layout: layoutOf(exact), projected: exact.projected, inspect };
          const moving = to && !animation.unavailable(to.model) ? animation.frame(recorded, to.model, fraction, () => layoutOf(to), form, diagnostic) : null;
          if (diagnostic && next) {
            diagnostic.reason = position <= begin ? 'recordedHour' : !to ? 'modelValidity' : moving === null ? 'incompatible' : 'unchanged';
          }
          const shown = moving || recorded;
          drawCanvas(shown, target);
          return { kind: moving ? 'interpolated' : 'exact', hit: point => family.hit(shown, point), from: exact.from, until: exact.until };
        };
        playback = {
          paint,
          unavailableFrom(picture) {
            const visual = modelAt(picture);
            return visual && animation.unavailable(visual.model) ? visual.from : undefined;
          },
        };
      }

      const tPaint = timed ? performance.now() : 0;
      const fields: Record<string, Field> = { ...keep.demandFields };
      const structureKey = [model, layout, playbackUnavailable];
      const structureChanged = !sameKey(keep.structureKey, structureKey);
      if (structureChanged) {
        keep.structureKey = structureKey;
        const next: Record<string, Field> = {};
        for (const [slot, value] of Object.entries(family.structure(frame))) next[slot] = field(keep, slot, value, content);
        keep.structureFields = next;
      }
      Object.assign(fields, keep.structureFields);
      const frameKey = [model, layout, drawn];
      const mirror = family.mirror;
      if (!sameKey(keep.frameKey, frameKey)) {
        keep.frameKey = frameKey;
        keep.frameRevision++;
      }
      const frameRevision = keep.frameRevision;

      const now = input.now;
      if (mirror) {
        const mirrorView = `${binding.focusedKey ?? ''}\u0000${binding.scrollLeft ?? 0}\u0000${binding.scrollTop}`;
        const mirrorStale = keep.mirrorFrame !== frameRevision || keep.mirrorView !== mirrorView;
        const mirrorRequest = binding.mirrorRequest ?? 0;
        const hasRows = hasStructureRows(fields);
        const mirrorNeeded =
          input.speed === 0
          || Boolean(binding.focusedKey || binding.focusInside)
          || !keep.mirrorField
          || mirrorRequest !== keep.mirrorRequest
          || (!keep.mirrorHasRows && hasRows)
          || keep.mirrorSeek !== input.seek;
        if (mirrorStale && mirrorNeeded) {
          const mirrorValue = mirror(frame);
          keep.mirrorField = field(keep, 'mirror', mirrorValue, content);
          keep.mirrorFrame = frameRevision;
          keep.mirrorView = mirrorView;
          keep.mirrorSeek = input.seek;
          keep.mirrorRequest = mirrorRequest;
          keep.mirrorHasRows = hasMirrorRows(mirrorValue);
        } else if (!mirrorStale) {
          if (keep.mirrorSeek !== input.seek) keep.mirrorSeek = input.seek;
          if (keep.mirrorRequest !== mirrorRequest) keep.mirrorRequest = mirrorRequest;
        }
        if (keep.mirrorField && keep.mirrorFrame === frameRevision && keep.mirrorView === mirrorView && keep.mirrorField.value !== null) fields.mirror = keep.mirrorField;
      }

      const tipOf = (tip: Tip, slot: string, key: string, wire: string, focus: boolean): Field => {
        if (!tip.field || tip.frame !== frameRevision || tip.key !== key) {
          tip.entry = family.tooltip(frame, key, focus);
          tip.frame = frameRevision;
          tip.key = key;
          tip.field = null;
        }
        const undated = tip.entry !== null && tip.entry.hour === undefined;
        if (!tip.field || (undated && tip.hour !== hour)) {
          const entry = tip.entry;
          const text = (undated && entry ? { ...entry, hour, ...(entry.payload?.hour !== undefined ? { payload: { ...entry.payload, hour } } : {}) } : entry) as FamilyTooltipEntry | null;
          tip.field = field(keep, slot, { key: wire, text } satisfies FamilyTooltipField, content);
          tip.hour = hour;
        }
        return tip.field;
      };
      if (input.inspect) fields.tooltip = tipOf(keep.tip, 'tooltip', input.inspect, input.inspect, false);
      else keep.tip.field = null;
      const focused = binding.focusedKey;
      if (focused) fields.focusTooltip = tipOf(keep.focusTip, 'focusTooltip', focused, `focus:${focused}`, true);
      else keep.focusTip.field = null;
      keep.flusher.sync(now, input.speed, () => keep.wake(), keep.expiry);
      const tFields = timed ? performance.now() : 0;

      if (binding.audit) {
        const auditKey = [model, layout, binding.scrollTop, binding.scrollLeft ?? 0, binding.auditId ?? -1];
        if (!keep.auditField || !sameKey(keep.auditKey, auditKey)) {
          keep.auditKey = auditKey;
          keep.auditField = field(keep, 'audit', { id: binding.auditId ?? -1, rows: family.audit(frame) }, content);
        }
        fields.audit = keep.auditField;
      } else {
        keep.auditField = null;
        keep.auditKey = [];
      }

      if (timed) {
        const phases = keep.phases;
        phases.serves++;
        phases.projMs += tProj - t0;
        phases.layoutMs += tLayout - tProj;
        phases.paintMs += tPaint - tLayout;
        phases.fieldsMs += tFields - tPaint;
        phases.auditMs += performance.now() - tFields;
        if (input.speed === 0 || phases.serves % 30 === 0 || !keep.phasesField) keep.phasesField = { value: { kind: binding.kind, ...phases }, content: content() };
        fields.phases = keep.phasesField;
      }

      return {
        ...(status === 'picture' ? { mode: 'picture' as const } : {}),
        ...nextChangeAt,
        viewport: { width: surface.width, height: surface.height, dpr, scrollLeft: binding.scrollLeft ?? 0, scrollTop: binding.scrollTop },
        surfaces: keep.workerCanvas || shared ? [] : [{ source: keep.canvas, content: keep.content }],
        ...(keep.workerCanvas || shared ? { surfaceContent: keep.content } : {}),
        fields,
        motion: keep.motion,
        ...(playback ? { playback } : {}),
        ...(metadataOnly ? { playbackOwnsPixels: true } : {}),
        ...(playbackUnavailable ? { playbackUnavailable: true } : {}),
        repainted,
        ...(keep.motion || Number.isFinite(keep.expiry) ? { ephemeral: true } : {}),
        hit: keep.hit,
      };
    },
  };
}
