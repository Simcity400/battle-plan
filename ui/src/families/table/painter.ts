import { LABEL_WEIGHT, VALUE_WEIGHT } from '../../charts/draw.ts';
import { ellipsise } from '../../charts/labels.ts';
import { measureCanvasText, textWidthCacheStats } from '../../charts/textWidth.ts';
import { formatDateRange } from '../../dashboard/calendar.ts';
import { FLAG_SLOT_HEIGHT, FLAG_SLOT_WIDTH, ICON_STRIP_GAP, MEDAL_ICON_SIZE, TRAIT_ICON_SIZE, artDrawnBounds, drawChildRowTint,
  drawClockSummary,
  drawRowFocusRing,
  drawRowHairline,
  drawRowHover,
  drawRowSelected,
  drawScrollFade,
  fitIconStripCount,
  iconStripSlotWidth,
  missingMarkWidth,
  resolveKitTokens,
  type ClockSummaryData,
} from '../../draw/kit.ts';
import { resolveCachedArt, toArtSlotState } from '../art/adapters.ts';
import type { LaneArtCache } from '../art/laneCache.ts';
import type { ArtUrlInput } from '../art/provider.ts';
import { readyArt, type ArtResult } from '../art/types.ts';
import { drawHatchRect, MISSING_BOX_SIZE } from '../valueCanvas.ts';
import { notApplicable } from '../value.ts';
import { formatScalar, formatValueState } from '../valueFormat.ts';
import { CHILD_FLAG_GAP, KIND_TILE_ART, UNIT_ART_GAP, UNIT_ART_SIZE, artGeometry, drawApprovedCell, drawDetailCell, glyphAdvance, treeLevelsOf, treeTextStart, type CellRenderContext } from './cellPainters.ts';
import { DETAIL_CHIP_GAP, DETAIL_FLAG, DETAIL_HEAD_HEIGHT, DETAIL_HEAD_LEAD, DETAIL_LINE_HEIGHT, DETAIL_NOTE_GAP, DETAIL_TEXT_SIZE, END_NOTE_HEIGHT, LIVE_END_GAP, TREE_CHEVRON_LANE, detailNote, treeColumnOf, type DetailRowLayout, type FamilyColumnLayout, type FamilyTableLayout, type RowLayoutGeometry, type TableRhythm } from './layout.ts';
import { computeColumnMaxima, resolveCellPaintModel, type FlatTableRow, type TableCell, type TableRow } from './model.ts';
import { PANE_FADE_WIDTH, PANE_INDICATOR_HEIGHT, type PaneGeometry, type PaneScroll, type ResolvedPane } from './panes.ts';
import {
  CELL_INSET,
  MEDIA_ROW_HEIGHT,
  ROW_HEIGHT,
  ROW_MARGIN_X,
  glideOffset,
  measuresColumn,
  scrollBounds,
  showsIcon,
  type GlideOffset,
  type RankMarker,
  type TableCanvasContext,
  type TableDensity,
  type TableSettings,
  type TableTheme,
} from './settings.ts';

export const WATCHING_SPEED = 24;
export const FAST_SPEED = 168;
export const WATCHING_GLIDE_MS = 180;
export const FAST_GLIDE_MS = 120;
export const TOP_RANK_HOLD_MS = 1500;
export const WATCHING_RANK_HOLD_MS = 2500;
const HAIRLINE_INSET = 10;
const DETAIL_PLATE_INSET = 5;
const DETAIL_RULE_END = 6;

export function glideDurationForSpeed(speed: number, reducedMotion = false, overrideMs?: number): number {
  if (reducedMotion) return 0;
  if (overrideMs !== undefined) return Math.max(0, overrideMs);
  if (speed > FAST_SPEED) return 0;
  return speed > WATCHING_SPEED ? FAST_GLIDE_MS : WATCHING_GLIDE_MS;
}

export function rankHoldForSpeed(speed: number): number {
  return speed > FAST_SPEED ? TOP_RANK_HOLD_MS : WATCHING_RANK_HOLD_MS;
}

export interface FamilyTablePicture {
  frame: string | null;
  rowKeys: (string | undefined)[];
  rowRefs: (TableRow | undefined)[];
  rowMarks: (string | undefined)[];
  cells: Map<string, (string | undefined)[]>;
  ranks: (string | undefined)[];
  fittedTexts: Map<string, string>;
  maximaRows?: readonly FlatTableRow[];
  maximaHour?: number;
  maximaCols?: string;
  maxima?: Map<string, number>;
  lastMaximaSig?: string;
  lastIconVersion?: number;
  lastHour?: number;
}

export function createTablePicture(): FamilyTablePicture {
  return {
    frame: null,
    rowKeys: [],
    rowRefs: [],
    rowMarks: [],
    cells: new Map(),
    ranks: [],
    fittedTexts: new Map(),
  };
}

export interface PaintStats {
  readonly drew: boolean;
  readonly paintedCells: number;
  readonly measurements: number;
}

export interface DrawFamilyTableInput {
  readonly layout: FamilyTableLayout;
  readonly rows: readonly FlatTableRow[];
  readonly settings: TableSettings;
  readonly hour: number;
  readonly width: number;
  readonly height: number;
  readonly scrollLeft: number;
  readonly scrollTop: number;
  readonly rankShown: boolean;
  readonly rankWidth: number;
  readonly theme: TableTheme;
  readonly now: number;
  readonly glideDuration: number;
  readonly glide: TableGlide;
  readonly markers?: ReadonlyMap<string, RankMarker>;
  readonly inspectedKey?: string | null;
  readonly selectedKey?: string | null;
  readonly focusedKey?: string | null;
  readonly empty?: string;
  readonly iconVersion?: number;
  readonly artCache?: LaneArtCache;
  readonly dpr?: number;
  readonly hasGroupHeaders?: boolean;
  readonly dateGrouped?: boolean;
  readonly dayDividersOnly?: boolean;
  readonly rhythm?: TableRhythm;
  readonly hideHeader?: boolean;
  readonly endNote?: string;
  readonly precomputedMaxima?: Map<string, number>;
  readonly staticRowLooks?: boolean;
  readonly rowHeightMode?: 'standard' | 'media';
  readonly rowGeom?: RowLayoutGeometry | null;
  readonly clockSummary?: ClockSummaryData;
}

const fontOf = (theme: TableTheme, size: number, weight: number) => `${weight} ${size}px ${theme.font}`;

function colorOf(theme: TableTheme, tone: string | undefined, countryColor: string | null, fraction: number | null): string {
  if (!tone) return theme.text;
  if (tone === 'country') {
    return countryColor ?? theme.text;
  }
  if (tone === 'value-good' || tone === 'value-bad') {
    if (fraction === null) return theme.muted;
    const badness = tone === 'value-bad' ? fraction : 1 - fraction;
    return badness < 1 / 3 ? theme.green : badness < 2 / 3 ? theme.amber : theme.red;
  }
  if (tone in theme) return (theme as unknown as Record<string, string>)[tone]!;
  return theme.text;
}

function rowMarkState(
  item: Extract<FlatTableRow, { kind: 'data' }>,
  inspectedKey: string | null | undefined,
  selectedKey: string | null | undefined,
  focusedKey: string | null | undefined,
  lastChild: boolean,
): string {
  const i = inspectedKey === item.key ? 1 : 0;
  const s = selectedKey === item.key ? 2 : 0;
  const f = focusedKey === item.key ? 4 : 0;
  const d = item.isDayEnd ? 8 : 0;
  const l = item.isLiveEnd ? 16 : 0;
  const c = lastChild ? 32 : 0;
  const sd = item.showDate === false ? 64 : 0;
  return String(i | s | f | d | l | c | sd);
}

function nonDataMark(item: Exclude<FlatTableRow, { kind: 'data' }>): string {
  if (item.kind === 'group-header') return `g:${item.label}`;
  if (item.kind === 'gap') return `gap:${item.firstHour}:${item.lastHour}:${item.reason}`;
  if (item.kind === 'refused') return `ref:${item.reason}`;
  return `load:${item.key}`;
}

export function barPixelSpan(column: FamilyColumnLayout, dpr: number): number {
  return Math.max(1, Math.max(1, column.width - CELL_INSET * 2) * dpr);
}

export function barPixels(fraction: number | null, span: number): number {
  return fraction === null ? -1 : Math.round(fraction * span);
}

function paintRowBackground(
  ctx: TableCanvasContext,
  theme: TableTheme,
  rowWidth: number,
  y: number,
  rowH: number,
  contentH: number,
  isChild: boolean,
  inspected: boolean,
  selected: boolean,
  focused: boolean,
  hairlineKind: 'normal' | 'strong' | 'band',
  hairlineLeft: number,
  fullPlate: boolean,
): void {
  ctx.fillStyle = theme.surface;
  ctx.fillRect(0, y, rowWidth, rowH);
  if (isChild) drawChildRowTint(ctx, ROW_MARGIN_X, y, rowWidth - ROW_MARGIN_X * 2, contentH, theme);
  const plateX = ROW_MARGIN_X;
  const plateW = Math.max(1, rowWidth - ROW_MARGIN_X * 2);
  const plateY = fullPlate ? y - 1 : y + 1;
  const plateH = Math.max(1, fullPlate ? contentH + 2 : contentH - 2);
  const inset = fullPlate ? 0 : undefined;
  if (selected) {
    drawRowSelected(ctx, plateX, plateY, plateW, plateH, theme, inset);
  } else if (inspected) {
    drawRowHover(ctx, plateX, plateY, plateW, plateH, theme, inset);
  }
  if (focused) {
    drawRowFocusRing(ctx, plateX, plateY, plateW, plateH, theme, inset);
  }
  if (hairlineKind === 'band') {
    const bandY = y + contentH + Math.floor((rowH - contentH) / 2);
    drawRowHairline(ctx, HAIRLINE_INSET, rowWidth - HAIRLINE_INSET, bandY, theme, 'band');
  } else if (!selected && !inspected) {
    drawRowHairline(ctx, hairlineLeft, rowWidth - HAIRLINE_INSET, y + contentH - 1, theme, hairlineKind);
  }
}

