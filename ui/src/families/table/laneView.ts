import { canvasMeasureText, measureCanvasText } from '../../charts/textWidth.ts';
import { BEHIND_BAR_HEIGHT, BEHIND_BAR_MAX_WIDTH, BEHIND_BAR_MIN_WIDTH, BEHIND_BAR_OFFSET, contextWidgetKey } from '../../draw/kit.ts';
import { collectArtUrls, type ArtUrlInput } from '../art/provider.ts';
import type { LaneArtCache } from '../art/laneCache.ts';
import type { FamilyAuditRow } from '../auditRow.ts';
import type { Surface } from '../../draw/lane.ts';
import type { FamilyTheme } from '../contract.ts';
import type { LaneBinding, LaneFamily, LaneFrame, LaneStep } from '../laneRuntime.ts';
import { sameFieldValue } from '../laneRuntime.ts';
import { formatScalar, valueStateProps } from '../valueFormat.ts';
import { treeLevelsOf } from './cellPainters.ts';
import { buildColumnLayouts, computeRowLayout, END_NOTE_HEIGHT, extentTop, layoutFamilyTable, measureFixedColumnWidths, rowExtents, tableRowWindow, type FamilyColumnLayout, type FamilyTableLayout, type RowExtents, type RowLayoutGeometry } from './layout.ts';
import {
  cellBarFraction,
  computeColumnMaxima,
  deriveTableAudit,
  deriveTableMirror,
  deriveTableTooltip,
  keepsProductOrder,
  nextTableChange,
  resolveTableRows,
  resolveTableRowsIncremental,
  tableRank,
  updateColumnMaximaIncremental,
  type FlatTableRow,
  type TableCell,
  type TableColumnSpec,
  type TableModel,
  type TableRow,
} from './model.ts';
import { barPixels, barPixelSpan, collectVisibleGlidingArt, collectVisibleTableArt, createTablePicture, drawFamilyTable, drawPaneFrame, glideDurationForSpeed, hasGlideCrossing, hitFamilyTable, hitPanes, NO_GLIDE, rankHoldForSpeed, tableGlide, type FamilyTablePicture, type TableGlide } from './painter.ts';
import { combinedPaneGeometry, combinedPaneRows, paneGeometry, panesHeight, paneScroll, paneTitleText, resolvePanes, visiblePaneRange, type ResolvedPane } from './panes.ts';
import { CELL_INSET, glideOffset, MEDIA_ROW_HEIGHT, ROW_HEIGHT, rankMarkers, showsIcon, type GlideOffset, type RankMarker, type TableSettings } from './settings.ts';

export interface FamilyTableBinding<TOptions = unknown> extends LaneBinding<TOptions> {
  readonly family: 'table';
  readonly settings: TableSettings;
  readonly width: number;
  readonly height: number;
  readonly scrollLeft: number;
  readonly expanded?: Readonly<Record<string, boolean>>;
  readonly reducedMotion?: boolean;
  readonly pauseFlush?: number;
}

const TEXT_SAMPLE_ROWS = 40;

interface TableShape {
  readonly model: TableModel;
  readonly options: unknown;
  readonly settings: TableSettings;
  readonly expanded: unknown;
  readonly width: number;
  readonly height: number;
  readonly dpr: number;
  readonly theme: FamilyTheme;
  readonly measureText: (text: string, font: string) => number;
  readonly flatRows: readonly FlatTableRow[];
  readonly rowIndex: Map<string, number>;
  readonly hasGroups: boolean;
  readonly incremental: boolean;
  readonly hasTree: boolean;
  readonly treeLevels: number | readonly number[];
  readonly panes: readonly ResolvedPane[] | null;
  readonly paneGeom: ReturnType<typeof paneGeometry> | null;
  readonly paneSpans: ReturnType<typeof combinedPaneGeometry> | null;
  readonly paneStarts: readonly number[];
  readonly rankShown: boolean;
  readonly rankMarkersOn: boolean;
  readonly rankWidth: number;
  readonly widthKey: string;
  readonly samples: Record<string, string[]>;
  readonly measuredWidths: Record<string, number>;
  readonly fixedWidths: Record<string, number>;
  readonly heldWidths: boolean;
  readonly allLayouts: FamilyColumnLayout[];
  readonly columns: FamilyTableLayout;
  readonly rowHeightMode: 'standard' | 'media';
  readonly rowHeight: number;
  readonly rowGeom: RowLayoutGeometry | null;
  readonly variableHeightCount: number;
  readonly windowOffset: number;
  readonly totalCount: number;
  readonly totalHeight: number;
  readonly extents: RowExtents | null;
  readonly maxima: Map<string, number>;
  readonly maximaCols: string;
  readonly rowAuditCache: WeakMap<TableRow, { readonly sig: string; readonly rows: readonly FamilyAuditRow[] }>;
  readonly sortSig: string;
  readonly orderKeys: readonly string[];
}

interface TableMotion {
  readonly markers: ReadonlyMap<string, RankMarker>;
  readonly expiries: readonly number[];
  readonly glide: TableGlide;
  readonly glideEnd: number;
}

interface TableLayout {
  readonly shape: TableShape;
  readonly localScrollTop: number;
  readonly scrollLeft: number;
  readonly rowWindow: { readonly first: number; readonly count: number };
  readonly shownWindows: readonly { readonly first: number; readonly count: number }[];
  readonly tops: (index: number) => number;
  readonly heights: (index: number) => number;
  readonly focusedIndex: number;
  readonly height: number;
  readonly visibleRows: readonly { readonly first: number; readonly count: number }[];
  readonly visibleColumns: readonly FamilyColumnLayout[] | null;
  readonly visibleColumnStarts: readonly number[] | null;
}

type TableFrame = LaneFrame<FamilyTableBinding, TableModel, TableLayout>;

interface PaintedTable {
  readonly width: number;
  readonly height: number;
  art: number;
  artStates?: readonly (readonly [string, unknown])[];
  readonly list: FamilyTablePicture[];
  still?: TableLayout;
  inspect?: string | null;
  selected?: string | null;
  focused?: string | null;
  dpr?: number;
}

const pictures = new WeakMap<object, PaintedTable>();

function picturesFor(ctx: OffscreenCanvasRenderingContext2D, surface: Surface, art: number): PaintedTable {
  const width = surface.width * surface.dpr;
  const height = surface.height * surface.dpr;
  const key = contextWidgetKey(ctx);
  let held = pictures.get(key);
  if (!held || held.width !== width || held.height !== height) pictures.set(key, held = { width, height, art, list: [] });
  else held.art = art;
  return held;
}

function widthConfigOf(columns: readonly TableColumnSpec[], settings: TableSettings, theme: FamilyTheme, hideHeader: boolean): string {
  return JSON.stringify([
    hideHeader,
    settings.sort.column,
    theme.font,
    theme.fontSize.small,
    theme.fontSize.body,
    settings.order,
    columns.map(c => {
      const own = settings.columns[c.key];
      return [c.key, c.label, c.as, own?.show ?? true, own?.as ?? '', own?.width ?? null, c.numeric ? 1 : 0, c.cellKind ?? ''];
    }),
  ]);
}

function textSamples(columns: readonly { readonly key: string; readonly numeric?: boolean; readonly widest?: readonly string[] }[], rows: readonly FlatTableRow[]): Record<string, string[]> {
  const samples: Record<string, string[]> = {};
  const limit = Math.min(rows.length, TEXT_SAMPLE_ROWS);
  for (const column of columns) {
    if (column.numeric || column.widest?.length) continue;
    const list: string[] = [];
    for (let i = 0; i < limit; i++) {
      const item = rows[i]!;
      if (item.kind !== 'data') continue;
      const cell = item.row.cells[column.key];
      if (!cell) continue;
      const val = cell.value.kind === 'known' ? String(cell.value.value) : '';
      if (val) list.push(val);
    }
    samples[column.key] = list;
  }
  return samples;
}