function paintCellSliceGround(
  ctx: TableCanvasContext,
  theme: TableTheme,
  x: number,
  y: number,
  cellWidth: number,
  rowH: number,
  contentH: number,
  rowWidth: number,
  isChild: boolean,
  hairlineKind: 'normal' | 'strong' | 'band',
  hairlineLeft: number,
): void {
  ctx.fillStyle = theme.surface;
  ctx.fillRect(x, y, cellWidth, rowH);
  if (isChild) {
    const tintLeft = Math.max(x, ROW_MARGIN_X);
    drawChildRowTint(ctx, tintLeft, y, Math.min(x + cellWidth, rowWidth - ROW_MARGIN_X) - tintLeft, contentH, theme);
  }
  const lineY = hairlineKind === 'band' ? y + contentH + Math.floor((rowH - contentH) / 2) : y + contentH - 1;
  drawRowHairline(ctx, Math.max(x, hairlineLeft), Math.min(x + cellWidth, rowWidth - HAIRLINE_INSET), lineY, theme, hairlineKind);
}

function isLastChildRow(rows: readonly FlatTableRow[], index: number): boolean {
  const item = rows[index];
  if (!item || item.kind !== 'data' || item.level <= 1) return false;
  const next = rows[index + 1];
  return !next || next.kind !== 'data' || next.level === 1;
}

function gapLength(firstHour: number, lastHour: number): string {
  const hours = Math.max(1, lastHour - firstHour + 1);
  const days = Math.floor(hours / 24);
  const rest = hours % 24;
  const dayText = days ? `${formatScalar(days, 'integer')} ${days === 1 ? 'day' : 'days'}` : '';
  const hourText = rest ? `${rest} ${rest === 1 ? 'hour' : 'hours'}` : '';
  return [dayText, hourText].filter(Boolean).join(' ');
}