function flagCounts(columns: readonly { readonly key: string; readonly display?: string }[], rows: readonly FlatTableRow[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const column of columns) {
    if (column.display !== 'flagName' && column.display !== 'flag') continue;
    let most = 0;
    for (let i = 0; i < rows.length; i++) {
      const item = rows[i]!;
      if (item.kind === 'data') most = Math.max(most, item.row.cells[column.key]?.flags?.length ?? 0);
    }
    if (most > 1) counts[column.key] = most;
  }
  return counts;
}

interface MeasuredRow {
  readonly key: string;
  readonly height: number;
}

interface HeightBasis {
  readonly options: unknown;
  readonly settings: TableSettings;
  readonly expanded: unknown;
  readonly width: number;
  readonly theme: FamilyTheme;
  readonly rowHeight: number;
  readonly totalCount: number;
}

const heightMemories = new WeakMap<object, { readonly basis: HeightBasis; readonly rows: ReadonlyMap<number, MeasuredRow> }>();

function heldHeights(owner: object, basis: HeightBasis, rows: readonly FlatTableRow[], windowOffset: number): ReadonlyMap<number, MeasuredRow> | null {
  const held = heightMemories.get(owner);
  if (!held || !(Object.keys(basis) as (keyof HeightBasis)[]).every(key => held.basis[key] === basis[key])) return null;
  const end = windowOffset + rows.length;
  for (const [ordinal, row] of held.rows) {
    if (ordinal < windowOffset || ordinal >= end) continue;
    const at = rows[ordinal - windowOffset]!;
    if (at.kind !== 'loading' && at.key !== row.key) return null;
  }
  return held.rows;
}

function rememberHeights(
  owner: object,
  basis: HeightBasis,
  held: ReadonlyMap<number, MeasuredRow> | null,
  rows: readonly FlatTableRow[],
  rowGeom: RowLayoutGeometry | null,
  windowOffset: number,
): Map<number, MeasuredRow> {
  const memory = new Map<number, MeasuredRow>();
  const end = windowOffset + rows.length;
  if (held) for (const [ordinal, row] of held) if (ordinal < windowOffset || ordinal >= end) memory.set(ordinal, row);
  for (let i = 0; i < rows.length; i++) {
    const kept = rows[i]!.kind === 'loading' ? held?.get(windowOffset + i) : undefined;
    const height = rowGeom ? rowGeom.heights[i]! : basis.rowHeight;
    if (kept) memory.set(windowOffset + i, kept);
    else if (height !== basis.rowHeight) memory.set(windowOffset + i, { key: rows[i]!.key, height });
  }
  heightMemories.set(owner, { basis, rows: memory });
  return memory;
}

function holdColumnWidths(measured: Record<string, number>, previous: Record<string, number>): Record<string, number> {
  let held = previous;
  for (const key of Object.keys(measured)) {
    const width = measured[key]!;
    if (width <= previous[key]!) continue;
    if (held === previous) held = { ...previous };
    held[key] = width;
  }
  return held;
}

function shapeOf(model: TableModel, binding: FamilyTableBinding, theme: FamilyTheme, surface: Surface, measure: OffscreenCanvasRenderingContext2D, step: LaneStep<TableModel, TableLayout>): TableShape {
  const prev = step.previousLayout?.shape ?? null;
  const { width, height, dpr } = surface;
  const playing = step.speed > 0;
  const settings = binding.settings;
  const expanded = binding.expanded;
  const sameSurface = prev !== null && prev.width === width && prev.height === height && prev.dpr === dpr && prev.theme === theme;
  if (prev && prev.model === model && prev.options === binding.options && prev.settings === settings && prev.expanded === expanded && sameSurface && (playing || !prev.heldWidths)) return prev;
  const sortSig = `${settings.sort.column}\u0000${settings.sort.direction}`;
  const sameSettings = prev !== null && (prev.settings === settings || sameFieldValue(prev.settings, settings));
  const sameExpanded = prev !== null && (prev.expanded === expanded || sameFieldValue(prev.expanded, expanded));
  const sameView = sameSettings && sameExpanded && (prev.options === binding.options || sameFieldValue(prev.options, binding.options));
  const inc = sameView && step.previous?.layout.shape === prev && prev.model !== model && model.changedKeys !== undefined && !prev.hasGroups && !prev.panes && model.panes === undefined
    ? resolveTableRowsIncremental(prev.model, prev.flatRows, prev.rowIndex, model, settings)
    : null;
  const rowHeightMode = model.rowHeightMode ?? 'standard';
  let flatRows: readonly FlatTableRow[];
  let rowIndex: Map<string, number>;
  let hasGroups = false;
  let variableHeightCount = 0;
  let panes: readonly ResolvedPane[] | null = null;
  const isVariableRow = (item: FlatTableRow): boolean =>
    item.kind !== 'data' || item.level > 1 || item.isLiveEnd === true || item.row.detailColumn !== undefined;
  if (inc) {
    flatRows = inc.flatRows;
    rowIndex = inc.rowIndex;
    variableHeightCount = prev!.variableHeightCount;
    if (model.changedKeys) {
      for (const key of model.changedKeys) {
        const oldAt = prev!.rowIndex.get(key);
        const newAt = rowIndex.get(key);
        const oldVar = oldAt !== undefined && isVariableRow(prev!.flatRows[oldAt]!);
        const newVar = newAt !== undefined && isVariableRow(flatRows[newAt]!);
        if (oldVar && !newVar) variableHeightCount--;
        else if (!oldVar && newVar) variableHeightCount++;
      }
    }
  } else {
    panes = model.panes ? resolvePanes(model, settings, expanded, rowHeightMode) : null;
    flatRows = panes ? combinedPaneRows(panes) : resolveTableRows(model, settings, expanded);
    rowIndex = new Map();
    for (let i = 0; i < flatRows.length; i++) {
      const item = flatRows[i]!;
      if (item.kind === 'group-header') {
        hasGroups = true;
        variableHeightCount++;
      } else if (item.kind !== 'data') {
        variableHeightCount++;
        if (item.kind === 'gap' || item.kind === 'refused') rowIndex.set(item.key, i);
      } else {
        if (item.level > 1 || item.isLiveEnd === true || item.row.detailColumn !== undefined) variableHeightCount++;
        rowIndex.set(item.key, i);
      }
    }
  }
  const hasVariableHeights = variableHeightCount > 0;
  const { shown: rankShown, markers: rankMarkersOn, width: rankWidth } = tableRank(model, settings);
  const paneGeom = panes ? paneGeometry(width, panes.length) : null;
  const paneStarts: number[] = [];
  if (panes) for (let p = 0, at = 0; p < panes.length; at += panes[p]!.rows.length + 1, p++) paneStarts.push(at);
  const layoutWidth = paneGeom ? paneGeom.width : width;
  const baseCols = buildColumnLayouts(model.columns, settings);
  const samples = textSamples(baseCols, flatRows);
  const flags = flagCounts(baseCols, flatRows);
  const widthKey = `${widthConfigOf(model.columns, settings, theme, model.hideHeader === true)}${JSON.stringify(flags)}`;
  const measureText = canvasMeasureText(measure);
  const sameColumns = prev !== null && sameFieldValue(prev.model.columns, model.columns);
  const measuredWidths = prev && sameColumns && prev.widthKey === widthKey && sameFieldValue(prev.samples, samples)
    ? prev.measuredWidths
    : measureFixedColumnWidths(baseCols, settings, theme, measureText, samples, { hideHeader: model.hideHeader === true, flags });
  const fixedWidths = prev && playing && sameView && sameSurface && sameColumns && prev.model.hideHeader === model.hideHeader
      && (prev.paneGeom?.width ?? prev.width) === layoutWidth
    ? holdColumnWidths(measuredWidths, prev.fixedWidths) : measuredWidths;
  const heldWidths = fixedWidths !== measuredWidths && Object.keys(fixedWidths).some(key => fixedWidths[key] !== measuredWidths[key]);
  const allLayouts = prev && sameSettings && prev.fixedWidths === fixedWidths && sameColumns
    ? prev.allLayouts : buildColumnLayouts(model.columns, settings, fixedWidths);
  const columns = prev && prev.allLayouts === allLayouts && (prev.paneGeom?.width ?? prev.width) === layoutWidth &&
    prev.rankShown === rankShown && prev.rankWidth === rankWidth && prev.model.rhythm === model.rhythm
    ? prev.columns : layoutFamilyTable(allLayouts, Math.max(1, layoutWidth), { rankShown, rankWidth, rhythm: model.rhythm });
  const rowHeight = rowHeightMode === 'media' ? MEDIA_ROW_HEIGHT[settings.density] : ROW_HEIGHT[settings.density];
  const windowOffset = panes ? 0 : (model.windowOffset ?? 0);
  const totalCount = panes ? flatRows.length : Math.max(windowOffset + flatRows.length, model.totalRows ?? flatRows.length);
  const basis: HeightBasis = { options: binding.options, settings, expanded, width, theme, rowHeight, totalCount };
  const heldRows = panes ? null : heldHeights(measure, basis, flatRows, windowOffset);
  const rowGeom = hasVariableHeights ? computeRowLayout(flatRows, settings.density, rowHeightMode,
    { width, columns: columns.columns, theme, measure: measureText }, heldRows ? index => heldRows.get(windowOffset + index)?.height : undefined) : null;
  const localTotal = panes ? panesHeight(panes) : rowGeom ? rowGeom.totalHeight : flatRows.length * rowHeight;
  const heightMemory = panes ? new Map<number, MeasuredRow>() : rememberHeights(measure, basis, heldRows, flatRows, rowGeom, windowOffset);
  const extents = rowExtents(rowHeight, [...heightMemory].map(([ordinal, row]) => [ordinal, row.height] as const));
  const totalHeight = panes ? localTotal : extentTop(extents, rowHeight, totalCount) + (model.endNote ? END_NOTE_HEIGHT : 0);
  let maximaCols = '';
  for (const col of columns.columns) if (col.numeric) maximaCols += `${col.key}:${col.display}|`;
  const maxima = inc && prev && prev.maximaCols === maximaCols && model.changedKeys
    ? (model.changedKeys.size === 0
      ? prev.maxima
      : updateColumnMaximaIncremental(prev.maxima, prev.flatRows, prev.rowIndex, flatRows, rowIndex, columns.columns, model.changedKeys))
    : computeColumnMaxima(flatRows, columns.columns);
  const dataKeys = flatRows.filter(item => item.kind === 'data').map(item => item.key);
  const treeLevels = inc && prev ? prev.treeLevels : panes ? panes.map(pane => treeLevelsOf(pane.rows)) : treeLevelsOf(flatRows);
  const shape: TableShape = {
    model, options: binding.options, settings, expanded, width, height, dpr, theme, measureText,
    flatRows, rowIndex, hasGroups, incremental: inc !== null,
    treeLevels, hasTree: inc === null && (typeof treeLevels === 'number' ? treeLevels !== 0 : treeLevels.some(levels => levels !== 0)),
    panes, paneGeom, paneSpans: panes ? combinedPaneGeometry(panes) : null, paneStarts, rankShown, rankMarkersOn, rankWidth,
    widthKey, samples, measuredWidths, fixedWidths, heldWidths, allLayouts, columns, rowHeightMode, rowHeight, rowGeom, variableHeightCount,
    windowOffset, totalCount, totalHeight, extents, maxima, maximaCols,
    rowAuditCache: inc && prev ? prev.rowAuditCache : new WeakMap(),
    sortSig, orderKeys: dataKeys,
  };
  return shape;
}

function layoutTable(model: TableModel, binding: FamilyTableBinding, surface: Surface, theme: FamilyTheme, measure: OffscreenCanvasRenderingContext2D, _hour: number, step: LaneStep<TableModel, TableLayout>): TableLayout {
  const shape = shapeOf(model, binding, theme, surface, measure, step);
  const { panes, rowGeom, rowHeight, flatRows } = shape;
  const windowTop = panes ? 0 : extentTop(shape.extents, rowHeight, shape.windowOffset);
  const localScrollTop = Math.max(0, binding.scrollTop - windowTop);
  const focused = binding.focusedKey;
  const focusedIndex = focused ? (shape.rowIndex.get(focused) ?? -1) : -1;
  const previous = step.previousLayout;
  const columnWidth = shape.paneGeom?.width ?? surface.width;
  const left = Math.max(0, Math.min(binding.scrollLeft, shape.columns.totalWidth - columnWidth));
  if (previous?.shape === shape && previous.localScrollTop === localScrollTop && previous.scrollLeft === left && previous.focusedIndex === focusedIndex) return previous;
  const spans = shape.paneSpans;
  const rowWindow = spans ? { first: 0, count: flatRows.length } : tableRowWindow(flatRows.length, rowGeom, rowHeight, localScrollTop, surface.height);
  const shownWindows = panes
    ? panes.flatMap((pane, p) => {
      const [first, last] = visiblePaneRange(pane.geom, pane.rows.length, localScrollTop, surface.height);
      return [{ first: shape.paneStarts[p]!, count: 1 }, { first: shape.paneStarts[p]! + 1 + first, count: last - first }];
    })
    : [rowWindow];
  const tops = (index: number) => (spans ? spans.tops[index]! : (rowGeom ? rowGeom.tops[index]! : index * rowHeight) + windowTop);
  const heights = (index: number) => (spans ? spans.heights[index]! : rowGeom ? rowGeom.heights[index]! : rowHeight);
  const reuseVisibleCols = previous
    && previous.shape.columns === shape.columns
    && previous.shape.settings.barsBehind === shape.settings.barsBehind
    && previous.shape.settings.sort.column === shape.settings.sort.column
    && previous.scrollLeft === left
    && !!previous.shape.panes === !!panes;
  const shownCols = reuseVisibleCols ? null : columnsOnScreen(shape, columnWidth, left);
  const visibleColumns = reuseVisibleCols ? previous.visibleColumns : shownCols?.columns ?? null;
  const visibleColumnStarts = reuseVisibleCols ? previous.visibleColumnStarts : shownCols?.starts ?? null;
  const visibleRows = panes ? shownWindows : [visibleRange(shape, localScrollTop, surface.height)];
  return { shape, localScrollTop, rowWindow, shownWindows, tops, heights, focusedIndex, height: surface.height, visibleRows, visibleColumns, visibleColumnStarts, scrollLeft: left };
}

function drawsBehindBar(col: FamilyColumnLayout, settings: TableSettings): boolean {
  const barsMode = settings.barsBehind ?? 'all';
  if (barsMode === 'off' || col.barEligible === false) return false;
  if (col.barEligible === true || col.display === 'faint') {
    return barsMode !== 'sorted' || col.key === settings.sort.column;
  }
  return false;
}

function canSpillLeft(col: FamilyColumnLayout, x: number, width: number, settings: TableSettings): boolean {
  if (col.headerGroup || col.display === 'track' || col.display === 'meterBefore' || col.display === 'meterAfter') return true;
  switch (col.cellKind) {
    case undefined: case 'standard': case 'item': case 'country': case 'numeric': case 'status':
      return !!col.numeric && drawsBehindBar(col, settings) && x + col.width - (col.trailInset ?? 0) - CELL_INSET + BEHIND_BAR_OFFSET - BEHIND_BAR_MAX_WIDTH < width;
    default:
      return true;
  }
}