export function drawFamilyTable(
  ctx: TableCanvasContext,
  input: DrawFamilyTableInput,
  picture?: FamilyTablePicture,
): PaintStats {
  const {
    layout,
    rows,
    settings,
    hour,
    width,
    height,
    scrollLeft,
    scrollTop,
    rankShown,
    rankWidth,
    theme,
    now,
    glideDuration,
    glide,
    markers,
    inspectedKey,
    selectedKey,
    focusedKey,
    empty,
    iconVersion,
    artCache,
    dpr = 1,
    precomputedMaxima,
    staticRowLooks,
    dateGrouped = false,
    dayDividersOnly = false,
    rhythm,
    hideHeader = false,
    endNote,
    rowHeightMode = 'standard',
    rowGeom = null,
    clockSummary,
  } = input;

  if (clockSummary) {
    if (picture) picture.frame = null;
    ctx.clearRect(0, 0, width, height);
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, 0, width, height);
    drawClockSummary(ctx, width, height, clockSummary, theme);
    return { drew: true, paintedCells: 1, measurements: 0 };
  }

  const uniformRowHeight = rowHeightMode === 'media' ? MEDIA_ROW_HEIGHT[settings.density] : ROW_HEIGHT[settings.density];
  const variableHeights = rowGeom !== null;
  const bounds = scrollBounds(layout, width, scrollLeft);
  const rank = rankShown ? rankWidth : 0;
  const rowWidth = Math.max(width, layout.totalWidth);
  const moving = glideDuration > 0 && glide.offsets.size > 0;

  let first = 0;
  let last = rows.length;
  if (!variableHeights) {
    first = Math.max(0, Math.floor(scrollTop / uniformRowHeight));
    last = Math.min(rows.length, Math.ceil((scrollTop + height) / uniformRowHeight) + (moving ? 2 : 0));
  } else {
    let lo = 0;
    let hi = rows.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (rowGeom.tops[mid]! + rowGeom.heights[mid]! <= scrollTop) lo = mid + 1;
      else hi = mid;
    }
    first = lo;
    let end = first;
    const bottomEdge = scrollTop + height;
    while (end < rows.length && rowGeom.tops[end]! < bottomEdge) end++;
    last = Math.min(rows.length, end + (moving ? 2 : 0));
  }

  let measurements = 0;
  const fitCache = picture?.fittedTexts ?? new Map<string, string>();

  const measure = (text: string, font: string): number => {
    const before = textWidthCacheStats().misses;
    const w = measureCanvasText(ctx, text, font);
    measurements += textWidthCacheStats().misses - before;
    return w;
  };

  const fit = (text: string, font: string, maxWidth: number): string => {
    if (maxWidth <= 0) return '';
    if (measure(text, font) <= maxWidth) return text;
    const key = `${font}\u0000${Math.floor(maxWidth)}\u0000${text}`;
    let cached = fitCache.get(key);
    if (cached === undefined) {
      cached = ellipsise(text, maxWidth, sample => measure(sample, font));
      if (fitCache.size > 4096) fitCache.clear();
      fitCache.set(key, cached);
    }
    return cached;
  };

  let maximaCols = '';
  let hasMeasuredCols = false;
  for (let c = 0; c < layout.columns.length; c++) {
    const col = layout.columns[c]!;
    if (col.numeric && measuresColumn(col)) {
      hasMeasuredCols = true;
      maximaCols += `${col.key}:${col.display}|`;
    }
  }
  let maxima: Map<string, number>;
  if (precomputedMaxima) {
    maxima = precomputedMaxima;
    if (picture) {
      picture.maximaRows = rows;
      picture.maximaHour = hour;
      picture.maximaCols = maximaCols;
      picture.maxima = maxima;
    }
  } else if (!hasMeasuredCols) {
    maxima = new Map();
    if (picture) {
      picture.maximaRows = rows;
      picture.maximaHour = hour;
      picture.maximaCols = maximaCols;
      picture.maxima = maxima;
    }
  } else if (picture && picture.maxima && picture.maximaRows === rows && picture.maximaHour === hour && picture.maximaCols === maximaCols) {
    maxima = picture.maxima;
  } else {
    maxima = computeColumnMaxima(rows, layout.columns);
    if (picture) {
      picture.maximaRows = rows;
      picture.maximaHour = hour;
      picture.maximaCols = maximaCols;
      picture.maxima = maxima;
    }
  }
  let maximaSig = '';
  if (maxima.size > 0) {
    for (const [k, v] of maxima) maximaSig += `${k}=${v};`;
  }

  let frameKey: string | null = null;
  if (picture && !moving && !variableHeights) {
    let colKey = '';
    for (let c = 0; c < layout.columns.length; c++) {
      const col = layout.columns[c]!;
      colKey += `${col.key}:${col.width}:${col.narrowed ? 1 : 0}:${col.display}:${col.tone ?? ''}:${col.barEligible ? 1 : 0}|`;
    }
    frameKey = `${width}|${height}|${uniformRowHeight}|${rowHeightMode}|${rhythm ?? ''}|${hideHeader ? 1 : 0}|${dateGrouped ? 1 : 0}|${settings.barsBehind ?? 'all'}|${settings.sort.column}|${rank}|${scrollTop}|${scrollLeft}|${layout.scrollable ? 1 : 0}|${layout.pinnedCount}|${layout.pinnedWidth}|${first}|${last}|${rows.length ? '' : empty ?? ''}|${endNote ?? ''}|${colKey}`;
  }

  const logRows = rhythm === 'log';
  const afterDate = layout.columns[0]?.cellKind === 'date' && (dateGrouped || logRows);
  const intraDayLeftBase = afterDate
    ? rank + layout.columns[0]!.width + (logRows ? CELL_INSET + (layout.columns[1]?.leadInset ?? 0) : 0)
    : HAIRLINE_INSET;
  const hairlineSpec = (item: Extract<FlatTableRow, { kind: 'data' }>, index: number, pinned: boolean) => {
    if (item.isLiveEnd) return { kind: 'band' as const, left: HAIRLINE_INSET };
    if (item.isDayEnd || item.isTotal || (!item.row.detailColumn && isLastChildRow(rows, index))) return { kind: 'strong' as const, left: HAIRLINE_INSET };
    if (dayDividersOnly) return { kind: 'normal' as const, left: rowWidth };
    if (afterDate && intraDayLeftBase > HAIRLINE_INSET) {
      const shifted = pinned ? intraDayLeftBase : Math.max(HAIRLINE_INSET, intraDayLeftBase - bounds.left);
      return { kind: 'normal' as const, left: shifted };
    }
    return { kind: 'normal' as const, left: HAIRLINE_INSET };
  };

  const treeLevels = treeLevelsOf(rows);
  const renderCtx: CellRenderContext = {
    ctx,
    theme,
    density: settings.density,
    barsMode: settings.barsBehind ?? 'all',
    sortedColumn: settings.sort.column,
    rhythm,
    treeColumn: treeColumnOf(layout.columns),
    treeLevels,
    hasActionColumn: layout.columns.some(col => col.cellKind === 'action'),
    hideHeader,
    artCache,
    measure,
    fit,
  };

  const drawRankContent = (item: Extract<FlatTableRow, { kind: 'data' }>, x: number, y: number, rowH: number) => {
    const middle = y + rowH / 2;
    const smallFont = fontOf(theme, theme.fontSize.small, LABEL_WEIGHT);
    ctx.font = smallFont;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    const label = String(item.rank);
    const labelW = measure(label, smallFont);
    ctx.fillStyle = theme.muted;
    ctx.fillText(label, x + CELL_INSET, middle);
    const marker = markers?.get(item.key);
    if (marker && marker.until > now) {
      ctx.fillStyle = marker.delta > 0 ? theme.green : theme.red;
      const markerText = `${marker.delta > 0 ? '\u25b2' : '\u25bc'}${Math.abs(marker.delta)}`;
      ctx.fillText(fit(markerText, smallFont, Math.max(0, rank - CELL_INSET - labelW - 3)), x + CELL_INSET + labelW + 3, middle);
    }
  };

  const rankSignature = (item: FlatTableRow): string => {
    if (item.kind !== 'data') return '';
    const marker = markers?.get(item.key);
    return marker && marker.until > now ? `${item.key}\u0002${item.rank}\u0002${marker.delta}` : `${item.key}\u0002${item.rank}`;
  };

  const artStateOf = (art: ArtResult | null | undefined): string => {
    if (!art) return '';
    const resolved = resolveCachedArt(art, artCache);
    return resolved.status === 'ready' ? `${resolved.url}\u0003${resolved.bitmap ? 'r' : 'p'}` : `${resolved.status}\u0003${'reason' in resolved ? resolved.reason : ''}`;
  };

  const cellListSignature = (cell: TableCell): string => {
    let sig = '';
    if (cell.flags) {
      for (const flag of cell.flags) sig += `f${flag.tag}\u0005${artStateOf(flag.art)}\u0005${flag.dimmed ? 1 : 0}`;
    }
    if (cell.icons) {
      for (const entry of cell.icons) sig += `i${artStateOf(entry.art)}\u0005${entry.name}\u0005${entry.count ?? ''}`;
    }
    return sig;
  };

  const computeLook = (item: Extract<FlatTableRow, { kind: 'data' }>, column: FamilyColumnLayout, remember = true) => {
    const cell = item.row.cells[column.key];
    const paintModel = resolveCellPaintModel(cell, column, maxima);
    const formatted = formatValueState(paintModel.value, paintModel.format, cell?.missingFigure ? { figure: cell.missingFigure } : undefined);
    const span = barPixelSpan(column, dpr);
    const pixels = barPixels(paintModel.bar.fraction, span);
    const fraction = paintModel.bar.fraction === null ? null : pixels / span;
    const fill = paintModel.bar.alert ? theme.red : colorOf(theme, paintModel.tone ?? undefined, paintModel.countryColor, fraction);
    if (!remember) return { paintModel, formatted, fraction, fill, key: '' };
    const art = artStateOf(paintModel.art);
    const treeKey = `${item.isTreeGroup ? (item.isOpen ? 'o' : 'c') : ''}${treeTextStart(item.level, treeLevels)}`;
    const textVal = formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable' || formatted.kind === 'pending' ? formatted.label : formatted.text;
    const secVal = cell?.secondaryValue ? (cell.secondaryValue.kind === 'known' || cell.secondaryValue.kind === 'zero' ? String(cell.secondaryValue.value) : cell.secondaryValue.kind) : '';
    const pair = cell?.pair?.map(value => formatValueState(value, paintModel.format).spoken).join('|') ?? '';
    const unitArt = artStateOf(cell?.unitArt);
    const splitSig = cell?.splitBar ? `[${cell.splitBar.map(s => `${s.fraction}:${s.fill}:${s.hatched ? 1 : 0}`).join(',')}]` : '';
    const flags = column.inlineFlags?.map(key => {
      const tail = item.row.cells[key];
      return tail && !tail.blank ? `${tail.glyph ?? ''}:${tail.value.kind}:${cellListSignature(tail)}` : '';
    }).join('|') ?? '';
    const extraSig = `${flags}|${cell?.glyph ?? ''}|${cell?.artTile ?? ''}|${cell?.blank ? 'blank' : ''}${cell?.subtext ?? ''}\u0004${secVal}\u0004${cell?.shortage ? 1 : 0}\u0004${cell?.stockpile ? JSON.stringify(cell.stockpile) : ''}\u0004${cell ? cellListSignature(cell) : ''}\u0004${cell?.statusKind ?? ''}\u0004${cell?.actionLabel ?? ''}|${cell?.actionIcon ?? ''}|${pair}\u0004${cell?.inlinePrefix ?? ''}\u0004${cell?.inlineSubtext ?? ''}\u0004${cell?.narrowText ?? ''}\u0004${cell?.strong ? 1 : 0}\u0004${cell?.unitSuffix ?? ''}\u0004${unitArt}\u0004${cell?.missingFigure ?? ''}\u0004${splitSig}\u0004${item.showDate === false ? 0 : 1}`;
    const key = `${formatted.kind}\u0002${textVal}\u0002${pixels}\u0002${fill}\u0002${paintModel.countryColor ?? ''}\u0002${art}\u0002${treeKey}\u0002${item.row.muted ? 1 : 0}\u0002${paintModel.status ?? ''}\u0002${extraSig}`;
    return { paintModel, formatted, fraction, fill, key };
  };

  const drawGroupHeaderRow = (item: Extract<FlatTableRow, { kind: 'group-header' }>, y: number, rowH: number) => {
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, y, rowWidth, rowH);
    drawRowHairline(ctx, HAIRLINE_INSET, rowWidth - HAIRLINE_INSET, y + rowH - 1, theme, 'normal');
    const smallFont = fontOf(theme, 12, VALUE_WEIGHT);
    ctx.font = smallFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(fit(item.label, smallFont, Math.max(1, width - 20)), CELL_INSET, y + rowH / 2);
  };

  const drawGapRow = (item: Extract<FlatTableRow, { kind: 'gap' }>, y: number, rowH: number) => {
    const tokens = resolveKitTokens(theme);
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, y, rowWidth, rowH);
    const middle = y + rowH / 2;
    const left = ROW_MARGIN_X + CELL_INSET;
    const right = Math.max(left, width - ROW_MARGIN_X - CELL_INSET);
    const strongFont = fontOf(theme, 11.5, VALUE_WEIGHT);
    const smallFont = fontOf(theme, 11.5, 400);
    const length = gapLength(item.firstHour, item.lastHour);
    const lengthW = measure(length, smallFont);
    const lead = 'Not recorded';
    const dates = ` · ${formatDateRange(item.firstHour, item.lastHour)}`;
    const leadW = measure(lead, strongFont);
    ctx.save();
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.font = strongFont;
    ctx.fillStyle = tokens.textSecondary;
    ctx.fillText(fit(lead, strongFont, right - left), left, middle);
    const datesShown = fit(dates, smallFont, Math.max(0, right - left - leadW - lengthW - 60));
    const datesW = measure(datesShown, smallFont);
    ctx.font = smallFont;
    ctx.fillStyle = theme.muted;
    ctx.fillText(datesShown, left + leadW, middle);
    const bandLeft = left + leadW + datesW + 10;
    const bandRight = right - lengthW - 10;
    if (bandRight - bandLeft >= 24) {
      drawHatchRect(ctx, bandLeft, Math.round(middle - 2.5), bandRight - bandLeft, 5, 2.5, theme);
      ctx.textAlign = 'right';
      ctx.fillText(length, right, middle);
    }
    ctx.restore();
  };

  const drawRefusedRow = (item: Extract<FlatTableRow, { kind: 'refused' }>, y: number, rowH: number) => {
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, y, rowWidth, rowH);
    const middle = y + rowH / 2;
    const boxX = HAIRLINE_INSET + 4;
    drawHatchRect(ctx, boxX, Math.round(middle - MISSING_BOX_SIZE / 2), MISSING_BOX_SIZE, MISSING_BOX_SIZE, 2, theme);
    const smallFont = fontOf(theme, 12, LABEL_WEIGHT);
    ctx.save();
    ctx.font = smallFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    const textX = boxX + MISSING_BOX_SIZE + 8;
    ctx.fillText(fit(item.reason, smallFont, Math.max(1, width - textX - HAIRLINE_INSET)), textX, middle);
    ctx.restore();
    drawRowHairline(ctx, HAIRLINE_INSET, rowWidth - HAIRLINE_INSET, y + rowH - 1, theme, 'strong');
  };

  const drawLoadingRow = (y: number, rowH: number) => {
    const tokens = resolveKitTokens(theme);
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, y, rowWidth, rowH);
    const barW = Math.min(180, Math.max(48, width - HAIRLINE_INSET * 2 - 40));
    const barH = 10;
    ctx.save();
    ctx.fillStyle = tokens.headerBand;
    ctx.beginPath();
    ctx.roundRect(HAIRLINE_INSET + 4, Math.round(y + (rowH - barH) / 2), barW, barH, 3);
    ctx.fill();
    ctx.restore();
    drawRowHairline(ctx, HAIRLINE_INSET, rowWidth - HAIRLINE_INSET, y + rowH - 1, theme, 'normal');
  };

  const drawNonDataRow = (item: Exclude<FlatTableRow, { kind: 'data' }>, y: number, rowH: number) => {
    if (item.kind === 'group-header') drawGroupHeaderRow(item, y, rowH);
    else if (item.kind === 'gap') drawGapRow(item, y, rowH);
    else if (item.kind === 'refused') drawRefusedRow(item, y, rowH);
    else drawLoadingRow(y, rowH);
  };

  const paintPinnedOverlayForRow = (
    item: Extract<FlatTableRow, { kind: 'data' }>,
    index: number,
    y: number,
    rowH: number,
  ) => {
    const inspected = inspectedKey === item.key;
    const selected = selectedKey === item.key;
    const focused = focusedKey === item.key;
    const isChild = item.level > 1;
    const contentH = item.isLiveEnd ? rowH - LIVE_END_GAP : rowH;
    const hl = hairlineSpec(item, index, true);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, y, layout.pinnedWidth, rowH);
    ctx.clip();
    paintRowBackground(ctx, theme, rowWidth, y, rowH, contentH, isChild, inspected, selected, focused, hl.kind, hl.left, logRows);
    if (rank > 0) drawRankContent(item, 0, y, contentH);
    let px = rank;
    const limit = Math.min(layout.pinnedCount, layout.columns.length);
    for (let c = 0; c < limit; c++) {
      const col = layout.columns[c]!;
      const look = computeLook(item, col, false);
      drawApprovedCell(renderCtx, {
        item,
        column: col,
        colIndex: c,
        x: px,
        y,
        rowHeight: contentH,
        paintModel: look.paintModel,
        formatted: look.formatted,
        fraction: look.fraction,
        fill: look.fill,
        rowHovered: inspected,
        rowFocused: focused,
        lastChild: isLastChildRow(rows, index),
      });
      px += col.width;
    }
    ctx.restore();
  };

  if (picture && frameKey !== null && picture.frame === frameKey) {
    let drew = false;
    let paintedCells = 0;
    const dirtyRows = new Set<number>();
    const fullRepaintRows = new Set<number>();
    const maximaSame = picture.lastMaximaSig === maximaSig;
    const iconsSame = iconVersion !== undefined && picture.lastIconVersion === iconVersion;
    const hourSafe = staticRowLooks === true || picture.lastHour === hour;
    const canFastSkip = maximaSame && iconsSame && hourSafe;

    const skipRow = new Uint8Array(Math.max(0, last - first));
    if (canFastSkip) {
      for (let index = first; index < last; index++) {
        const item = rows[index]!;
        if (item.kind !== 'data') {
          if (picture.rowKeys[index] === item.key && picture.rowMarks[index] === nonDataMark(item)) {
            skipRow[index - first] = 1;
          }
          continue;
        }
        if (picture.rowRefs[index] !== item.row || picture.rowKeys[index] !== item.key) continue;
        const mark = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
        if (picture.rowMarks[index] !== mark) continue;
        if (rank > 0) {
          const rSig = `${rankSignature(item)}\u0001${mark}`;
          if (picture.ranks[index] !== rSig) continue;
        }
        skipRow[index - first] = 1;
      }
    }

    for (let index = first; index < last; index++) {
      if (skipRow[index - first]) continue;
      const item = rows[index]!;
      if (item.kind !== 'data') {
        const nMark = nonDataMark(item);
        if (picture.rowKeys[index] === item.key && picture.rowMarks[index] === nMark) continue;
        drawNonDataRow(item, index * uniformRowHeight - scrollTop, uniformRowHeight);
        picture.rowKeys[index] = item.key;
        picture.rowRefs[index] = undefined;
        picture.rowMarks[index] = nMark;
        picture.ranks[index] = undefined;
        dirtyRows.add(index);
        drew = true;
        continue;
      }
      const mark = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
      if (inspectedKey === item.key || selectedKey === item.key || focusedKey === item.key || picture.rowMarks[index] !== mark || picture.rowKeys[index] !== item.key) {
        fullRepaintRows.add(index);
      }
    }

    for (const index of fullRepaintRows) {
      const item = rows[index]!;
      if (item.kind !== 'data') continue;
      const y = index * uniformRowHeight - scrollTop;
      const inspected = inspectedKey === item.key;
      const selected = selectedKey === item.key;
      const focused = focusedKey === item.key;
      const mark = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
      const hl = hairlineSpec(item, index, false);
      paintRowBackground(ctx, theme, rowWidth, y, uniformRowHeight, uniformRowHeight, item.level > 1, inspected, selected, focused, hl.kind, hl.left, logRows);
      let x = rank - bounds.left;
      for (let colIndex = 0; colIndex < layout.columns.length; colIndex++) {
        const column = layout.columns[colIndex]!;
        let drawn = picture.cells.get(column.key);
        if (!drawn) picture.cells.set(column.key, drawn = []);
        const look = computeLook(item, column);
        drawApprovedCell(renderCtx, {
          item,
          column,
          colIndex,
          x,
          y,
          rowHeight: uniformRowHeight,
          paintModel: look.paintModel,
          formatted: look.formatted,
          fraction: look.fraction,
          fill: look.fill,
          rowHovered: inspected,
          rowFocused: focused,
          lastChild: isLastChildRow(rows, index),
        });
        drawn[index] = `${look.key}\u0001${mark}`;
        paintedCells++;
        x += column.width;
      }
      if (rank > 0) {
        drawRankContent(item, layout.scrollable ? 0 : -bounds.left, y, uniformRowHeight);
        picture.ranks[index] = `${rankSignature(item)}\u0001${mark}`;
      }
      dirtyRows.add(index);
      drew = true;
    }

    let x = rank - bounds.left;
    for (let colIndex = 0; colIndex < layout.columns.length; colIndex++) {
      const column = layout.columns[colIndex]!;
      let drawn = picture.cells.get(column.key);
      if (!drawn) picture.cells.set(column.key, drawn = []);
      for (let index = first; index < last; index++) {
        if (skipRow[index - first] || fullRepaintRows.has(index)) continue;
        const item = rows[index]!;
        if (item.kind !== 'data') continue;
        const mark = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
        const look = computeLook(item, column);
        const cellSig = `${look.key}\u0001${mark}`;
        if (drawn[index] === cellSig) continue;
        const y = index * uniformRowHeight - scrollTop;
        const hl = hairlineSpec(item, index, false);
        paintCellSliceGround(ctx, theme, x, y, column.width, uniformRowHeight, uniformRowHeight, rowWidth, item.level > 1, hl.kind, hl.left);
        drawApprovedCell(renderCtx, {
          item,
          column,
          colIndex,
          x,
          y,
          rowHeight: uniformRowHeight,
          paintModel: look.paintModel,
          formatted: look.formatted,
          fraction: look.fraction,
          fill: look.fill,
          rowHovered: false,
          rowFocused: false,
          lastChild: isLastChildRow(rows, index),
        });
        drawn[index] = cellSig;
        dirtyRows.add(index);
        paintedCells++;
        drew = true;
      }
      x += column.width;
    }

    if (rank > 0) {
      for (let index = first; index < last; index++) {
        if (skipRow[index - first] || fullRepaintRows.has(index)) continue;
        const item = rows[index]!;
        if (item.kind !== 'data') continue;
        const mark = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
        const rSig = `${rankSignature(item)}\u0001${mark}`;
        if (picture.ranks[index] === rSig && !dirtyRows.has(index)) continue;
        const rx = layout.scrollable ? 0 : -bounds.left;
        const y = index * uniformRowHeight - scrollTop;
        const hl = hairlineSpec(item, index, false);
        paintCellSliceGround(ctx, theme, rx, y, rank, uniformRowHeight, uniformRowHeight, rowWidth, item.level > 1, hl.kind, hl.left);
        drawRankContent(item, rx, y, uniformRowHeight);
        picture.ranks[index] = rSig;
        dirtyRows.add(index);
        drew = true;
      }
    }

    if (dirtyRows.size > 0 && layout.scrollable && layout.pinnedWidth > 0) {
      for (const index of dirtyRows) {
        const item = rows[index]!;
        if (item.kind !== 'data') continue;
        paintPinnedOverlayForRow(item, index, index * uniformRowHeight - scrollTop, uniformRowHeight);
      }
    }

    for (let index = first; index < last; index++) {
      if (skipRow[index - first]) continue;
      const item = rows[index]!;
      picture.rowKeys[index] = item.key;
      if (item.kind === 'data') {
        picture.rowRefs[index] = item.row;
        picture.rowMarks[index] = rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index));
      } else {
        picture.rowRefs[index] = undefined;
        picture.rowMarks[index] = nonDataMark(item);
      }
    }
    picture.lastMaximaSig = maximaSig;
    picture.lastIconVersion = iconVersion;
    picture.lastHour = hour;
    return { drew, paintedCells, measurements };
  }

  if (picture) {
    picture.frame = frameKey;
    picture.rowKeys = [];
    picture.rowRefs = [];
    picture.rowMarks = [];
    picture.cells = new Map();
    picture.ranks = [];
    picture.lastMaximaSig = maximaSig;
    picture.lastIconVersion = iconVersion;
    picture.lastHour = hour;
  }

  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = theme.surface;
  ctx.fillRect(0, 0, width, height);

  if (!rows.length && empty) {
    const bodyFont = fontOf(theme, theme.fontSize.body, LABEL_WEIGHT);
    ctx.font = bodyFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(fit(empty, bodyFont, Math.max(1, width - 32)), width / 2, height / 2);
    return { drew: true, paintedCells: 0, measurements };
  }

  let paintedCells = 0;
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, 0, width, height);
  ctx.clip();

  const topAt = (index: number) => (variableHeights ? rowGeom.tops[index]! : index * uniformRowHeight);
  const heightAt = (index: number) => (variableHeights ? rowGeom.heights[index]! : uniformRowHeight);
  const rowY = (item: FlatTableRow, index: number) =>
    topAt(index) - scrollTop + (moving && item.kind === 'data' ? glideOffset(glide.offsets.get(item.key), now, glideDuration) : 0);

  const visibleIndices = moving
    ? visibleGlidingIndices(rows, glide, first, last, scrollTop, height, now, glideDuration, variableHeights ? rowGeom : null, uniformRowHeight)
    : Array.from({ length: Math.max(0, last - first) }, (_, i) => first + i);

  const paintDetailRow = (
    item: Extract<FlatTableRow, { kind: 'data' }>,
    detail: DetailRowLayout,
    y: number,
    rowH: number,
    inspected: boolean,
    selected: boolean,
    focused: boolean,
    hl: ReturnType<typeof hairlineSpec>,
  ) => {
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, y, rowWidth, rowH);
    const plateX = detail.indent + DETAIL_PLATE_INSET;
    const plateW = Math.max(1, rowWidth - ROW_MARGIN_X - plateX);
    const lead = detail.heading ? DETAIL_HEAD_LEAD : 0;
    const plateY = y + lead;
    const plateH = Math.max(1, rowH - lead - detail.bottom);
    if (selected) drawRowSelected(ctx, plateX, plateY, plateW, plateH, theme, 0);
    else if (inspected) drawRowHover(ctx, plateX, plateY, plateW, plateH, theme, 0);
    if (focused) drawRowFocusRing(ctx, plateX, plateY, plateW, plateH, theme, 0);
    ctx.fillStyle = theme.line;
    ctx.fillRect(detail.indent, y, 1, rowH - (detail.last ? DETAIL_RULE_END : 0));
    if (detail.last && hl.kind !== 'band' && hl.left < rowWidth) drawRowHairline(ctx, hl.left, rowWidth - HAIRLINE_INSET, y + rowH - 1, theme, hl.kind);
    const cell = item.row.cells[item.row.detailColumn!];
    drawDetailCell(renderCtx, item, cell, formatValueState(cell?.value ?? notApplicable('No detail for this row')), detail, y);
  };

  const paintPass = (pinnedOnly: boolean) => {
    const retained = !pinnedOnly && frameKey !== null ? picture : undefined;
    const startCol = 0;
    const endCol = pinnedOnly ? Math.min(layout.pinnedCount, layout.columns.length) : layout.columns.length;
    for (let vi = 0; vi < visibleIndices.length; vi++) {
      const index = visibleIndices[vi]!;
      const item = rows[index]!;
      const y = rowY(item, index);
      const rowH = heightAt(index);
      if (item.kind !== 'data') {
        if (!pinnedOnly) {
          drawNonDataRow(item, y, rowH);
          if (retained) {
            retained.rowKeys[index] = item.key;
            retained.rowRefs[index] = undefined;
            retained.rowMarks[index] = nonDataMark(item);
          }
        }
        continue;
      }
      const inspected = inspectedKey === item.key;
      const selected = selectedKey === item.key;
      const focused = focusedKey === item.key;
      const mark = retained ? rowMarkState(item, inspectedKey, selectedKey, focusedKey, isLastChildRow(rows, index)) : '';
      const contentH = item.isLiveEnd ? rowH - LIVE_END_GAP : rowH;
      const hl = hairlineSpec(item, index, pinnedOnly);
      const detailRow = rowGeom?.detailRows?.get(index);
      if (detailRow) {
        paintDetailRow(item, detailRow, y, rowH, inspected, selected, focused, hl);
        paintedCells++;
        continue;
      }
      paintRowBackground(ctx, theme, rowWidth, y, rowH, contentH, item.level > 1, inspected, selected, focused, hl.kind, hl.left, logRows);
      if (rank > 0) {
        drawRankContent(item, pinnedOnly ? 0 : -bounds.left, y, contentH);
        if (retained) retained.ranks[index] = `${rankSignature(item)}\u0001${mark}`;
      }
      let x = pinnedOnly ? rank : rank - bounds.left;
      for (let colIndex = startCol; colIndex < endCol; colIndex++) {
        const column = layout.columns[colIndex]!;
        const look = computeLook(item, column, retained !== undefined);
        drawApprovedCell(renderCtx, {
          item,
          column,
          colIndex,
          x,
          y,
          rowHeight: contentH,
          paintModel: look.paintModel,
          formatted: look.formatted,
          fraction: look.fraction,
          fill: look.fill,
          rowHovered: inspected,
          rowFocused: focused,
          lastChild: isLastChildRow(rows, index),
        });
        if (!pinnedOnly) {
          paintedCells++;
          if (retained) {
            let drawn = retained.cells.get(column.key);
            if (!drawn) retained.cells.set(column.key, drawn = []);
            drawn[index] = `${look.key}\u0001${mark}`;
          }
        }
        x += column.width;
      }
      if (retained) {
        retained.rowKeys[index] = item.key;
        retained.rowRefs[index] = item.row;
        retained.rowMarks[index] = mark;
      }
    }
  };

  paintPass(false);

  if (endNote && rows.length) {
    const noteTop = topAt(rows.length - 1) + heightAt(rows.length - 1) - scrollTop;
    if (noteTop < height) {
      const noteFont = fontOf(theme, 12, 400);
      ctx.font = noteFont;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(fit(endNote, noteFont, Math.max(1, width - 32)), width / 2, noteTop + 6 + (END_NOTE_HEIGHT - 6) / 2);
    }
  }

  if (layout.scrollable && layout.pinnedWidth > 0) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, layout.pinnedWidth, height);
    ctx.clip();
    paintPass(true);
    ctx.restore();
  }

  ctx.restore();
  return { drew: true, paintedCells, measurements };
}