function columnsOnScreen(shape: TableShape, width: number, left: number): { readonly columns: FamilyColumnLayout[]; readonly starts: number[] } | null {
  const layout = shape.columns;
  const shown: FamilyColumnLayout[] = [];
  const starts: number[] = [];
  let inView = 0;
  let x = shape.rankShown && !shape.panes ? shape.rankWidth : 0;
  const pinnedCover = layout.scrollable ? layout.pinnedWidth : 0;
  for (let i = 0; i < layout.columns.length; i++) {
    const col = layout.columns[i]!;
    const pinned = i < layout.pinnedCount;
    const start = pinned ? x : x - left;
    const minLeft = pinned ? 0 : pinnedCover;
    if (col.width > 0 && start + col.width > minLeft) {
      if (start < width) inView++;
      if (start < width || canSpillLeft(col, start, width, shape.settings)) {
        shown.push(col);
        starts.push(start);
      }
    }
    x += col.width;
  }
  return shown.length - inView <= inView ? { columns: shown, starts } : null;
}

interface ModelMotion {
  readonly continuity: number;
  readonly sortSig: string;
  readonly markers: ReadonlyMap<string, RankMarker>;
  readonly expiries: readonly number[];
  readonly glideOverride?: number;
  offsets: ReadonlyMap<string, GlideOffset>;
  glideEnd: number;
}

const EMPTY_OFFSETS: ReadonlyMap<string, GlideOffset> = new Map();
const modelMotions = new WeakMap<TableModel, ModelMotion>();
const shapeMotions = new WeakMap<TableShape, { readonly model: ModelMotion; readonly offsets: ReadonlyMap<string, GlideOffset>; readonly motion: TableMotion }>();
const paintedGlides = new WeakMap<TableFrame, { readonly duration: number; readonly glide: TableGlide }>();

function motionOf(frame: TableFrame): TableMotion {
  const shape = frame.layout.shape;
  let mm = modelMotions.get(shape.model);
  if (!mm || mm.continuity !== frame.continuity) {
    mm = transition(shape, frame);
    modelMotions.set(shape.model, mm);
  } else if (mm.sortSig !== shape.sortSig) {
    mm = { continuity: frame.continuity, sortSig: shape.sortSig, markers: new Map(), expiries: [], offsets: EMPTY_OFFSETS, glideEnd: -Infinity };
    modelMotions.set(shape.model, mm);
  }
  if (glideDurationForSpeed(frame.speed, frame.binding.reducedMotion === true, frame.binding.motion?.glideDurationMs) <= 0 && mm.offsets.size > 0) {
    mm.offsets = EMPTY_OFFSETS;
    mm.glideEnd = -Infinity;
  }
  const held = shapeMotions.get(shape);
  if (held && held.model === mm && held.offsets === mm.offsets) return held.motion;
  const glide = mm.offsets.size > 0
    ? tableGlide(mm.offsets, shape.flatRows, shape.rowIndex, shape.rowGeom, shape.rowHeight)
    : NO_GLIDE;
  const motion: TableMotion = { markers: mm.markers, expiries: mm.expiries, glide, glideEnd: mm.glideEnd };
  shapeMotions.set(shape, { model: mm, offsets: mm.offsets, motion });
  return motion;
}

function transition(shape: TableShape, frame: TableFrame): ModelMotion {
  const prevLayout = frame.before?.layout;
  const prev = prevLayout?.shape;
  if (!prev || !prevLayout || prev.sortSig !== shape.sortSig || shape.model.hour <= prev.model.hour) {
    return { continuity: frame.continuity, sortSig: shape.sortSig, markers: new Map(), expiries: [], offsets: EMPTY_OFFSETS, glideEnd: -Infinity };
  }
  const prior = modelMotions.get(prev.model);
  const priorMarkers = prior && prior.sortSig === prev.sortSig ? prior.markers : new Map<string, RankMarker>();
  const priorOffsets = prior && prior.sortSig === prev.sortSig ? prior.offsets : EMPTY_OFFSETS;
  const started = frame.changedAt;
  const markers = shape.rankMarkersOn
    ? rankMarkers(prev.orderKeys, shape.orderKeys, started, priorMarkers, rankHoldForSpeed(frame.speed))
    : new Map<string, RankMarker>();
  const glideMs = glideDurationForSpeed(frame.speed, frame.binding.reducedMotion === true, frame.binding.motion?.glideDurationMs);
  const glide = new Map<string, GlideOffset>();
  if (glideMs > 0 && !shape.panes && !prev.panes) {
    const currLayout = frame.layout;
    const oldIndex = new Map<string, number>();
    prev.flatRows.forEach((item, index) => { if (item.kind === 'data') oldIndex.set(item.key, index); });
    shape.flatRows.forEach((item, index) => {
      if (item.kind !== 'data') return;
      const before = oldIndex.get(item.key);
      if (before === undefined) return;
      const prevTop = prevLayout.tops(before);
      const nextTop = currLayout.tops(index);
      const existing = priorOffsets.get(item.key);
      if (prevTop === nextTop) {
        if (existing && started < existing.started + (existing.duration ?? glideMs)) glide.set(item.key, existing);
        return;
      }
      const paintedBefore = prevTop + glideOffset(existing, started, glideMs);
      const delta = paintedBefore - nextTop;
      if (delta !== 0) glide.set(item.key, { from: delta, to: 0, started, duration: glideMs });
    });
  }
  const expiries = [...new Set([...markers.values()].map(marker => marker.until))].sort((a, b) => a - b);
  let glideEnd = -Infinity;
  for (const offset of glide.values()) {
    const end = offset.started + (offset.duration ?? glideMs);
    if (end > glideEnd) glideEnd = end;
  }
  return { continuity: frame.continuity, sortSig: shape.sortSig, markers, expiries, glideOverride: frame.binding.motion?.glideDurationMs, offsets: glide, glideEnd };
}

function liveGlide(frame: TableFrame): { readonly duration: number; readonly glide: TableGlide } {
  const duration = glideDurationForSpeed(frame.speed, frame.binding.reducedMotion === true, frame.binding.motion?.glideDurationMs);
  const motion = motionOf(frame);
  return { duration, glide: duration > 0 && frame.now < motion.glideEnd ? motion.glide : NO_GLIDE };
}

function nextExpiry(motion: TableMotion, now: number): number {
  for (const until of motion.expiries) if (until > now) return until;
  return Infinity;
}

function visibleRange(shape: TableShape, localScrollTop: number, height: number): { first: number; count: number } {
  const { rowGeom, rowHeight, flatRows } = shape;
  if (rowGeom) {
    let lo = 0;
    let hi = flatRows.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (rowGeom.tops[mid]! + rowGeom.heights[mid]! <= localScrollTop) lo = mid + 1;
      else hi = mid;
    }
    let end = lo;
    while (end < flatRows.length && rowGeom.tops[end]! < localScrollTop + height) end++;
    return { first: lo, count: end - lo };
  }
  const first = Math.max(0, Math.floor(localScrollTop / rowHeight));
  return { first, count: Math.max(0, Math.min(flatRows.length, Math.ceil((localScrollTop + height) / rowHeight)) - first) };
}

function sameOtherCellFields(a: TableCell, b: TableCell): boolean {
  for (const key in a) {
    if (key !== 'value' && !sameFieldValue(a[key as keyof TableCell], b[key as keyof TableCell])) return false;
  }
  for (const key in b) if (!Object.hasOwn(a, key) && b[key as keyof TableCell] !== undefined) return false;
  return true;
}

function sameRoundedFigure(a: TableCell, b: TableCell, col: FamilyColumnLayout): boolean {
  if (!col.numeric || (col.cellKind && col.cellKind !== 'numeric' && col.cellKind !== 'standard') ||
      a.value.kind !== 'known' || b.value.kind !== 'known') return false;
  const left = a.value.value, right = b.value.value;
  const format = b.format ?? col.valueFormat;
  if (typeof left !== 'number' || typeof right !== 'number' || Math.sign(left) !== Math.sign(right) ||
      Math.trunc(left) !== Math.trunc(right) ||
      (format && format !== 'integer' && !(typeof format === 'object' && (format.decimals > 0 || !format.maxDecimals)))) return false;
  return sameOtherCellFields(a, b);
}

function sameRightClippedFigure(
  ctx: OffscreenCanvasRenderingContext2D,
  a: TableCell,
  b: TableCell,
  col: FamilyColumnLayout,
  colStart: number,
  vpW: number,
  theme: FamilyTheme,
  hideHeader: boolean,
): boolean {
  if (hideHeader || !col.numeric || col.headerGroup || col.display === 'track' || col.display === 'meterAfter' || col.display === 'meterBefore') return false;
  if (col.cellKind && col.cellKind !== 'numeric' && col.cellKind !== 'standard') return false;
  if (a.value.kind !== 'known' || b.value.kind !== 'known') return false;
  if (a.unitSuffix || b.unitSuffix || a.unitArt || b.unitArt || a.splitBar?.length || b.splitBar?.length || a.blank || b.blank) return false;
  const right = colStart + col.width - CELL_INSET - (col.trailInset ?? 0);
  if (right <= vpW) return false;
  if (!sameOtherCellFields(a, b)) return false;
  const format = b.format ?? col.valueFormat;
  const textA = formatScalar(a.value.value, format);
  const textB = formatScalar(b.value.value, format);
  if (textA === textB) return true;
  const font = `500 13px ${theme.font}`;
  const left = colStart + CELL_INSET + (col.leadInset ?? 0);
  const avail = Math.max(0, right - left);
  const wA = measureCanvasText(ctx, textA, font);
  const wB = measureCanvasText(ctx, textB, font);
  if (wA !== wB || wA > avail) return false;
  let prefix = 0;
  const limit = Math.min(textA.length, textB.length);
  while (prefix < limit && textA.charCodeAt(prefix) === textB.charCodeAt(prefix)) prefix++;
  const prefixW = prefix > 0 ? measureCanvasText(ctx, textA.slice(0, prefix), font) : 0;
  return right - wA + prefixW >= vpW + 2;
}

function sameBarPixels(
  a: TableCell,
  b: TableCell,
  col: FamilyColumnLayout,
  oldMax: number | undefined,
  newMax: number | undefined,
  dpr: number,
  settings: TableSettings,
): boolean {
  const kind = col.cellKind;
  if (kind === 'twoWay' || kind === 'shaded' || kind === 'share' || kind === 'track' || col.display === 'track' || col.display === 'meterAfter' || col.display === 'meterBefore') {
    const magnitude = kind === 'twoWay' || kind === 'shaded';
    const span = barPixelSpan(col, dpr);
    return barPixels(cellBarFraction(a, a.value, a.maximum ?? oldMax, magnitude), span) ===
      barPixels(cellBarFraction(b, b.value, b.maximum ?? newMax, magnitude), span);
  }
  if (!drawsBehindBar(col, settings)) return true;
  const span = barPixelSpan(col, dpr);
  const behindWidth = (cell: TableCell, max: number | undefined): number => {
    if (cell.value.kind === 'zero') return 0;
    const raw = cellBarFraction(cell, cell.value, cell.maximum ?? max, false);
    if (raw === null) return 0;
    const quantized = barPixels(raw, span) / span;
    const clamped = Math.max(0, Math.min(1, quantized));
    if (clamped <= 0) return 0;
    return Math.min(BEHIND_BAR_MAX_WIDTH, Math.max(BEHIND_BAR_MIN_WIDTH, Math.round(BEHIND_BAR_MAX_WIDTH * clamped)));
  };
  return behindWidth(a, oldMax) === behindWidth(b, newMax);
}

function sameVisibleCell(
  ctx: OffscreenCanvasRenderingContext2D,
  a: TableCell | undefined,
  b: TableCell | undefined,
  col: FamilyColumnLayout,
  colStart: number,
  vpW: number,
  oldMax: number | undefined,
  newMax: number | undefined,
  dpr: number,
  settings: TableSettings,
  theme: FamilyTheme,
  hideHeader: boolean,
): boolean {
  if (!a || !b) return a === b;
  const same = sameFieldValue(a, b);
  if (!same && !sameRoundedFigure(a, b, col) && !sameRightClippedFigure(ctx, a, b, col, colStart, vpW, theme, hideHeader)) return false;
  if (same && (oldMax === newMax || a.bar !== undefined || a.maximum !== undefined)) return true;
  return sameBarPixels(a, b, col, oldMax, newMax, dpr, settings);
}

function isPlainSingleLineCell(cell: TableCell | undefined, col: FamilyColumnLayout): boolean {
  if (!cell) return true;
  if (cell.subtext || cell.splitBar?.length || cell.artShape || cell.glyph || cell.quantities?.length) return false;
  if (cell.iconSlot && showsIcon(col.display) && col.display !== 'flag' && col.display !== 'flagName') return false;
  return true;
}

function rowInkBelowCanvas(
  layout: TableLayout,
  columns: readonly FamilyColumnLayout[],
  i: number,
  a: Extract<FlatTableRow, { kind: 'data' }>,
  b: Extract<FlatTableRow, { kind: 'data' }>,
  inspectRow: string | null,
  selectedKey: string | null,
  focusedKey: string | null,
): boolean {
  const shape = layout.shape;
  if (shape.panes || shape.rowHeightMode !== 'standard' || shape.hasTree) return false;
  if (a.level > 1 || b.level > 1 || a.isTreeGroup || b.isTreeGroup || a.isLiveEnd || b.isLiveEnd || a.row.detailColumn || b.row.detailColumn) return false;
  if (a.key === inspectRow || b.key === inspectRow || a.key === selectedKey || b.key === selectedKey || a.key === focusedKey || b.key === focusedKey) return false;
  const localTop = shape.rowGeom ? shape.rowGeom.tops[i]! : i * shape.rowHeight;
  const y = localTop - layout.localScrollTop;
  const rowH = layout.heights(i);
  const inkHalf = Math.max(BEHIND_BAR_HEIGHT[shape.settings.density] / 2, shape.theme.fontSize.small * 0.75, 9.5) + 2;
  if (y + rowH / 2 - inkHalf < layout.height) return false;
  for (let c = 0; c < columns.length; c++) {
    const col = columns[c]!;
    if (col.headerGroup || col.inlineFlags?.length) return false;
    const kind = col.cellKind;
    if (kind !== undefined && kind !== 'standard' && kind !== 'country' && kind !== 'numeric' && kind !== 'item' && kind !== 'outOf' && kind !== 'track') return false;
    if (!isPlainSingleLineCell(a.row.cells[col.key], col) || !isPlainSingleLineCell(b.row.cells[col.key], col)) return false;
  }
  return true;
}