export interface TableGlide {
  readonly offsets: ReadonlyMap<string, GlideOffset>;
  readonly movers: readonly { readonly index: number; readonly lo: number; readonly hi: number }[];
  readonly starts: Float64Array;
  readonly ends: Float64Array;
}

export const NO_GLIDE: TableGlide = { offsets: new Map(), movers: [], starts: new Float64Array(0), ends: new Float64Array(0) };

export function tableGlide(offsets: ReadonlyMap<string, GlideOffset>, rows: readonly FlatTableRow[], rowIndex: ReadonlyMap<string, number>, rowGeom: RowLayoutGeometry | null, rowHeight: number): TableGlide {
  if (offsets.size === 0) return NO_GLIDE;
  const movers: { index: number; lo: number; hi: number }[] = [];
  for (const [key, offset] of offsets) {
    const index = rowIndex.get(key);
    if (index === undefined || rows[index]?.kind !== 'data') continue;
    const top = rowGeom ? rowGeom.tops[index]! : index * rowHeight;
    const lo = top + Math.min(offset.from, offset.to, 0);
    const hi = top + Math.max(offset.from, offset.to, 0) + (rowGeom ? rowGeom.heights[index]! : rowHeight);
    movers.push({ index, lo, hi });
  }
  movers.sort((x, y) => x.lo - y.lo);
  let size = 1;
  while (size < movers.length) size *= 2;
  const starts = new Float64Array(movers.length);
  const ends = new Float64Array(2 * size).fill(-Infinity);
  for (let at = 0; at < movers.length; at++) {
    starts[at] = movers[at]!.lo;
    ends[size + at] = movers[at]!.hi;
  }
  for (let node = size - 1; node >= 1; node--) ends[node] = Math.max(ends[2 * node]!, ends[2 * node + 1]!);
  return { offsets, movers, starts, ends };
}