function samePaintedRow(
  ctx: OffscreenCanvasRenderingContext2D,
  previous: TableLayout,
  layout: TableLayout,
  columns: readonly FamilyColumnLayout[],
  starts: readonly number[],
  vpW: number,
  i: number,
  dpr: number,
  scaleChanged: boolean,
  inspectRow: string | null,
  selectedKey: string | null,
  focusedKey: string | null,
): boolean {
  const a = previous.shape.flatRows[i]!;
  const b = layout.shape.flatRows[i]!;
  if (a.kind !== 'data' || b.kind !== 'data') return sameFieldValue(a, b);
  if (rowInkBelowCanvas(layout, columns, i, a, b, inspectRow, selectedKey, focusedKey)) return true;
  if (a.key !== b.key || a.name !== b.name || a.level !== b.level || a.isTreeGroup !== b.isTreeGroup ||
      a.isOpen !== b.isOpen || a.isDayEnd !== b.isDayEnd || a.isLiveEnd !== b.isLiveEnd || a.isTotal !== b.isTotal ||
      a.showDate !== b.showDate || (layout.shape.rankShown && a.rank !== b.rank) ||
      a.row.muted !== b.row.muted || a.row.pageParent !== b.row.pageParent || !!a.row.pageGroup !== !!b.row.pageGroup ||
      a.row.detailColumn !== b.row.detailColumn) return false;
  if (a.level > 1) {
    const afterA = previous.shape.flatRows[i + 1];
    const afterB = layout.shape.flatRows[i + 1];
    if ((afterA?.kind === 'data' && afterA.level > 1) !== (afterB?.kind === 'data' && afterB.level > 1)) return false;
  }
  if (b.row.detailColumn && (!sameFieldValue(previous.shape.rowGeom?.detailRows?.get(i), layout.shape.rowGeom?.detailRows?.get(i)) ||
      !sameFieldValue(a.row.cells[b.row.detailColumn], b.row.cells[b.row.detailColumn]))) return false;
  if (a.row === b.row && !scaleChanged) return true;
  const settings = layout.shape.settings;
  const theme = layout.shape.theme;
  const hideHeader = layout.shape.model.hideHeader === true;
  for (let c = 0; c < columns.length; c++) {
    const col = columns[c]!;
    if (!sameVisibleCell(ctx, a.row.cells[col.key], b.row.cells[col.key], col, starts[c]!, vpW, previous.shape.maxima.get(col.key), layout.shape.maxima.get(col.key), dpr, settings, theme, hideHeader)) return false;
    if (col.inlineFlags) for (let f = 0; f < col.inlineFlags.length; f++) {
      const key = col.inlineFlags[f]!;
      if (!sameFieldValue(a.row.cells[key], b.row.cells[key])) return false;
    }
  }
  return true;
}

function sameViewport(
  ctx: OffscreenCanvasRenderingContext2D,
  before: TableLayout,
  after: TableLayout,
  dpr: number,
  inspectRow: string | null,
  selectedKey: string | null,
  focusedKey: string | null,
): boolean {
  if (before === after) return true;
  const a = before.shape, b = after.shape;
  if ((a.theme !== b.theme && !sameFieldValue(a.theme, b.theme)) ||
      (a.settings !== b.settings && !sameFieldValue(a.settings, b.settings)) ||
      (a.columns !== b.columns && !sameFieldValue(a.columns, b.columns)) ||
      a.width !== b.width ||
      before.height !== after.height || before.localScrollTop !== after.localScrollTop || before.scrollLeft !== after.scrollLeft ||
      a.windowOffset !== b.windowOffset || a.rowHeightMode !== b.rowHeightMode || a.hasTree !== b.hasTree ||
      a.model.dateGrouped !== b.model.dateGrouped || a.model.dayDividersOnly !== b.model.dayDividersOnly ||
      a.model.hideHeader !== b.model.hideHeader || a.model.empty !== b.model.empty || a.model.endNote !== b.model.endNote ||
      (b.model.endNote && (a.totalCount !== b.totalCount || a.totalHeight !== b.totalHeight || a.flatRows.length !== b.flatRows.length)) ||
      !sameFieldValue(a.model.clockSummary, b.model.clockSummary) || !sameFieldValue(a.paneGeom, b.paneGeom)) return false;
  const columns = after.visibleColumns;
  const starts = after.visibleColumnStarts;
  if (!columns || !starts || !sameFieldValue(a.treeLevels, b.treeLevels)) return false;
  const vpW = b.paneGeom?.width ?? b.width;
  let scaleChanged = false;
  for (let c = 0; c < columns.length && !scaleChanged; c++) {
    const key = columns[c]!.key;
    scaleChanged = a.maxima.get(key) !== b.maxima.get(key);
  }
  if (before.visibleRows.length !== after.visibleRows.length) return false;
  for (let w = 0; w < after.visibleRows.length; w++) {
    const win = after.visibleRows[w]!, old = before.visibleRows[w]!;
    if (win.first !== old.first || win.count !== old.count) return false;
    for (let i = win.first; i < win.first + win.count; i++) {
      if (before.tops(i) !== after.tops(i) || before.heights(i) !== after.heights(i) || !samePaintedRow(ctx, before, after, columns, starts, vpW, i, dpr, scaleChanged, inspectRow, selectedKey, focusedKey)) return false;
    }
  }
  return true;
}

const shapeFieldCache = new WeakMap<TableShape, { readonly width: number; readonly fields: Readonly<Record<string, unknown>> }>();

function shapeFields(shape: TableShape, layout: TableLayout, width: number): Readonly<Record<string, unknown>> {
  const held = shapeFieldCache.get(shape);
  if (held && held.width === width) return held.fields;
  const { model, columns, allLayouts, paneGeom, panes, flatRows } = shape;
  const fields: Record<string, unknown> = {
    widths: {
      widths: Object.fromEntries([...allLayouts.filter(col => col.shown), ...columns.columns].map(col => [col.key, col.width])),
      visible: columns.columns.map(col => col.key),
      totals: columns.columns.filter(col => col.narrow === 'total' && !col.headerGroup).map(col => col.key),
      narrowed: columns.narrowed,
      totalWidth: paneGeom ? width + Math.max(0, columns.totalWidth - paneGeom.width) : columns.totalWidth,
      scrollable: columns.scrollable,
      pinnedCount: columns.pinnedCount,
    },
    meta: {
      empty: model.empty ?? null,
      emptyDetail: model.emptyDetail ?? null,
      emptyKind: model.emptyKind ?? 'empty',
      summaryText: model.titleSummary ?? null,
      summaryPartial: model.titleSummaryPartial ?? null,
      hideHeader: model.hideHeader === true,
      rhythm: model.rhythm,
      kindFilter: model.kindFilter,
      searchable: model.searchable === true,
      search: model.search,
      subjectSummary: model.subjectSummary,
      rowHeightMode: shape.rowHeightMode,
      notes: [],
      warnings: model.warnings ?? [],
      hasTree: shape.hasTree,
      unsorted: keepsProductOrder(model),
      rankShown: shape.rankShown,
      rankWidth: shape.rankWidth,
      columns: model.columns,
    },
    nav: flatRows.flatMap((item, i) => (item.kind === 'data'
      ? [[item.key, item.name, shape.windowOffset + i, item.isTreeGroup ? (item.isOpen ? 1 : 0) : -1, item.parent ?? '', item.row.meta ?? null, layout.tops(i), layout.heights(i)] as const]
      : [])),
  };
  const description = [model.titleSummary, model.titleSummaryPartial].filter((part): part is string => Boolean(part)).join('. ');
  if (description) fields.accessibleDescription = description;
  if (panes && paneGeom) {
    fields.panes = {
      width: paneGeom.width,
      gutter: paneGeom.gutter,
      xs: paneGeom.xs,
      tableWidth: columns.totalWidth,
      titles: panes.map(pane => ({ key: pane.key, name: paneTitleText(pane.title), nameState: valueStateProps(pane.title.name), missing: pane.title.name.kind === 'notRecorded', flag: pane.title.flag ?? null, flagPending: pane.title.flagPending === true, country: pane.title.country ?? null })),
    };
  }
  shapeFieldCache.set(shape, { width, fields });
  return fields;
}

const inspectedRow = (inspect: string | null): string | null => (inspect?.startsWith('row:') ? inspect.slice(4) : null);

function snapshotVisibleArt(
  layout: TableLayout,
  binding: FamilyTableBinding,
  artCache: LaneArtCache,
  inspectRow: string | null,
): readonly (readonly [string, unknown])[] {
  const shape = layout.shape;
  const cols = layout.visibleColumns ?? shape.columns.columns;
  let urls: string[];
  if (shape.panes) {
    urls = collectArtUrls(shape.panes.flatMap(pane => {
      const [first, last] = visiblePaneRange(pane.geom, pane.rows.length, binding.scrollTop, layout.height);
      return visibleCellArtItems(pane.rows, cols, first, last);
    }));
  } else {
    const { first, count } = layout.visibleRows[0]!;
    const selected = binding.selectedKey ?? null;
    const focused = binding.focusedKey ?? null;
    let end = first + count;
    while (end > first) {
      const row = shape.flatRows[end - 1];
      if (!row || row.kind !== 'data' || !rowInkBelowCanvas(layout, cols, end - 1, row, row, inspectRow, selected, focused)) break;
      end--;
    }
    urls = collectArtUrls(visibleCellArtItems(shape.flatRows, cols, first, end));
  }
  const states = new Array<readonly [string, unknown]>(urls.length);
  for (let i = 0; i < urls.length; i++) {
    const url = urls[i]!;
    states[i] = [url, artCache.bitmapOf(url) ?? artCache.failureOf(url) ?? null];
  }
  return states;
}

function sameVisibleArtStates(
  states: readonly (readonly [string, unknown])[] | undefined,
  artCache: LaneArtCache,
): boolean {
  if (!states) return false;
  for (let i = 0; i < states.length; i++) {
    const [url, expected] = states[i]!;
    if ((artCache.bitmapOf(url) ?? artCache.failureOf(url) ?? null) !== expected) return false;
  }
  return true;
}