function visitGlideCrossings(glide: TableGlide, top: number, bottom: number, out: number[] | null, node = 1, first = 0, span = glide.ends.length / 2): boolean {
  if (first >= glide.starts.length || glide.starts[first]! >= bottom || glide.ends[node]! <= top) return false;
  if (span === 1) {
    if (!out) return true;
    out.push(glide.movers[first]!.index);
    return false;
  }
  const half = span / 2;
  return visitGlideCrossings(glide, top, bottom, out, 2 * node, first, half) ||
    visitGlideCrossings(glide, top, bottom, out, 2 * node + 1, first + half, half);
}

export function hasGlideCrossing(glide: TableGlide, top: number, bottom: number): boolean {
  return visitGlideCrossings(glide, top, bottom, null);
}

export function glideCrossing(glide: TableGlide, top: number, bottom: number): number[] {
  const out: number[] = [];
  visitGlideCrossings(glide, top, bottom, out);
  return out;
}

function mergeInterval(intervals: [number, number][], a: number, b: number): void {
  if (b <= a) return;
  let i = 0;
  while (i < intervals.length && intervals[i]![1] < a) i++;
  let start = a;
  let end = b;
  let remove = 0;
  while (i + remove < intervals.length && intervals[i + remove]![0] <= end) {
    const cur = intervals[i + remove]!;
    if (cur[0] < start) start = cur[0];
    if (cur[1] > end) end = cur[1];
    remove++;
  }
  intervals.splice(i, remove, [start, end]);
}

function isIntervalCovered(intervals: readonly [number, number][], a: number, b: number): boolean {
  for (let i = 0; i < intervals.length; i++) {
    const cur = intervals[i]!;
    if (cur[0] <= a && cur[1] >= b) return true;
    if (cur[0] > a) return false;
  }
  return false;
}

export function visibleGlidingIndices(
  rows: readonly FlatTableRow[],
  glide: TableGlide,
  first: number,
  last: number,
  scrollTop: number,
  height: number,
  now: number,
  glideDuration: number,
  rowGeom: RowLayoutGeometry | null,
  rowHeight: number,
  ordered = true,
): number[] {
  const out: number[] = [];
  const start = Math.max(0, first);
  const end = Math.min(rows.length, last);
  const check = (index: number) => {
    const item = rows[index];
    if (!item) return;
    const h = rowGeom ? rowGeom.heights[index]! : rowHeight;
    const top = rowGeom ? rowGeom.tops[index]! : index * rowHeight;
    const y = top - scrollTop + (item.kind === 'data' ? glideOffset(glide.offsets.get(item.key), now, glideDuration) : 0);
    if (y + h > 0 && y < height) out.push(index);
  };
  for (let index = start; index < end; index++) check(index);
  for (const index of glideCrossing(glide, scrollTop, scrollTop + height)) {
    if (index < start || index >= end) check(index);
  }
  if (ordered) out.sort((a, b) => a - b);
  return out;
}

function resolvedArtDimensions(resolved: ArtResult): { readonly width: number; readonly height: number } | null {
  if (resolved.status !== 'ready') return null;
  if (resolved.frame.outputWidth > 0 && resolved.frame.outputHeight > 0) {
    return { width: resolved.frame.outputWidth, height: resolved.frame.outputHeight };
  }
  if (resolved.bitmap && resolved.bitmap.width > 0 && resolved.bitmap.height > 0) {
    return { width: resolved.bitmap.width, height: resolved.bitmap.height };
  }
  if (resolved.frame.frameWidth > 0 && resolved.frame.frameHeight > 0) {
    return { width: resolved.frame.frameWidth, height: resolved.frame.frameHeight };
  }
  return null;
}

function iconArtSlotWidth(art: ArtResult | null | undefined, size: number, artCache?: LaneArtCache): number {
  if (!art) return size;
  return iconStripSlotWidth(toArtSlotState(resolveCachedArt(art, artCache)), size);
}

function iconArtNeedsDecode(art: ArtResult | null | undefined, artCache?: LaneArtCache): boolean {
  if (!art) return false;
  const resolved = resolveCachedArt(art, artCache);
  return resolved.status === 'ready' && !resolved.bitmap && resolvedArtDimensions(resolved) === null;
}

function fittedArtSize(
  art: ArtResult | null | undefined,
  slotW: number,
  slotH: number,
  artCache?: LaneArtCache,
  trim = false,
): { readonly width: number; readonly height: number } {
  if (!art) return { width: slotW, height: slotH };
  const resolved = resolveCachedArt(art, artCache);
  if (resolved.status === 'ready') {
    if (trim) {
      const bitmap = resolved.bitmap;
      if (bitmap) {
        const box = artDrawnBounds(bitmap as unknown as CanvasImageSource, Math.max(1, bitmap.width), Math.max(1, bitmap.height));
        const sw = box?.width ?? Math.max(1, bitmap.width);
        const sh = box?.height ?? Math.max(1, bitmap.height);
        artCache?.rememberTrimmed(resolved.url, sw, sh);
        const scale = Math.min(slotW / sw, slotH / sh);
        return { width: sw * scale, height: sh * scale };
      }
      const cachedTrim = artCache?.trimmedOfUrl(resolved.url);
      if (cachedTrim) {
        const scale = Math.min(slotW / cachedTrim[0], slotH / cachedTrim[1]);
        return { width: cachedTrim[0] * scale, height: cachedTrim[1] * scale };
      }
      return { width: slotW, height: slotH };
    }
    const dims = resolvedArtDimensions(resolved);
    if (dims) {
      const scale = Math.min(slotW / dims.width, slotH / dims.height);
      return { width: dims.width * scale, height: dims.height * scale };
    }
  }
  return { width: slotW, height: slotH };
}

function inlineRunsWidth(
  text: string,
  maxWidth: number,
  font: string,
  measure: (text: string, font: string) => number,
): number {
  if (maxWidth <= 0 || !text) return 0;
  if (text.indexOf('\uFFFC') < 0) {
    const fitted = ellipsise(text, maxWidth, s => measure(s, font));
    return fitted ? measure(fitted, font) : 0;
  }
  const parts = text.split('\uFFFC');
  let cursor = 0;
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i]!;
    if (part) {
      const rem = maxWidth - cursor;
      if (rem <= 0) break;
      const fitted = ellipsise(part, rem, s => measure(s, font));
      if (fitted) cursor += measure(fitted, font);
      if (fitted !== part) break;
    }
    if (i + 1 < parts.length) {
      const lead = cursor > 0 ? 4 : 0;
      const boxW = Math.min(48, Math.max(16, maxWidth - cursor - lead));
      if (cursor + lead + 16 > maxWidth) break;
      cursor += lead + boxW + (parts[i + 1] ? 4 : 0);
    }
  }
  return cursor;
}

export interface TableVisibleArtViewport {
  readonly width: number;
  readonly scrollLeft: number;
  readonly startX: number;
  readonly pinnedCount: number;
  readonly pinnedWidth: number;
  readonly artCache?: LaneArtCache;
  readonly treeLevels?: number;
  readonly hideHeader?: boolean;
  readonly fontSize?: { readonly small: number; readonly body: number };
}

type VisibleArtColumn = Pick<
  FamilyColumnLayout,
  'key' | 'width' | 'display' | 'skillIcon' | 'leadInset' | 'trailInset' | 'inlineFlags' | 'cellKind' | 'notRecordedReason' | 'narrow' | 'narrowed' | 'valueFormat' | 'numeric'
>;