export const tableLane: LaneFamily<FamilyTableBinding, TableModel, TableLayout> = {
  family: 'table',
  boundary: nextTableChange,
  redraw: () => Infinity,
  status(model) {
    if (model.refused !== undefined) return { refused: model.refused };
    const handled = model.rows.length > 0 || model.panes !== undefined || (model.slots?.length ?? 0) > 0 || model.empty !== undefined || model.clockSummary !== undefined;
    return handled ? 'picture' : 'scene';
  },
  view: ['settings', 'expanded', 'scrollTop', 'scrollLeft', 'focusedKey', 'reducedMotion'],
  layoutTracksPlayback: true,
  layoutHeld: ({ shape }) => shape.heldWidths,
  layout: layoutTable,
  onSpeed(layout, speed, continuity) {
    const mm = modelMotions.get(layout.shape.model);
    if (!mm || mm.continuity !== continuity) return;
    if (glideDurationForSpeed(speed, false, mm.glideOverride) > 0) return;
    mm.offsets = EMPTY_OFFSETS;
    mm.glideEnd = -Infinity;
  },
  keepsCanvas(ctx, frame) {
    const last = pictures.get(contextWidgetKey(ctx));
    if (!last?.still || last.dpr !== frame.surface.dpr ||
        last.inspect !== frame.inspect || last.selected !== frame.binding.selectedKey || last.focused !== frame.binding.focusedKey ||
        (last.art !== frame.art.version && !sameVisibleArtStates(last.artStates, frame.art)) ||
        !sameViewport(ctx, last.still, frame.layout, frame.surface.dpr, inspectedRow(frame.inspect), frame.binding.selectedKey ?? null, frame.binding.focusedKey ?? null)) return false;
    const motion = motionOf(frame);
    if ((motion.expiries.at(-1) ?? -Infinity) > frame.now) return false;
    const glide = motion.glide;
    const top = frame.layout.localScrollTop, bottom = top + frame.layout.height;
    const ok = !glide.offsets.size || glideDurationForSpeed(frame.speed, frame.binding.reducedMotion === true, frame.binding.motion?.glideDurationMs) <= 0 || frame.now >= motion.glideEnd ||
      !hasGlideCrossing(glide, top, bottom);
    if (ok) paintedGlides.set(frame, { duration: 0, glide: NO_GLIDE });
    return ok;
  },
  paint(ctx, frame) {
    const { layout, binding, model, theme } = frame;
    const shape = layout.shape;
    const live = liveGlide(frame);
    const gliding = live.glide.offsets.size > 0 && hasGlideCrossing(live.glide, layout.localScrollTop, layout.localScrollTop + layout.height);
    const { duration, glide } = gliding ? live : { duration: live.duration, glide: NO_GLIDE };
    paintedGlides.set(frame, { duration, glide });
    const motion = motionOf(frame);
    const picture = picturesFor(ctx, frame.surface, frame.art.version);
    const list = picture.list;
    picture.still = undefined;
    const common = {
      layout: shape.columns,
      settings: binding.settings,
      hour: model.hour,
      height: layout.height,
      theme,
      now: frame.now,
      inspectedKey: inspectedRow(frame.inspect),
      selectedKey: binding.selectedKey ?? null,
      focusedKey: binding.focusedKey ?? null,
      iconVersion: frame.art.version,
      artCache: frame.art,
      dpr: frame.surface.dpr > 0 ? frame.surface.dpr : 1,
      precomputedMaxima: shape.maxima,
      staticRowLooks: model.rowTimeBoundaries === false,
      rowHeightMode: shape.rowHeightMode,
      rhythm: model.rhythm,
      hideHeader: model.hideHeader === true,
    };
    let drew = false;
    if (shape.panes && shape.paneGeom) {
      const scroll = paneScroll(shape.columns, shape.paneGeom, binding.scrollLeft);
      for (let p = 0; p < shape.panes.length; p++) {
        const pane = shape.panes[p]!;
        const picture = list[p] ?? (list[p] = createTablePicture());
        if (frame.fullRepaint) picture.frame = null;
        ctx.save();
        ctx.translate(shape.paneGeom.xs[p]!, 0);
        ctx.beginPath();
        ctx.rect(0, 0, shape.paneGeom.width, layout.height);
        ctx.clip();
        const result = drawFamilyTable(ctx, {
          ...common,
          rows: pane.rows,
          width: shape.paneGeom.width,
          scrollLeft: scroll.left,
          scrollTop: binding.scrollTop,
          rankShown: false,
          rankWidth: 0,
          glideDuration: 0,
          glide: NO_GLIDE,
          hasGroupHeaders: true,
          rowGeom: pane.geom,
        }, picture);
        ctx.restore();
        drew = drew || result.drew;
      }
      if (drew) drawPaneFrame(ctx, shape.paneGeom, shape.columns, scroll, layout.height, theme);
    } else {
      const picture = list[0] ?? (list[0] = createTablePicture());
      if (frame.fullRepaint) picture.frame = null;
      drew = drawFamilyTable(ctx, {
        ...common,
        rows: shape.flatRows,
        width: frame.surface.width,
        scrollLeft: binding.scrollLeft,
        scrollTop: layout.localScrollTop,
        rankShown: shape.rankShown,
        rankWidth: shape.rankWidth,
        glideDuration: duration,
        glide,
        markers: motion.markers,
        empty: model.empty,
        hasGroupHeaders: shape.hasGroups,
        dateGrouped: model.dateGrouped === true,
        dayDividersOnly: model.dayDividersOnly,
        ...(model.endNote && shape.windowOffset + shape.flatRows.length >= shape.totalCount ? { endNote: model.endNote } : {}),
        rowGeom: shape.rowGeom,
        clockSummary: model.clockSummary,
      }, picture).drew;
    }
    if (gliding) return { drew, next: true };
    const until = nextExpiry(motion, frame.now);
    if (!Number.isFinite(until)) {
      picture.still = layout;
      picture.artStates = snapshotVisibleArt(layout, binding, frame.art, inspectedRow(frame.inspect));
      picture.dpr = frame.surface.dpr;
      picture.inspect = frame.inspect;
      picture.selected = binding.selectedKey;
      picture.focused = binding.focusedKey;
    }
    return { drew, next: Number.isFinite(until) ? until : false };
  },
  hit(frame, point) {
    const { model, layout, binding } = frame;
    const shape = layout.shape;
    if (model.clockSummary) return null;
    if (shape.panes && shape.paneGeom) {
      return hitPanes(shape.columns, shape.panes, shape.paneGeom, { x: point[0], y: point[1] }, shape.rowHeight, paneScroll(shape.columns, shape.paneGeom, binding.scrollLeft), binding.scrollTop);
    }
    const { duration, glide } = paintedGlides.get(frame) ?? liveGlide(frame);
    const at = hitFamilyTable(shape.columns, shape.flatRows, { x: point[0], y: point[1] }, shape.rowHeight, binding.scrollLeft, layout.localScrollTop, shape.rankShown, shape.rankWidth, glide, frame.now, duration, shape.rowGeom);
    if (at.kind !== 'row' || !at.rowKey) return null;
    return at.toggle ? `toggle:${at.rowKey}` : `row:${at.rowKey}`;
  },
  tooltip(frame, key, focus) {
    const rowKey = focus ? key : inspectedRow(key);
    if (!rowKey) return null;
    const content = deriveTableTooltip(frame.layout.shape.flatRows, rowKey, frame.model.columns, frame.layout.shape.rowIndex);
    return content ? { rowKey, content, ...(frame.model.tooltipHour !== undefined ? { hour: frame.model.tooltipHour } : {}) } : null;
  },
  structure({ layout, binding }) {
    const shape = layout.shape;
    const fields: Record<string, unknown> = {
      ...shapeFields(shape, layout, binding.width),
      table: {
        count: shape.totalCount,
        totalHeight: shape.totalHeight,
        ...(shape.extents ? { extents: shape.extents } : {}),
        focusedIndex: layout.focusedIndex,
        ...(shape.model.gapBefore?.length ? { gapBefore: shape.model.gapBefore } : {}),
      },
      range: layout.rowWindow,
    };
    return fields;
  },
  mirror({ model, layout, binding }) {
    const shape = layout.shape;
    const visibleSpecs = shape.columns.columns.map(col => model.columns.find(spec => spec.key === col.key)!).filter(Boolean);
    const base = deriveTableMirror(shape.flatRows, visibleSpecs, shape.rankShown, layout.shownWindows, binding.focusedKey ?? null, layout.focusedIndex, { offset: shape.windowOffset, total: shape.totalCount });
    return {
      ...base,
      columns: [...(shape.rankShown ? ['Rank'] : []), ...visibleSpecs.map(col => col.label)],
    };
  },
  audit({ model, layout }) {
    const shape = layout.shape;
    return deriveTableAudit({
      model,
      flatRows: shape.flatRows,
      visibleColumns: shape.columns.columns,
      rankShown: shape.rankShown,
      maxima: shape.maxima,
      window: layout.rowWindow,
      rowAuditCache: shape.rowAuditCache,
    });
  },
  visibleArt(frame) {
    const { layout, binding } = frame;
    const shape = layout.shape;
    const items: ArtUrlInput[] = [];
    const pinnedActive = shape.columns.scrollable && shape.columns.pinnedWidth > 0;
    const viewport = {
      width: shape.paneGeom?.width ?? frame.surface.width,
      scrollLeft: layout.scrollLeft,
      startX: shape.rankShown && !shape.panes ? shape.rankWidth : 0,
      pinnedCount: pinnedActive ? shape.columns.pinnedCount : 0,
      pinnedWidth: pinnedActive ? shape.columns.pinnedWidth : 0,
      artCache: frame.art,
      treeLevels: typeof shape.treeLevels === 'number' ? shape.treeLevels : 0,
      hideHeader: shape.model.hideHeader === true,
      fontSize: shape.theme.fontSize,
    };
    if (shape.panes) {
      for (let p = 0; p < shape.panes.length; p++) {
        const pane = shape.panes[p]!;
        const paneLevels = Array.isArray(shape.treeLevels) ? (shape.treeLevels[p] ?? 0) : viewport.treeLevels;
        const [first, last] = visiblePaneRange(pane.geom, pane.rows.length, binding.scrollTop, layout.height);
        const indices: number[] = [];
        for (let i = Math.max(0, first); i < Math.min(pane.rows.length, last); i++) indices.push(i);
        collectVisibleTableArt(
          pane.rows,
          indices,
          i => (pane.geom ? pane.geom.tops[i]! : i * shape.rowHeight) - binding.scrollTop,
          layout.height,
          pane.geom,
          shape.rowHeight,
          shape.columns.columns,
          binding.settings.density,
          { ...viewport, treeLevels: paneLevels },
          shape.theme.font,
          shape.measureText,
          false,
          items,
        );
      }
      return collectArtUrls(items);
    }
    const { first, count } = layout.visibleRows[0]!;
    const last = first + count;
    const live = liveGlide(frame);
    if (live.glide.offsets.size > 0) {
      collectVisibleGlidingArt(
        shape.flatRows,
        live.glide,
        first,
        last,
        layout.localScrollTop,
        layout.height,
        frame.now,
        live.duration,
        shape.rowGeom,
        shape.rowHeight,
        shape.columns.columns,
        binding.settings.density,
        viewport,
        shape.theme.font,
        shape.measureText,
        items,
      );
      return collectArtUrls(items);
    }
    const start = Math.max(0, first);
    const end = Math.min(shape.flatRows.length, last);
    const indices: number[] = [];
    for (let i = start; i < end; i++) indices.push(i);
    collectVisibleTableArt(
      shape.flatRows,
      indices,
      i => (shape.rowGeom ? shape.rowGeom.tops[i]! : i * shape.rowHeight) - layout.localScrollTop,
      layout.height,
      shape.rowGeom,
      shape.rowHeight,
      shape.columns.columns,
      binding.settings.density,
      viewport,
      shape.theme.font,
      shape.measureText,
      false,
      items,
    );
    return collectArtUrls(items);
  },
};

function pushCellArt(items: ArtUrlInput[], cell: TableCell | undefined): void {
  if (!cell) return;
  if (cell.heading) items.push(cell.heading.flag.art);
  if (cell.flags) for (const fl of cell.flags) items.push(fl.art);
  if (cell.icons) for (const ic of cell.icons) items.push(ic.art);
  items.push(cell.art, cell.unitArt);
}

function visibleCellArtItems(
  rows: readonly FlatTableRow[],
  columns: readonly Pick<FamilyColumnLayout, 'key' | 'display' | 'skillIcon' | 'inlineFlags'>[],
  first: number,
  last: number,
): ArtUrlInput[] {
  const items: ArtUrlInput[] = [];
  const start = Math.max(0, first);
  const end = Math.min(rows.length, last);
  for (let c = 0; c < columns.length; c++) {
    const col = columns[c]!;
    if (col.skillIcon) items.push(col.skillIcon);
  }
  for (let i = start; i < end; i++) {
    const item = rows[i]!;
    if (item.kind !== 'data') continue;
    for (let c = 0; c < columns.length; c++) {
      const col = columns[c]!;
      pushCellArt(items, item.row.cells[col.key]);
      if (col.inlineFlags) {
        for (let f = 0; f < col.inlineFlags.length; f++) {
          pushCellArt(items, item.row.cells[col.inlineFlags[f]!]);
        }
      }
    }
    if (item.row.detailColumn !== undefined) {
      pushCellArt(items, item.row.cells[item.row.detailColumn]);
    }
  }
  return items;
}