export function collectVisibleTableArt(
  rows: readonly FlatTableRow[],
  indices: readonly number[],
  rowY: (index: number) => number,
  height: number,
  rowGeom: RowLayoutGeometry | null,
  rowHeight: number,
  columns: readonly VisibleArtColumn[],
  density: TableDensity,
  viewport: TableVisibleArtViewport,
  themeFont: string,
  measure: (text: string, font: string) => number,
  cullOccluded: boolean,
  items: ArtUrlInput[],
): void {
  const pinned = Math.min(columns.length, viewport.pinnedCount);
  const treeCol = treeColumnOf(columns as readonly FamilyColumnLayout[]);
  const hasActionCol = columns.some(col => col.cellKind === 'action');
  const treeLevels = viewport.treeLevels ?? 0;
  const visibleCols: {
    readonly col: VisibleArtColumn;
    readonly index: number;
    readonly pinned: boolean;
    readonly screenLeft: number;
    readonly screenRight: number;
    readonly clipLeft: number;
    readonly clipRight: number;
  }[] = [];
  let colX = viewport.startX;
  for (let c = 0; c < columns.length; c++) {
    const col = columns[c]!;
    const isPinned = c < pinned;
    const screenLeft = isPinned ? colX : colX - viewport.scrollLeft;
    const screenRight = screenLeft + col.width;
    const clipLeft = isPinned ? 0 : viewport.pinnedWidth;
    const clipRight = viewport.width;
    colX += col.width;
    if (col.notRecordedReason || col.width <= 0 || clipLeft >= clipRight || screenLeft >= clipRight) continue;
    visibleCols.push({ col, index: c, pinned: isPinned, screenLeft, screenRight, clipLeft, clipRight });
  }
  if (indices.length === 0) return;
  const coveredAll: [number, number][] = [];
  const coveredData: [number, number][] = [];
  const countFont = `600 11px ${themeFont}`;
  const moreFont = `500 11.5px ${themeFont}`;
  const prefixFont = `600 ${density === 'compact' ? 13 : 13.5}px ${themeFont}`;
  const detailFont = `400 ${DETAIL_TEXT_SIZE}px ${themeFont}`;
  const markTheme = { font: themeFont, fontSize: viewport.fontSize ?? { small: 11, body: 12 } };
  const rectVisible = (
    covered: readonly [number, number][],
    left: number,
    right: number,
    top: number,
    bot: number,
    clipLeft: number,
    clipRight: number,
  ): boolean => {
    if (clipLeft >= clipRight || right <= clipLeft || left >= clipRight) return false;
    const artTop = Math.max(0, top);
    const artBot = Math.min(height, bot);
    if (artBot <= artTop) return false;
    return !cullOccluded || !isIntervalCovered(covered, artTop, artBot);
  };
  const artVisible = (covered: readonly [number, number][], center: number, artH: number): boolean => {
    const top = Math.round(center - artH / 2);
    const artTop = Math.max(0, top);
    const artBot = Math.min(height, top + artH);
    if (artBot <= artTop) return false;
    return !cullOccluded || !isIntervalCovered(covered, artTop, artBot);
  };
  const slotImageVisible = (
    covered: readonly [number, number][],
    art: ArtResult,
    slotLeft: number,
    centerY: number,
    slotW: number,
    slotH: number,
    clipLeft: number,
    clipRight: number,
    trim = false,
  ): boolean => {
    const fit = fittedArtSize(art, slotW, slotH, viewport.artCache, trim);
    const slotTop = Math.round(centerY - slotH / 2);
    const imgLeft = slotLeft + (slotW - fit.width) / 2;
    const imgTop = slotTop + (slotH - fit.height) / 2;
    return rectVisible(covered, imgLeft, imgLeft + fit.width, imgTop, imgTop + fit.height, clipLeft, clipRight);
  };
  for (let i = indices.length - 1; i >= 0; i--) {
    const index = indices[i]!;
    const item = rows[index];
    if (!item) continue;
    const h = rowGeom ? rowGeom.heights[index]! : rowHeight;
    const y = rowY(index);
    const y0 = Math.max(0, y);
    const y1 = Math.min(height, y + h);
    if (item.kind !== 'data') {
      if (cullOccluded && y1 > y0) mergeInterval(coveredAll, y0, y1);
      continue;
    }
    const detail = rowGeom?.detailRows?.get(index);
    if (detail) {
      const detailKey = item.row.detailColumn;
      const cell = detailKey ? item.row.cells[detailKey] : undefined;
      if (cell) {
        const pinnedEdge = Math.min(viewport.width, viewport.pinnedWidth);
        const detailFlagVisible = (flLeft: number, flRight: number, flTop: number, flBot: number): boolean =>
          (pinnedEdge > 0 && rectVisible(coveredData, flLeft, flRight, flTop, flBot, 0, pinnedEdge))
          || rectVisible(coveredAll, flLeft, flRight, flTop, flBot, pinnedEdge, viewport.width);
        if (cell.heading) {
          const headMid = y + DETAIL_HEAD_LEAD + DETAIL_HEAD_HEIGHT / 2;
          const headTop = Math.round(headMid - FLAG_SLOT_HEIGHT / 2);
          if (detailFlagVisible(detail.left, detail.left + FLAG_SLOT_WIDTH, headTop, headTop + FLAG_SLOT_HEIGHT)) {
            items.push(cell.heading.flag.art);
          }
        }
        const flags = cell.flags;
        const detailRight = detail.left + detail.width;
        if (flags?.length && detail.width > 0 && detail.left < viewport.width) {
          const lineCovered = pinnedEdge > 0 ? coveredData : coveredAll;
          for (let line = 0; line < detail.lines.length; line++) {
            const mid = y + detail.top + line * DETAIL_LINE_HEIGHT + DETAIL_LINE_HEIGHT / 2;
            if (!artVisible(lineCovered, mid, DETAIL_FLAG.height)) continue;
            const flTop = Math.round(mid - DETAIL_FLAG.height / 2);
            const start = detail.lines[line]!;
            const end = detail.lines[line + 1] ?? flags.length;
            let fx = detail.left;
            for (let f = start; f < end && fx < detailRight && fx < viewport.width; f++) {
              const fl = flags[f]!;
              if (detailFlagVisible(fx, fx + DETAIL_FLAG.width, flTop, flTop + DETAIL_FLAG.height)) {
                items.push(fl.art);
              }
              fx += DETAIL_FLAG.width + DETAIL_FLAG.gap;
              if (fl.label) fx += inlineRunsWidth(fl.label, detailRight - fx, detailFont, measure);
              if (fl.note && detailRight - fx > DETAIL_NOTE_GAP) {
                fx += DETAIL_NOTE_GAP + inlineRunsWidth(detailNote(fl.note), detailRight - fx - DETAIL_NOTE_GAP, detailFont, measure);
              }
              fx += DETAIL_CHIP_GAP;
            }
          }
        }
      }
    } else {
      const contentH = item.isLiveEnd ? h - LIVE_END_GAP : h;
      const middle = y + contentH / 2;
      for (let vc = 0; vc < visibleCols.length; vc++) {
        const entry = visibleCols[vc]!;
        const col = entry.col;
        const cell = item.row.cells[col.key];
        const covered = entry.pinned ? coveredData : coveredAll;
        if (cell?.blank) continue;
        let left = entry.screenLeft + CELL_INSET + (col.leadInset ?? 0);
        const right = entry.screenRight - CELL_INSET - (col.trailInset ?? 0);
        if (entry.index === treeCol) {
          if (hasActionCol || (item.row.pageGroup && col.cellKind === 'date')) {
            if (item.level > 1 && !item.row.pageParent) left += (item.level - 1) * TREE_CHEVRON_LANE;
          } else {
            left += treeTextStart(item.level, treeLevels);
            if (!item.isTreeGroup && item.level > 1 && cell?.iconSlot && !showsIcon(col.display) && cell.art) {
              const fx = Math.max(entry.screenLeft, left - FLAG_SLOT_WIDTH - CHILD_FLAG_GAP);
              const fy = Math.round(middle - FLAG_SLOT_HEIGHT / 2);
              if (rectVisible(covered, fx, fx + FLAG_SLOT_WIDTH, fy, fy + FLAG_SLOT_HEIGHT, entry.clipLeft, entry.clipRight)) {
                items.push(cell.art);
              }
            }
          }
        }
        if (col.cellKind === 'kind') {
          if (cell && !cell.glyph && cell.art) {
            const centerX = entry.screenLeft + (col.leadInset ?? 0) + (col.width - (col.leadInset ?? 0) - (col.trailInset ?? 0)) / 2;
            const slot = cell.artTile ? KIND_TILE_ART : artGeometry(cell.artShape ?? 'square', density, contentH);
            if (slotImageVisible(covered, cell.art, Math.round(centerX - slot.width / 2), middle, slot.width, slot.height, entry.clipLeft, entry.clipRight, cell.artShape === 'drawn')) {
              items.push(cell.art);
            }
          }
          continue;
        }
        if (cell?.glyph) {
          left += glyphAdvance(cell.glyph);
        }
        if (col.cellKind === 'skill') {
          const badgeArt = cell?.art ?? col.skillIcon;
          if (cell && badgeArt) {
            const formatted = formatValueState(cell.value, cell.format ?? col.valueFormat);
            if (formatted.kind !== 'notRecorded' && formatted.kind !== 'notApplicable' && formatted.kind !== 'pending') {
              const badge = col.key === 'skill' || col.key === 'level';
              const iconW = badge ? 18 : 24;
              const iconH = badge ? 26 : 22;
              const numW = Math.max(14, measure(formatted.text, `${badge ? 600 : 500} 13.5px ${themeFont}`));
              const iconX = right - numW - 6 - iconW;
              if (iconX >= left && slotImageVisible(covered, typeof badgeArt === 'string' ? readyArt(badgeArt) : badgeArt, iconX, middle, iconW, iconH, entry.clipLeft, entry.clipRight)) {
                items.push(badgeArt);
              }
            }
          }
          continue;
        }
        if (col.inlineFlags?.length && !cell?.inlinePrefix) {
          const marks: TableCell[] = [];
          for (const inlineKey of col.inlineFlags) {
            const tail = item.row.cells[inlineKey];
            if (tail && !tail.blank) marks.push(tail);
          }
          if (marks.length) {
            const gap = 6;
            let reserved = 0;
            for (const mark of marks) {
              reserved += (mark.glyph ? glyphAdvance(mark.glyph) : 0)
                + (mark.flags?.length ?? 0) * (FLAG_SLOT_WIDTH + gap)
                + (!mark.flags?.length && mark.value.kind === 'notRecorded' ? 16 + gap : 0);
            }
            const room = Math.max(0, right - left - reserved);
            const formatted = cell ? formatValueState(cell.value, cell.format ?? col.valueFormat) : formatValueState(notApplicable(''));
            const missing = formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable';
            const textW = missing
              ? missingMarkWidth(room, measure, markTheme, { figure: cell?.missingFigure })
              : inlineRunsWidth((col.narrowed ? cell?.narrowText : undefined) ?? (formatted.kind === 'pending' ? formatted.label : formatted.text), room, prefixFont, measure);
            let at = left + textW + gap;
            const fy = Math.round(middle - FLAG_SLOT_HEIGHT / 2);
            for (const mark of marks) {
              if (mark.glyph) at += glyphAdvance(mark.glyph);
              for (const fl of mark.flags ?? []) {
                if (rectVisible(covered, at, at + FLAG_SLOT_WIDTH, fy, fy + FLAG_SLOT_HEIGHT, entry.clipLeft, entry.clipRight)) {
                  items.push(fl.art);
                }
                at += FLAG_SLOT_WIDTH + gap;
              }
              if (!mark.flags?.length && mark.value.kind === 'notRecorded') at += 16 + gap;
            }
            continue;
          }
        }
        if (!cell) continue;
        if (cell.heading && artVisible(covered, middle, FLAG_SLOT_HEIGHT)) items.push(cell.heading.flag.art);
        if (cell.icons?.length) {
          const iconSize = cell.iconStripKind === 'medals' ? MEDAL_ICON_SIZE : TRAIT_ICON_SIZE;
          if (right > entry.clipLeft && left < entry.clipRight && artVisible(covered, middle, iconSize)) {
            const icons = cell.icons;
            const available = Math.max(0, right - left);
            const shown = fitIconStripCount(
              available,
              icons.length,
              idx => iconArtSlotWidth(icons[idx]!.art, iconSize, viewport.artCache),
              idx => icons[idx]!.count,
              measure,
              themeFont,
            );
            let cursor = left;
            let minCursor = left;
            let addedUndecoded = false;
            let canShiftEdge = false;
            let reachedEnd = true;
            for (let k = 0; k < shown; k++) {
              if (cursor >= entry.clipRight) {
                if (minCursor < entry.clipRight) canShiftEdge = true;
                reachedEnd = false;
                break;
              }
              const icon = icons[k]!;
              const w = iconArtSlotWidth(icon.art, iconSize, viewport.artCache);
              const needsDecode = iconArtNeedsDecode(icon.art, viewport.artCache);
              const minW = needsDecode ? 1 : w;
              const countW = icon.count && icon.count > 1 ? measure(`\u00d7${icon.count}`, countFont) + 3 : 0;
              if (cursor + w > entry.clipLeft && slotImageVisible(covered, icon.art, cursor, middle, w, iconSize, entry.clipLeft, entry.clipRight)) {
                items.push(icon.art);
                if (needsDecode) addedUndecoded = true;
                if (minCursor < cursor) canShiftEdge = true;
              }
              if (countW > 0 && artVisible(covered, middle, 11)) {
                if (cursor + w < entry.clipRight && cursor + w + countW > entry.clipLeft && (minCursor < cursor || needsDecode)) {
                  canShiftEdge = true;
                } else if (cursor + w >= entry.clipRight && minCursor + minW < entry.clipRight) {
                  canShiftEdge = true;
                }
              }
              cursor += w + countW + ICON_STRIP_GAP;
              minCursor += minW + countW + ICON_STRIP_GAP;
            }
            if (reachedEnd && shown < icons.length && cursor <= right && artVisible(covered, middle, 12)) {
              const moreW = measure(`+${icons.length - shown}`, moreFont);
              if (cursor < entry.clipRight && cursor + moreW > entry.clipLeft && minCursor < cursor) {
                canShiftEdge = true;
              } else if (cursor >= entry.clipRight && minCursor < entry.clipRight) {
                canShiftEdge = true;
              }
            }
            if (!addedUndecoded && (shown < icons.length || canShiftEdge)) {
              const maxPossible = shown < icons.length
                ? fitIconStripCount(
                  available,
                  icons.length,
                  idx => iconArtNeedsDecode(icons[idx]!.art, viewport.artCache) ? 1 : iconArtSlotWidth(icons[idx]!.art, iconSize, viewport.artCache),
                  idx => icons[idx]!.count,
                  measure,
                  themeFont,
                )
                : shown;
              if (maxPossible > shown || canShiftEdge) {
                let firstUndecoded = -1;
                for (let k = 0; k < maxPossible; k++) {
                  if (iconArtNeedsDecode(icons[k]!.art, viewport.artCache)) {
                    firstUndecoded = k;
                    break;
                  }
                }
                if (firstUndecoded !== -1 && (firstUndecoded < shown || cursor < entry.clipRight)) {
                  items.push(icons[firstUndecoded]!.art);
                }
              }
            }
          }
        }
        if (cell.inlinePrefix && !col.numeric) {
          const room = Math.max(0, right - left);
          const prefixW = measure(cell.inlinePrefix, prefixFont);
          if (cell.flags?.length || prefixW + 10 < room) {
            left += Math.min(room, prefixW) + 8;
            if (cell.flags?.length && right - left < FLAG_SLOT_WIDTH) continue;
          }
        }
        if (cell.flags?.length) {
          const fy = Math.round(middle - FLAG_SLOT_HEIGHT / 2);
          let fx = left;
          for (const fl of cell.flags) {
            if (col.narrow === 'flag' && fx + FLAG_SLOT_WIDTH > right) break;
            if (rectVisible(covered, fx, fx + FLAG_SLOT_WIDTH, fy, fy + FLAG_SLOT_HEIGHT, entry.clipLeft, entry.clipRight)) {
              items.push(fl.art);
            }
            fx += FLAG_SLOT_WIDTH + 6;
          }
        } else if (cell.art) {
          if (cell.artShape) {
            const slot = artGeometry(cell.artShape, density, contentH);
            if (slotImageVisible(covered, cell.art, left, middle, slot.width, slot.height, entry.clipLeft, entry.clipRight)) {
              items.push(cell.art);
            }
          } else if (cell.iconSlot && showsIcon(col.display)) {
            if (col.display === 'flag' || col.display === 'flagName') {
              const fy = Math.round(middle - FLAG_SLOT_HEIGHT / 2);
              if (rectVisible(covered, left, left + FLAG_SLOT_WIDTH, fy, fy + FLAG_SLOT_HEIGHT, entry.clipLeft, entry.clipRight)) {
                items.push(cell.art);
              }
            } else {
              const slot = artGeometry('square', density, contentH);
              if (slotImageVisible(covered, cell.art, left, middle, slot.width, slot.height, entry.clipLeft, entry.clipRight)) {
                items.push(cell.art);
              }
            }
          }
        }
        if (cell.unitArt && col.numeric && col.cellKind !== 'outOf') {
          const formatted = formatValueState(cell.value, cell.format ?? col.valueFormat);
          if (formatted.kind !== 'notRecorded' && formatted.kind !== 'notApplicable' && formatted.kind !== 'pending') {
            const hasSplit = Boolean(cell.splitBar?.length) && !(col.narrowed && col.narrow === 'total');
            const textMid = hasSplit ? middle - 3 : middle;
            const avail = Math.max(0, right - left);
            const starts = viewport.hideHeader === true && (col.cellKind === undefined || col.cellKind === 'track');
            if (starts) {
              if (avail >= UNIT_ART_SIZE + UNIT_ART_GAP && slotImageVisible(covered, cell.unitArt, left, textMid, UNIT_ART_SIZE, UNIT_ART_SIZE, entry.clipLeft, entry.clipRight)) {
                items.push(cell.unitArt);
              }
            } else {
              let numRight = right;
              if (cell.unitSuffix) {
                const suffixW = measure(` ${cell.unitSuffix}`, `400 11.5px ${themeFont}`);
                if (suffixW + 12 <= avail) numRight = right - suffixW;
              }
              const numFont = `500 13px ${themeFont}`;
              const shownNum = ellipsise(formatted.text, Math.max(0, numRight - left), s => measure(s, numFont));
              const iconX = Math.round(numRight - measure(shownNum, numFont) - UNIT_ART_SIZE - UNIT_ART_GAP);
              if (iconX >= left && slotImageVisible(covered, cell.unitArt, iconX, textMid, UNIT_ART_SIZE, UNIT_ART_SIZE, entry.clipLeft, entry.clipRight)) {
                items.push(cell.unitArt);
              }
            }
          }
        }
      }
    }
    if (cullOccluded) {
      mergeInterval(coveredAll, y0, y1);
      if (pinned > 0) mergeInterval(coveredData, y0, y1);
      const allFull = coveredAll.length === 1 && coveredAll[0]![0] <= 0 && coveredAll[0]![1] >= height;
      const dataFull = pinned === 0 || (coveredData.length === 1 && coveredData[0]![0] <= 0 && coveredData[0]![1] >= height);
      if (allFull && dataFull) break;
    }
  }
}

export function collectVisibleGlidingArt(
  rows: readonly FlatTableRow[],
  glide: TableGlide,
  first: number,
  last: number,
  scrollTop: number,
  height: number,
  now: number,
  glideDuration: number,
  rowGeom: RowLayoutGeometry | null,
  rowHeight: number,
  columns: readonly VisibleArtColumn[],
  density: TableDensity,
  viewport: TableVisibleArtViewport,
  themeFont: string,
  measure: (text: string, font: string) => number,
  items: ArtUrlInput[],
): void {
  const indices = visibleGlidingIndices(rows, glide, first, last, scrollTop, height, now, glideDuration, rowGeom, rowHeight, true);
  collectVisibleTableArt(
    rows,
    indices,
    index => {
      const item = rows[index]!;
      const top = rowGeom ? rowGeom.tops[index]! : index * rowHeight;
      return top - scrollTop + (item.kind === 'data' ? glideOffset(glide.offsets.get(item.key), now, glideDuration) : 0);
    },
    height,
    rowGeom,
    rowHeight,
    columns,
    density,
    viewport,
    themeFont,
    measure,
    true,
    items,
  );
}

export function hitFamilyTable(
  layout: FamilyTableLayout,
  rows: readonly FlatTableRow[],
  point: { readonly x: number; readonly y: number },
  rowHeight: number,
  scrollLeft: number,
  scrollTop: number,
  rankShown: boolean,
  rankWidth: number,
  glide?: TableGlide,
  now?: number,
  glideDuration?: number,
  rowGeom?: RowLayoutGeometry | null,
): { readonly kind: 'row' | 'empty'; readonly rowKey?: string; readonly column?: string; readonly toggle?: boolean; readonly action?: boolean } {
  let item: FlatTableRow | undefined;
  const absY = point.y + scrollTop;
  let at = -1;
  if (rowGeom) {
    let lo = 0;
    let hi = rows.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (rowGeom.tops[mid]! + rowGeom.heights[mid]! <= absY) lo = mid + 1;
      else hi = mid;
    }
    if (lo < rows.length && absY >= rowGeom.tops[lo]! && absY < rowGeom.tops[lo]! + rowGeom.heights[lo]!) at = lo;
  } else {
    at = Math.floor(absY / rowHeight);
  }
  if (glide && glide.offsets.size > 0 && glideDuration && glideDuration > 0 && now !== undefined) {
    const candidates = glideCrossing(glide, absY, absY + 1);
    if (at >= 0 && at < rows.length) candidates.push(at);
    candidates.sort((a, b) => b - a);
    for (const i of candidates) {
      const candidate = rows[i]!;
      const top = rowGeom ? rowGeom.tops[i]! : i * rowHeight;
      const h = rowGeom ? rowGeom.heights[i]! : rowHeight;
      const y = top - scrollTop + (candidate.kind === 'data' ? glideOffset(glide.offsets.get(candidate.key), now, glideDuration) : 0);
      if (point.y >= y && point.y < y + h) {
        item = candidate;
        break;
      }
    }
  } else {
    item = rows[at];
  }
  if (!item) return { kind: 'empty' };
  if (item.kind === 'gap' || item.kind === 'refused') return { kind: 'row', rowKey: item.key };
  if (item.kind !== 'data') return { kind: 'empty' };
  const rank = rankShown ? rankWidth : 0;
  const pinned = layout.scrollable && point.x < layout.pinnedWidth;
  const bodyX = pinned ? point.x : point.x + scrollLeft;
  let x = rank;
  for (let colIndex = 0; colIndex < layout.columns.length; colIndex++) {
    const col = layout.columns[colIndex]!;
    if (bodyX >= x && bodyX < x + col.width) {
      const actionCell = item.row.cells[col.key];
      const hasAction = col.cellKind === 'action' && actionCell && !actionCell.blank && actionCell.actionLabel !== '';
      const toggle = item.isTreeGroup && (
        (col.cellKind === 'action' && !hasAction)
        || (colIndex === treeColumnOf(layout.columns) && bodyX - x <= CELL_INSET + (col.leadInset ?? 0) + treeTextStart(item.level, treeLevelsOf(rows)) + 8)
        || (item.row.meta === undefined && layout.columns.some(c => c.cellKind === 'action'))
      );
      const action = col.cellKind === 'action' && !toggle;
      return { kind: 'row', rowKey: item.key, column: col.key, toggle, action };
    }
    x += col.width;
  }
  return { kind: 'row', rowKey: item.key };
}

export function hitPanes(
  layout: FamilyTableLayout,
  panes: readonly ResolvedPane[],
  geometry: PaneGeometry,
  point: { readonly x: number; readonly y: number },
  rowHeight: number,
  scroll: PaneScroll,
  scrollTop: number,
): string | null {
  for (let p = 0; p < panes.length; p++) {
    const x0 = geometry.xs[p]!;
    if (point.x < x0 || point.x >= x0 + geometry.width) continue;
    const pane = panes[p]!;
    const at = hitFamilyTable(layout, pane.rows, { x: point.x - x0, y: point.y }, rowHeight, scroll.left, scrollTop, false, 0, undefined, undefined, undefined, pane.geom);
    if (at.kind !== 'row' || !at.rowKey) return null;
    return at.toggle ? `toggle:${at.rowKey}` : `row:${at.rowKey}`;
  }
  return null;
}

export function drawPaneFrame(
  ctx: TableCanvasContext,
  geometry: PaneGeometry,
  layout: FamilyTableLayout,
  scroll: PaneScroll,
  height: number,
  theme: TableTheme,
): void {
  const tokens = resolveKitTokens(theme);
  for (let p = 1; p < geometry.xs.length; p++) {
    const gutterLeft = geometry.xs[p - 1]! + geometry.width;
    ctx.fillStyle = theme.surface;
    ctx.fillRect(gutterLeft, 0, geometry.gutter, height);
    ctx.fillStyle = theme.hairlineStrong ?? theme.line;
    ctx.fillRect(Math.round(gutterLeft + geometry.gutter / 2), 0, 1, height);
  }
  const usedRight = geometry.xs[geometry.xs.length - 1]! + geometry.width;
  if (geometry.view > usedRight) {
    ctx.fillStyle = theme.surface;
    ctx.fillRect(usedRight, 0, geometry.view - usedRight, height);
  }
  if (scroll.overflow <= 0) return;
  const bodyWidth = Math.max(1, geometry.width - layout.pinnedWidth);
  const thumbWidth = Math.max(16, Math.round(bodyWidth * (geometry.width - layout.pinnedWidth) / Math.max(1, layout.totalWidth - layout.pinnedWidth)));
  const thumbX = Math.round((bodyWidth - thumbWidth) * (scroll.left / scroll.overflow));
  const y = height - PANE_INDICATOR_HEIGHT - 2;
  for (const x0 of geometry.xs) {
    const fadeRight = x0 + geometry.width;
    if (scroll.left < scroll.overflow - 0.5) drawScrollFade(ctx, fadeRight - PANE_FADE_WIDTH, 0, PANE_FADE_WIDTH, height, theme, 'right');
    const trackX = x0 + layout.pinnedWidth;
    ctx.fillStyle = tokens.barTrack;
    ctx.beginPath();
    ctx.roundRect(trackX, y, bodyWidth, PANE_INDICATOR_HEIGHT, PANE_INDICATOR_HEIGHT / 2);
    ctx.fill();
    ctx.fillStyle = theme.muted;
    ctx.beginPath();
    ctx.roundRect(trackX + thumbX, y, thumbWidth, PANE_INDICATOR_HEIGHT, PANE_INDICATOR_HEIGHT / 2);
    ctx.fill();
  }
}
