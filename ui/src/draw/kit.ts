import { LABEL_WEIGHT, VALUE_WEIGHT } from '../charts/draw.ts';
import {
  drawFittedLabel,
  ellipsise,
  fitLine,
  graphemes,
  labelFitsTwoLines,
  labelLineHeight,
  type FittedLabel,
  type FittedLine,
} from '../charts/labels.ts';
import { clearTextWidthCache, measureCanvasText, textWidth, type Canvas2D } from '../charts/textWidth.ts';
import type { FamilyTheme } from '../families/contract';
import {
  drawHatchRect,
  MISSING_BOX_GAP,
  MISSING_BOX_SIZE,
  PARTIAL_TAB_HEIGHT,
  PARTIAL_TAB_WIDTH,
} from '../families/valueCanvas.ts';
import {
  formatDateOnly,
  formatDateParts,
  GAME_WINDOW_MISSING_LABEL,
  NOT_APPLICABLE_LABEL,
  notRecordedLabel,
} from '../families/valueFormat.ts';
import { paletteCache, readableColour } from '../charts/palette.ts';
import { markColours } from '../shell/appearance/themes.ts';
import { niceStep } from '../charts/valueAxis.ts';
import { colorCache, contrast, extendColor, isDark, mix, mixRgb, parseColor, parseColorAlpha, rgbaColor } from '../shell/appearance/color.ts';
import { engineHour, gameDate, gameYear, HOUR_EPOCH, monthNames } from '../dashboard/calendar.ts';
import { timeChartCacheState } from '../families/time-chart/cacheControl.ts';

export type RowDensity = 'compact' | 'normal' | 'comfortable';

const snappedContexts = new WeakSet<Canvas2D>();

export function snapCanvasText(ctx: Canvas2D): void {
  if (snappedContexts.has(ctx)) return;
  snappedContexts.add(ctx);
  const fill = ctx.fillText;
  ctx.fillText = (text, x, y, maxWidth) => {
    const t = ctx.getTransform();
    const px = t.a * x + t.c * y + t.e;
    const py = t.b * x + t.d * y + t.f;
    const dx = Math.round(px) - px, dy = Math.round(py) - py;
    const determinant = t.a * t.d - t.b * t.c;
    if (determinant !== 0) {
      x += (t.d * dx - t.c * dy) / determinant;
      y += (t.a * dy - t.b * dx) / determinant;
    }
    if (maxWidth === undefined) fill.call(ctx, text, x, y);
    else fill.call(ctx, text, x, y, maxWidth);
  };
}

export const BEHIND_BAR_HEIGHT: Readonly<Record<RowDensity, number>> = {
  compact: 18,
  normal: 22,
  comfortable: 26,
};

export const BEHIND_BAR_MAX_WIDTH = 64;
export const BEHIND_BAR_MIN_WIDTH = 6;
export const BEHIND_BAR_OFFSET = 7;
export const ROW_CORNER_RADIUS = 7;
export const FLAG_SLOT_WIDTH = 20;
export const FLAG_SLOT_HEIGHT = 13;
export const FLAG_LOSER_ALPHA = 0.35;
export const DATE_COLUMN_WIDTH = 50;
export const SERIES_MISSING_STRIP_HEIGHT = 5;
export const TRAIT_ICON_SIZE = 23;
export const MEDAL_ICON_SIZE = 28;
export const ICON_STRIP_GAP = 5;

const INITIALS_CACHE_MAX = 1024;
const initialsCache = new Map<string, string>();

function isDarkTheme(theme: Pick<FamilyTheme, 'surface' | 'dark'>): boolean {
  if (theme.dark !== undefined) return theme.dark;
  const rgb = parseColor(theme.surface);
  return !rgb || isDark(rgb);
}

type KitTokenInput = Pick<FamilyTheme, 'surface' | 'text' | 'muted' | 'accent' | 'red' | 'green'> & Partial<FamilyTheme>;

function buildKitTokens(theme: KitTokenInput) {
  const dark = isDarkTheme(theme);
  const ink = extendColor(theme.surface, theme.text);
  const veil = (alpha: number) => rgbaColor(ink, alpha);
  const textSecondary = mix(theme.text, theme.muted, 1 / 3);
  const marks = markColours(theme.red, theme.green);
  return {
    dark,
    ink,
    raised: theme.raised ?? theme.surface,
    hairline: theme.hairline ?? veil(dark ? 0.05 : 0.07),
    hairlineStrong: theme.hairlineStrong ?? veil(dark ? 0.085 : 0.11),
    bandDivider: theme.bandDivider ?? veil(dark ? 0.12 : 0.15),
    headerBand: theme.headerBand ?? veil(dark ? 0.025 : 0.03),
    rowHover: theme.rowHover ?? veil(dark ? 0.045 : 0.05),
    rowSelected: theme.rowSelected ?? rgbaColor(theme.accent, 0.14),
    childTint: theme.childTint ?? veil(dark ? 0.018 : 0.025),
    shortage: theme.shortage ?? readableColour(marks.shortInk, theme.surface),
    shortFill: marks.shortFill,
    goodFill: marks.goodFill,
    goodInk: readableColour(marks.goodInk, theme.surface),
    barMuted: theme.barMuted ?? veil(dark ? 0.085 : 0.095),
    barTrack: theme.barTrack ?? veil(dark ? 0.08 : 0.1),
    gridLine: theme.gridLine ?? veil(dark ? 0.055 : 0.07),
    zeroLine: theme.zeroLine ?? veil(dark ? 0.18 : 0.22),
    playhead: theme.playhead ?? veil(dark ? 0.28 : 0.34),
    selectedHour: rgbaColor(theme.accent, dark ? 0.88 : 0.82),
    crosshair: theme.crosshair ?? veil(dark ? 0.5 : 0.52),
    dateTabBg: theme.dateTabBg ?? (theme.raised ?? theme.surface),
    dateTabText: theme.dateTabText ?? theme.text,
    hoverTabBg: theme.hoverTabBg ?? theme.text,
    hoverTabText: theme.hoverTabText ?? theme.surface,
    textSecondary,
    autoName: mix(theme.text, theme.surface, 0.25),
    quiet: mix(theme.muted, theme.surface, 0.32),
    tileTop: mix(theme.raised ?? theme.surface, theme.text, 0.05),
    tileBottom: mix(theme.raised ?? theme.surface, theme.surface, 0.3),
    tileEdge: veil(dark ? 0.06 : 0.08),
    tileFlat: veil(dark ? 0.07 : 0.06),
    tileInk: mix(theme.text, theme.surface, 0.28),
    treeLine: veil(dark ? 0.14 : 0.18),
    barNeutral: mix(theme.text, theme.muted, 0.38),
    trackGrey: mix(theme.text, theme.muted, 0.5),
    twoWayNeutral: rgbaColor(theme.text, dark ? 0.28 : 0.25),
    markerTick: rgbaColor(theme.text, 0.42),
    partsTrack: rgbaColor(theme.text, dark ? 0.045 : 0.05),
    chipFill: veil(dark ? 0.09 : 0.08),
    netEdge: veil(dark ? 0.14 : 0.16),
    netBar: veil(dark ? 0.76 : 0.36),
    playFill: mix(theme.text, theme.muted, 0.455),
    pauseFill: rgbaColor(theme.text, dark ? 0.16 : 0.14),
    darkFillInk: rgbaColor(dark ? theme.surface : theme.text, 0.88),
    lightFillInk: rgbaColor(dark ? ink : extendColor(theme.text, theme.surface), 0.96),
  };
}

type KitTokens = ReturnType<typeof buildKitTokens>;

type ThemeSnapshot = readonly unknown[];

interface CachedTokens {
  readonly snapshot: ThemeSnapshot;
  readonly tokens: KitTokens;
}

const kitTokenCache = new WeakMap<object, CachedTokens>();
let activeTheme: ThemeSnapshot | null = null;
let activeThemeRef: KitTokenInput | null = null;
let activeThemeTokens: KitTokens | null = null;

function themeSnapshot(theme: KitTokenInput): ThemeSnapshot {
  return [
    theme.surface,
    theme.text,
    theme.muted,
    theme.accent,
    theme.red,
    theme.green,
    theme.raised,
    theme.dark,
    theme.font,
    theme.fontSize?.small,
    theme.fontSize?.body,
    theme.hairline,
    theme.hairlineStrong,
    theme.bandDivider,
    theme.headerBand,
    theme.rowHover,
    theme.rowSelected,
    theme.childTint,
    theme.shortage,
    theme.barMuted,
    theme.barTrack,
    theme.gridLine,
    theme.zeroLine,
    theme.playhead,
    theme.crosshair,
    theme.dateTabBg,
    theme.dateTabText,
    theme.hoverTabBg,
    theme.hoverTabText,
  ];
}

function matchesSnapshot(s: ThemeSnapshot, theme: KitTokenInput): boolean {
  return Object.is(s[0], theme.surface)
    && Object.is(s[1], theme.text)
    && Object.is(s[2], theme.muted)
    && Object.is(s[3], theme.accent)
    && Object.is(s[4], theme.red)
    && Object.is(s[5], theme.green)
    && Object.is(s[6], theme.raised)
    && Object.is(s[7], theme.dark)
    && Object.is(s[8], theme.font)
    && Object.is(s[9], theme.fontSize?.small)
    && Object.is(s[10], theme.fontSize?.body)
    && Object.is(s[11], theme.hairline)
    && Object.is(s[12], theme.hairlineStrong)
    && Object.is(s[13], theme.bandDivider)
    && Object.is(s[14], theme.headerBand)
    && Object.is(s[15], theme.rowHover)
    && Object.is(s[16], theme.rowSelected)
    && Object.is(s[17], theme.childTint)
    && Object.is(s[18], theme.shortage)
    && Object.is(s[19], theme.barMuted)
    && Object.is(s[20], theme.barTrack)
    && Object.is(s[21], theme.gridLine)
    && Object.is(s[22], theme.zeroLine)
    && Object.is(s[23], theme.playhead)
    && Object.is(s[24], theme.crosshair)
    && Object.is(s[25], theme.dateTabBg)
    && Object.is(s[26], theme.dateTabText)
    && Object.is(s[27], theme.hoverTabBg)
    && Object.is(s[28], theme.hoverTabText);
}

function sameSnapshot(a: ThemeSnapshot, b: ThemeSnapshot): boolean {
  for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i])) return false;
  return true;
}

export function resolveKitTokens(theme: KitTokenInput): KitTokens {
  if (activeTheme !== null && matchesSnapshot(activeTheme, theme)) {
    if (theme === activeThemeRef && activeThemeTokens !== null) return activeThemeTokens;
    const cached = kitTokenCache.get(theme);
    if (cached && matchesSnapshot(cached.snapshot, theme)) {
      activeThemeRef = theme;
      activeThemeTokens = cached.tokens;
      return cached.tokens;
    }
  }
  const snapshot = themeSnapshot(theme);
  if (activeTheme !== null && !sameSnapshot(activeTheme, snapshot)) {
    colorCache.clear();
    paletteCache.clear();
    clearTextWidthCache();
  }
  activeTheme = snapshot;
  const cached = kitTokenCache.get(theme);
  if (cached && sameSnapshot(cached.snapshot, snapshot)) {
    activeThemeRef = theme;
    activeThemeTokens = cached.tokens;
    return cached.tokens;
  }
  const tokens = buildKitTokens(theme);
  kitTokenCache.set(theme, { snapshot, tokens });
  activeThemeRef = theme;
  activeThemeTokens = tokens;
  return tokens;
}

export function drawRowHover(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
  inset = 4,
): void {
  const w = width - inset * 2;
  const h = height - 2;
  if (w <= 0 || h <= 0) return;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.fillStyle = tokens.rowHover;
  ctx.beginPath();
  ctx.roundRect(x + inset, y + 1, w, h, ROW_CORNER_RADIUS);
  ctx.fill();
  ctx.restore();
}

export function drawRowSelected(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
  inset = 4,
): void {
  const w = width - inset * 2;
  const h = height - 2;
  if (w <= 0 || h <= 0) return;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.fillStyle = tokens.rowSelected;
  ctx.beginPath();
  ctx.roundRect(x + inset, y + 1, w, h, ROW_CORNER_RADIUS);
  ctx.fill();
  ctx.restore();
}

export function drawRowFocusRing(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
  inset = 4,
): void {
  const w = width - inset * 2 - 2;
  const h = height - 4;
  if (w <= 0 || h <= 0) return;
  ctx.save();
  ctx.strokeStyle = theme.accent;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.roundRect(x + inset + 1, y + 2, w, h, ROW_CORNER_RADIUS);
  ctx.stroke();
  ctx.restore();
}

export function drawRowHairline(
  ctx: Canvas2D,
  x0: number,
  x1: number,
  y: number,
  theme: FamilyTheme,
  kind: 'normal' | 'strong' | 'band' = 'normal',
): void {
  if (x1 <= x0) return;
  const tokens = resolveKitTokens(theme);
  const stroke = kind === 'band' ? tokens.bandDivider : kind === 'strong' ? tokens.hairlineStrong : tokens.hairline;
  const py = Math.floor(y) + 0.5;
  ctx.save();
  ctx.strokeStyle = stroke;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(x0, py);
  ctx.lineTo(x1, py);
  ctx.stroke();
  ctx.restore();
}

export function drawChildRowTint(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
): void {
  if (width <= 0 || height <= 0) return;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.fillStyle = tokens.childTint;
  ctx.fillRect(x, y, width, height);
  ctx.restore();
}

export function drawScrollFade(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
  edge: 'top' | 'bottom' | 'left' | 'right',
): void {
  if (width <= 0 || height <= 0) return;
  const x0 = edge === 'right' ? x + width : x;
  const y0 = edge === 'bottom' ? y + height : y;
  const x1 = edge === 'left' ? x + width : edge === 'right' ? x : x;
  const y1 = edge === 'top' ? y + height : edge === 'bottom' ? y : y;
  ctx.save();
  const gradient = ctx.createLinearGradient(x0, y0, x1, y1);
  gradient.addColorStop(0, theme.surface);
  gradient.addColorStop(1, rgbaColor(theme.surface, 0));
  ctx.fillStyle = gradient;
  ctx.fillRect(x, y, width, height);
  ctx.restore();
}

export function artInitials(resolvedName: string | undefined): string {
  if (!resolvedName) return '';
  const trimmed = resolvedName.trim();
  if (!trimmed) return '';
  const cached = initialsCache.get(trimmed);
  if (cached !== undefined) return cached;
  const words = trimmed.split(/[\s\-_/·]+/).filter(Boolean);
  const letters: string[] = [];
  for (const word of words) {
    const first = graphemes(word).find(g => /[\p{L}\p{N}]/u.test(g));
    if (first) letters.push(first.toLocaleUpperCase());
    if (letters.length === 3) break;
  }
  if (!letters.length) {
    for (const g of graphemes(trimmed)) {
      if (/[\p{L}\p{N}]/u.test(g)) {
        letters.push(g.toLocaleUpperCase());
        if (letters.length === 2) break;
      }
    }
  }
  const result = letters.join('');
  if (initialsCache.size >= INITIALS_CACHE_MAX) initialsCache.clear();
  initialsCache.set(trimmed, result);
  return result;
}

export type ArtSlotState =
  | { readonly kind: 'none' }
  | { readonly kind: 'pending'; readonly width?: number; readonly height?: number }
  | { readonly kind: 'ready'; readonly bitmap: CanvasImageSource; readonly width: number; readonly height: number }
  | { readonly kind: 'missing'; readonly reason: string; readonly label?: string }
  | { readonly kind: 'unrecorded'; readonly reason: string }
  | { readonly kind: 'refused'; readonly reason: string }
  | { readonly kind: 'failed'; readonly reason: string; readonly retryable: boolean };

export interface ArtSlotResult {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly drawn: ArtSlotState['kind'];
  readonly label?: string;
}

interface DrawnBounds { readonly x: number; readonly y: number; readonly width: number; readonly height: number }
const drawnBounds = new WeakMap<object, DrawnBounds | null>();
const OPAQUE = 24;

export function artDrawnBounds(source: CanvasImageSource, width: number, height: number): DrawnBounds | null {
  if (typeof source !== 'object' || source === null) return null;
  if (drawnBounds.has(source)) return drawnBounds.get(source)!;
  let bounds: DrawnBounds | null = null;
  try {
    const canvas = new OffscreenCanvas(width, height);
    const g = canvas.getContext('2d', { willReadFrequently: true });
    if (g) {
      g.drawImage(source, 0, 0);
      const data = g.getImageData(0, 0, width, height).data;
      let x0 = width, y0 = height, x1 = -1, y1 = -1;
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          if (data[(y * width + x) * 4 + 3]! <= OPAQUE) continue;
          if (x < x0) x0 = x;
          if (x > x1) x1 = x;
          if (y < y0) y0 = y;
          if (y > y1) y1 = y;
        }
      }
      bounds = x1 < 0 ? null : { x: x0, y: y0, width: x1 - x0 + 1, height: y1 - y0 + 1 };
    }
  } catch {
    bounds = null;
  }
  drawnBounds.set(source, bounds);
  return bounds;
}

function drawSlotImage(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  state: Extract<ArtSlotState, { kind: 'ready' }>,
  radius: number,
  trim = false,
): void {
  const box = trim ? artDrawnBounds(state.bitmap, Math.max(1, state.width), Math.max(1, state.height)) : null;
  const sx = box?.x ?? 0;
  const sy = box?.y ?? 0;
  const sw = box?.width ?? Math.max(1, state.width);
  const sh = box?.height ?? Math.max(1, state.height);
  const scale = Math.min(width / sw, height / sh);
  const dw = sw * scale;
  const dh = sh * scale;
  ctx.save();
  if (radius > 0) {
    ctx.beginPath();
    ctx.roundRect(x, y, width, height, radius);
    ctx.clip();
  }
  ctx.drawImage(state.bitmap, sx, sy, sw, sh, x + (width - dw) / 2, y + (height - dh) / 2, dw, dh);
  ctx.restore();
}

function drawSlotMark(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  state: Extract<ArtSlotState, { kind: 'missing' | 'unrecorded' | 'refused' | 'failed' }>,
  label: string,
  labelSize: number,
  radius: number,
  theme: FamilyTheme,
): void {
  if (state.kind === 'missing' || state.kind === 'unrecorded') {
    drawHatchRect(ctx, x, y, width, height, radius, theme);
    if (state.kind === 'missing' && label) {
      ctx.font = `${VALUE_WEIGHT} ${labelSize}px ${theme.font}`;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(ellipsise(label, Math.max(1, width - 2), s => textWidth(ctx, s)), x + width / 2, y + height / 2);
    }
    return;
  }
  const tokens = resolveKitTokens(theme);
  ctx.fillStyle = tokens.headerBand;
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, radius);
  ctx.fill();
  ctx.strokeStyle = theme.line;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.roundRect(x + 0.5, y + 0.5, Math.max(0, width - 1), Math.max(0, height - 1), radius);
  ctx.stroke();
  ctx.strokeStyle = theme.muted;
  ctx.fillStyle = theme.muted;
  if (state.kind === 'refused') {
    const inset = Math.max(2, Math.min(width, height) * 0.25);
    ctx.lineWidth = 1.25;
    ctx.beginPath();
    ctx.moveTo(x + inset, y + height - inset);
    ctx.lineTo(x + width - inset, y + inset);
    ctx.stroke();
    return;
  }
  ctx.beginPath();
  ctx.arc(x + width / 2, y + height / 2, Math.max(1.5, Math.min(2.5, Math.min(width, height) * 0.12)), 0, Math.PI * 2);
  ctx.fill();
}

export function drawArtSlot(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  state: ArtSlotState,
  theme: FamilyTheme,
  options?: { readonly radius?: number; readonly alpha?: number; readonly trim?: boolean },
): ArtSlotResult {
  const radius = options?.radius ?? 3;
  if (width <= 0 || height <= 0 || state.kind === 'none' || state.kind === 'pending') {
    return { x, y, width, height, drawn: state.kind };
  }
  ctx.save();
  if (options?.alpha !== undefined) ctx.globalAlpha = options.alpha;
  if (state.kind === 'ready') {
    drawSlotImage(ctx, x, y, width, height, state, radius, options?.trim);
    ctx.restore();
    return { x, y, width, height, drawn: 'ready' };
  }
  const edge = Math.min(width, height);
  const label = state.kind === 'missing' && edge >= 14 ? (artInitials(state.label) || '?') : '';
  drawSlotMark(ctx, x, y, width, height, state, label, Math.max(7, Math.min(13, Math.round(edge * 0.34))), radius, theme);
  ctx.restore();
  return { x, y, width, height, drawn: state.kind, ...(label ? { label } : {}) };
}

export function drawFlagSlot(
  ctx: Canvas2D,
  x: number,
  y: number,
  state: ArtSlotState,
  theme: FamilyTheme,
  options?: { readonly width?: number; readonly height?: number; readonly dimmed?: boolean; readonly radius?: number },
): ArtSlotResult {
  const width = options?.width ?? FLAG_SLOT_WIDTH;
  const height = options?.height ?? FLAG_SLOT_HEIGHT;
  const radius = options?.radius ?? 2;
  if (width <= 0 || height <= 0 || state.kind === 'pending' || state.kind === 'none') {
    return { x, y, width, height, drawn: state.kind };
  }
  const alpha = options?.dimmed ? FLAG_LOSER_ALPHA : 1;
  ctx.save();
  ctx.globalAlpha = alpha;
  if (state.kind === 'ready') {
    drawSlotImage(ctx, x, y, width, height, state, radius);
    ctx.globalAlpha = alpha * 0.4;
    ctx.strokeStyle = theme.line;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(x + 0.5, y + 0.5, Math.max(0, width - 1), Math.max(0, height - 1), radius);
    ctx.stroke();
    ctx.restore();
    return { x, y, width, height, drawn: 'ready' };
  }
  const label = state.kind === 'missing' && width >= 16 && height >= 11 ? (state.label?.trim().toUpperCase().slice(0, 3) || '?') : '';
  drawSlotMark(ctx, x, y, width, height, state, label, Math.max(7, Math.min(9, height - 4)), radius, theme);
  ctx.restore();
  return { x, y, width, height, drawn: state.kind, ...(label ? { label } : {}) };
}

export function drawNameSlot(
  ctx: Canvas2D,
  text: string,
  x: number,
  y: number,
  width: number,
  rowHeight: number,
  theme: FamilyTheme,
  options?: {
    readonly fontSize?: number;
    readonly weight?: number;
    readonly color?: string;
    readonly align?: CanvasTextAlign;
    readonly maxLines?: 1 | 2;
    readonly lineHeight?: number;
  },
): FittedLabel {
  const fontSize = options?.fontSize ?? theme.fontSize.body;
  const weight = options?.weight ?? LABEL_WEIGHT;
  const lineHeight = options?.lineHeight ?? labelLineHeight(fontSize);
  const maxLines: 1 | 2 = options?.maxLines ?? (labelFitsTwoLines(rowHeight, lineHeight) ? 2 : 1);
  ctx.save();
  ctx.font = `${weight} ${fontSize}px ${theme.font}`;
  ctx.fillStyle = options?.color ?? theme.text;
  ctx.textAlign = options?.align ?? 'left';
  ctx.textBaseline = 'middle';
  const fitted = drawFittedLabel(ctx, text, x, y + rowHeight / 2, width, {
    fontSize,
    lineHeight,
    maxLines,
  });
  ctx.restore();
  return fitted;
}

export function barCornerRadius(height: number): number {
  return Math.max(0, Math.min(4, height / 3));
}

export function barEndPath(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  direction: 'right' | 'left' = 'right',
  radius = barCornerRadius(height),
): void {
  const r = Math.min(radius, Math.max(0, width), Math.max(0, height / 2));
  const radii: [number, number, number, number] = direction === 'right' ? [0, r, r, 0] : [r, 0, 0, r];
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, radii);
}

export function drawBar(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  fill: string,
  direction: 'right' | 'left' = 'right',
  alpha = 1,
): void {
  if (width <= 0 || height <= 0) return;
  ctx.save();
  ctx.fillStyle = fill;
  ctx.globalAlpha = alpha;
  barEndPath(ctx, x, y, width, height, direction);
  ctx.fill();
  ctx.restore();
}

export function drawBarBehindFigure(
  ctx: Canvas2D,
  figureRight: number,
  middleY: number,
  fraction: number,
  fill: string,
  options: {
    readonly density?: RowDensity;
    readonly sorted?: boolean;
    readonly maxWidth?: number;
    readonly theme: FamilyTheme;
  },
): { readonly x: number; readonly y: number; readonly width: number; readonly height: number } | null {
  const clamped = Math.max(0, Math.min(1, fraction));
  if (clamped <= 0) return null;
  const density = options.density ?? 'normal';
  const height = BEHIND_BAR_HEIGHT[density];
  const maxWidth = options.maxWidth ?? BEHIND_BAR_MAX_WIDTH;
  const width = Math.min(maxWidth, Math.max(BEHIND_BAR_MIN_WIDTH, Math.round(maxWidth * clamped)));
  const startRight = figureRight + BEHIND_BAR_OFFSET;
  const x = startRight - width;
  const y = Math.round(middleY - height / 2);
  const sorted = options.sorted ?? true;
  if (sorted) {
    drawBar(ctx, x, y, width, height, fill, 'left', 0.3);
  } else {
    drawBar(ctx, x, y, width, height, resolveKitTokens(options.theme).barMuted, 'left', 1);
  }
  return { x, y, width, height };
}

export function drawTwoWayBar(
  ctx: Canvas2D,
  centerX: number,
  middleY: number,
  halfWidth: number,
  height: number,
  signedFraction: number,
  positiveFill: string,
  negativeFill: string,
  theme: FamilyTheme,
): void {
  if (halfWidth <= 0 || height <= 0) return;
  const tokens = resolveKitTokens(theme);
  const y = Math.round(middleY - height / 2);
  const clamped = Math.max(-1, Math.min(1, signedFraction));
  const px = Math.floor(centerX) + 0.5;
  ctx.save();
  ctx.strokeStyle = tokens.zeroLine;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(px, y - 2);
  ctx.lineTo(px, y + height + 2);
  ctx.stroke();
  ctx.restore();
  if (clamped > 0) {
    drawBar(ctx, centerX, y, halfWidth * clamped, height, positiveFill, 'right');
  } else if (clamped < 0) {
    const w = halfWidth * Math.abs(clamped);
    drawBar(ctx, centerX - w, y, w, height, negativeFill, 'left');
  }
}

export interface SplitBarSegment {
  readonly fraction: number;
  readonly fill: string;
  readonly hatched?: boolean;
}

export function splitBarWidths(x: number, width: number, fractions: readonly number[]): readonly number[] {
  let cursor = x;
  return fractions.map(fraction => {
    const segmentWidth = Math.min(x + width - cursor, width * fraction);
    if (segmentWidth > 0) cursor += segmentWidth;
    return Math.max(0, segmentWidth);
  });
}

export function drawSplitBar(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  segments: readonly SplitBarSegment[],
  theme: FamilyTheme,
  options?: { readonly gap?: number; readonly widths?: readonly number[] },
): void {
  if (width <= 0 || height <= 0) return;
  const active = segments.filter(s => s.fraction > 0);
  if (!active.length) return;
  const gap = options?.gap ?? 0;
  if (gap > 0) {
    const span = Math.max(0, width - gap * (active.length - 1));
    ctx.save();
    ctx.beginPath();
    ctx.roundRect(x, y, width, height, Math.min(2, height / 2));
    ctx.clip();
    let at = x;
    for (const segment of active) {
      const segW = span * segment.fraction;
      if (segment.hatched) drawHatchRect(ctx, at, y, segW, height, 0, theme);
      else {
        ctx.fillStyle = segment.fill;
        ctx.fillRect(at, y, segW, height);
      }
      at += segW + gap;
    }
    ctx.restore();
    return;
  }
  const radius = barCornerRadius(height);
  const widths = options?.widths ?? splitBarWidths(x, width, active.map(segment => segment.fraction));
  let cursor = x;
  for (let i = 0; i < active.length; i++) {
    const segment = active[i]!;
    const segW = widths[i]!;
    if (segW <= 0) continue;
    const isLast = i === active.length - 1;
    if (segment.hatched) {
      drawHatchRect(ctx, cursor, y, segW, height, isLast ? radius : 0, theme);
    } else if (isLast) {
      drawBar(ctx, cursor, y, segW, height, segment.fill, 'right');
    } else {
      ctx.save();
      ctx.fillStyle = segment.fill;
      ctx.fillRect(cursor, y, segW, height);
      ctx.restore();
    }
    cursor += segW;
  }
}

export interface ShareBarSegment {
  readonly share: number;
  readonly color: string;
  readonly hatched?: boolean;
  readonly label?: string;
  readonly shortLabel?: string;
  readonly labelAlign?: 'left' | 'right';
}

export interface ShareBarBounds {
  readonly x: number;
  readonly width: number;
}

export interface ShareBarLabelDecision {
  readonly kind: 'full' | 'short' | 'none';
  readonly align: 'left' | 'right';
}

export interface ShareBarGeometry {
  readonly segments: readonly ShareBarBounds[];
  readonly remainder: ShareBarBounds;
  readonly labels?: readonly ShareBarLabelDecision[];
}

export function shareBarGeometry(x: number, width: number, shares: readonly number[]): ShareBarGeometry {
  const active = shares.filter(share => share > 0);
  const gap = active.length > 1 ? 1 : 0;
  const usable = Math.max(0, width - gap * Math.max(0, active.length - 1));
  const complete = active.reduce((total, share) => total + share, 0) >= 0.999;
  const right = x + width;
  let cursor = x;
  const segments = active.map((share, i) => {
    const width = complete && i === active.length - 1 ? Math.max(0, right - cursor) : usable * share;
    const bounds = { x: cursor, width };
    if (width > 0) cursor += width + gap;
    return bounds;
  });
  const remainderX = complete ? right : Math.min(right, cursor);
  return { segments, remainder: { x: remainderX, width: right - remainderX } };
}

export function shareBarLabelDecisions(
  segments: readonly ShareBarSegment[],
  bounds: readonly ShareBarBounds[],
  measure: (text: string) => number,
): readonly ShareBarLabelDecision[] {
  return segments.map((segment, i) => {
    const width = bounds[i]?.width ?? 0;
    const align = segment.labelAlign === 'right' ? 'right' : 'left';
    if (segment.hatched || !segment.label || width < 34) return { kind: 'none', align };
    const room = width - 12;
    if (measure(segment.label) <= room) return { kind: 'full', align };
    if (segment.shortLabel && measure(segment.shortLabel) <= room) return { kind: 'short', align };
    return { kind: 'none', align };
  });
}

export function drawShareBar(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  segments: readonly ShareBarSegment[],
  theme: FamilyTheme,
  radius = 2.5,
  geometry?: ShareBarGeometry,
): void {
  if (width <= 0 || height <= 0) return;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.fillStyle = tokens.barTrack;
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, radius);
  ctx.fill();
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, radius);
  ctx.clip();

  const active = segments.filter(seg => seg.share > 0);
  const bounds = geometry ?? shareBarGeometry(x, width, active.map(seg => seg.share));
  const labels = bounds.labels ?? shareBarLabelDecisions(active, bounds.segments, text => textWidth(ctx, text));
  for (let i = 0; i < active.length; i++) {
    const seg = active[i]!;
    const { x: cursor, width: segW } = bounds.segments[i]!;
    if (segW <= 0) continue;
    if (seg.hatched) {
      drawHatchRect(ctx, cursor, y, segW, height, 0, theme);
    } else {
      ctx.fillStyle = readableColour(seg.color, theme.surface);
      ctx.fillRect(cursor, y, segW, height);
    }
    const label = labels[i]!;
    if (label.kind !== 'none') {
      ctx.font = `600 11px ${theme.font}`;
      ctx.fillStyle = contrastInkForFill(readableColour(seg.color, theme.surface), theme);
      const right = label.align === 'right';
      ctx.textAlign = right ? 'right' : 'left';
      ctx.textBaseline = 'middle';
      const pad = 6;
      const shown = label.kind === 'full' ? seg.label : seg.shortLabel;
      if (shown) ctx.fillText(shown, right ? cursor + segW - pad : cursor + pad, y + height / 2);
    }
  }
  if (bounds.remainder.width > 0) drawHatchRect(ctx, bounds.remainder.x, y, bounds.remainder.width, height, 0, theme);
  ctx.restore();
}

export function drawTrackBar(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  width: number,
  height: number,
  fraction: number,
  fill: string,
  theme: FamilyTheme,
): void {
  if (width <= 0 || height <= 0) return;
  const tokens = resolveKitTokens(theme);
  const y = Math.round(middleY - height / 2);
  const clamped = Math.max(0, Math.min(1, fraction));
  drawBar(ctx, x, y, width, height, tokens.barTrack, 'right');
  if (clamped > 0) {
    drawBar(ctx, x, y, width * clamped, height, fill, 'right');
  }
}

export interface ThresholdTick {
  readonly at: number;
  readonly reached: boolean;
}

export function drawThresholdMeter(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  width: number,
  height: number,
  fraction: number | null,
  ticks: readonly ThresholdTick[],
  fill: string,
  theme: FamilyTheme,
): void {
  if (width <= 0 || height <= 0) return;
  const y = Math.round(middleY - height / 2);
  if (fraction === null) {
    drawHatchRect(ctx, x, y, width, height, barCornerRadius(height), theme);
    return;
  }
  drawTrackBar(ctx, x, middleY, width, height, fraction, fill, theme);
  if (!ticks.length) return;
  ctx.save();
  ctx.lineWidth = 1.2;
  for (let i = 0; i < ticks.length; i++) {
    const tick = ticks[i]!;
    if (tick.at <= 0 || tick.at >= 1) continue;
    const tx = Math.round(x + width * tick.at) + 0.5;
    ctx.strokeStyle = theme.muted;
    ctx.globalAlpha = tick.reached ? 0.9 : 0.55;
    ctx.beginPath();
    ctx.moveTo(tx, y - 3);
    ctx.lineTo(tx, y + height + 3);
    ctx.stroke();
  }
  ctx.restore();
}

export function drawProgressRing(
  ctx: Canvas2D,
  cx: number,
  cy: number,
  radius: number,
  fraction: number,
  stroke: string,
  theme: FamilyTheme,
  lineWidth = 2.4,
): void {
  if (radius <= 0) return;
  const tokens = resolveKitTokens(theme);
  const clamped = Math.max(0, Math.min(1, fraction));
  ctx.save();
  ctx.lineWidth = lineWidth;
  ctx.strokeStyle = tokens.barTrack;
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.stroke();
  if (clamped > 0) {
    const start = -Math.PI / 2;
    const end = start + clamped * Math.PI * 2;
    ctx.strokeStyle = stroke;
    ctx.lineCap = 'round';
    ctx.beginPath();
    ctx.arc(cx, cy, radius, start, end);
    ctx.stroke();
  }
  ctx.restore();
}

export type StatusMarkKind = 'running' | 'waiting' | 'succeeded' | 'finished' | 'ended';
export const STATUS_WORD_SIZE = 12.5;
export const STATUS_MARK_LEAD = 15;

export const STATUS_MARK_LABELS: Readonly<Record<StatusMarkKind, string>> = {
  running: 'Running',
  waiting: 'Waiting',
  succeeded: 'Succeeded',
  finished: 'Finished',
  ended: 'Ended',
};

export function drawStatusMark(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  kind: StatusMarkKind,
  theme: FamilyTheme,
  options?: { readonly label?: string; readonly maxWidth?: number },
): { readonly width: number; readonly label: FittedLabel } {
  const tokens = resolveKitTokens(theme);
  const dotRadius = 3.5;
  const dotDiameter = dotRadius * 2;
  const gap = STATUS_MARK_LEAD - dotDiameter;
  const color = kind === 'running' || kind === 'waiting'
    ? theme.accent
    : kind === 'succeeded'
      ? theme.green
      : kind === 'ended'
        ? tokens.quiet
        : theme.muted;
  const ring = kind === 'waiting' || kind === 'ended';
  const stroke = kind === 'waiting' ? 1.5 : 1.4;
  ctx.save();
  ctx.beginPath();
  ctx.arc(x + dotRadius, middleY, ring ? dotRadius - stroke / 2 : dotRadius, 0, Math.PI * 2);
  if (ring) {
    ctx.strokeStyle = color;
    ctx.lineWidth = stroke;
    ctx.stroke();
  } else {
    ctx.fillStyle = color;
    ctx.fill();
  }
  const word = options?.label ?? STATUS_MARK_LABELS[kind];
  const fontSize = STATUS_WORD_SIZE;
  ctx.font = `400 ${fontSize}px ${theme.font}`;
  ctx.fillStyle = kind === 'ended' ? theme.muted : tokens.textSecondary;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  const textMax = options?.maxWidth !== undefined ? Math.max(0, options.maxWidth - dotDiameter - gap) : 10_000;
  const fitted = drawFittedLabel(ctx, word, x + dotDiameter + gap, middleY, textMax, {
    fontSize,
    lineHeight: labelLineHeight(fontSize),
    maxLines: 1,
  });
  const drawnW = fitted.lines[0] ? textWidth(ctx, fitted.lines[0]) : 0;
  ctx.restore();
  return { width: dotDiameter + gap + drawnW, label: fitted };
}

export function drawLegendEntry(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  label: string,
  color: string,
  theme: FamilyTheme,
  options?: {
    readonly line?: boolean;
    readonly dash?: readonly number[];
    readonly hatch?: boolean;
    readonly outlined?: boolean;
    readonly swatch?: 'line' | 'square';
    readonly flag?: ArtSlotState;
    readonly muted?: boolean;
  },
): { readonly width: number; readonly label: FittedLabel } {
  const isLine = Boolean(options?.line || options?.swatch === 'line' || options?.dash?.length);
  const swatchW = isLine ? 14 : options?.hatch ? 10 : 9;
  const gap = 6;
  let cursor = x;
  ctx.save();
  if (options?.hatch) {
    drawHatchRect(ctx, cursor, Math.round(middleY - 5), swatchW, 10, 1.5, theme);
    cursor += swatchW + gap;
  } else if (options?.outlined) {
    ctx.strokeStyle = theme.muted;
    ctx.lineWidth = 1;
    ctx.setLineDash([2, 2]);
    ctx.beginPath();
    ctx.roundRect(cursor + 0.5, Math.round(middleY - 4.5) + 0.5, 8, 8, 2);
    ctx.stroke();
    ctx.setLineDash([]);
    cursor += swatchW + gap;
  } else if (isLine) {
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    if (options?.dash?.length) ctx.setLineDash([...options.dash]);
    ctx.beginPath();
    ctx.moveTo(cursor, middleY);
    ctx.lineTo(cursor + swatchW, middleY);
    ctx.stroke();
    if (options?.dash?.length) ctx.setLineDash([]);
    cursor += swatchW + gap;
  } else if (color) {
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.roundRect(cursor, Math.round(middleY - 4.5), swatchW, 9, 2);
    ctx.fill();
    cursor += swatchW + gap;
  }
  if (options?.flag && options.flag.kind !== 'none') {
    const fw = 16;
    const fh = 10;
    drawFlagSlot(ctx, cursor, Math.round(middleY - fh / 2), options.flag, theme, { width: fw, height: fh });
    cursor += fw + gap;
  }
  const fontSize = theme.fontSize.small;
  const font = `${LABEL_WEIGHT} ${fontSize}px ${theme.font}`;
  ctx.font = font;
  ctx.fillStyle = options?.muted ? theme.muted : theme.text;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  const usedW = cursor - x;
  const fitted = drawFittedLabel(ctx, label, cursor, middleY, 10_000, {
    font,
    fontSize,
    lineHeight: labelLineHeight(fontSize),
    maxLines: 1,
  });
  const drawnW = fitted.lines[0] ? measureCanvasText(ctx, fitted.lines[0], font) : 0;
  ctx.restore();
  return { width: usedW + drawnW, label: fitted };
}

export function drawDateColumn(
  ctx: Canvas2D,
  x: number,
  y: number,
  rowHeight: number,
  hour: number | null,
  theme: FamilyTheme,
  options?: { readonly width?: number; readonly align?: 'left' | 'right' },
): { readonly missing: boolean; readonly full: string; readonly hover: string } {
  const width = options?.width ?? DATE_COLUMN_WIDTH;
  const align = options?.align ?? 'left';
  const parts = hour === null ? null : formatDateParts(hour);
  const middleY = y + rowHeight / 2;
  if (!parts) {
    const boxX = align === 'right' ? x + Math.max(0, width - MISSING_BOX_SIZE) : x;
    drawHatchRect(ctx, boxX, Math.round(middleY - MISSING_BOX_SIZE / 2), MISSING_BOX_SIZE, MISSING_BOX_SIZE, 2, theme);
    return { missing: true, full: notRecordedLabel('Date'), hover: notRecordedLabel('Date') };
  }
  const anchorX = align === 'right' ? x + width : x;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.textAlign = align;
  ctx.textBaseline = 'middle';
  if (rowHeight >= 32) {
    ctx.font = `500 12.5px ${theme.font}`;
    ctx.fillStyle = tokens.textSecondary;
    ctx.fillText(ellipsise(parts.dayMonth, width, s => textWidth(ctx, s)), anchorX, middleY - 6);
    ctx.font = `400 10.5px ${theme.font}`;
    ctx.fillStyle = tokens.quiet;
    ctx.fillText(ellipsise(parts.year, width, s => textWidth(ctx, s)), anchorX, middleY + 7);
  } else {
    ctx.font = `500 11.5px ${theme.font}`;
    ctx.fillStyle = tokens.textSecondary;
    ctx.fillText(ellipsise(parts.dayMonth, width, s => textWidth(ctx, s)), anchorX, middleY);
  }
  ctx.restore();
  return { missing: false, full: parts.full, hover: parts.hover };
}

export function missingMarkWidth(
  maxWidth: number,
  measure: (text: string, font: string) => number,
  theme: Pick<FamilyTheme, 'font' | 'fontSize'>,
  options?: { readonly figure?: string; readonly gameWindow?: boolean; readonly notApplicable?: boolean },
): number {
  if (options?.notApplicable) return measure(NOT_APPLICABLE_LABEL, `${VALUE_WEIGHT} ${theme.fontSize.body}px ${theme.font}`);
  const smallFont = `${LABEL_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
  if (options?.gameWindow) return measure(ellipsise(GAME_WINDOW_MISSING_LABEL, maxWidth, s => measure(s, smallFont)), smallFont);
  const fullW = MISSING_BOX_SIZE + MISSING_BOX_GAP + measure(notRecordedLabel(options?.figure), smallFont);
  return maxWidth >= fullW ? fullW : MISSING_BOX_SIZE;
}

export function drawMissingMark(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  maxWidth: number,
  theme: FamilyTheme,
  options?: { readonly figure?: string; readonly gameWindow?: boolean; readonly align?: 'left' | 'right'; readonly notApplicable?: boolean },
): { readonly width: number; readonly squareOnly: boolean; readonly label: string } {
  const align = options?.align ?? 'left';
  const smallFont = `${LABEL_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
  if (options?.notApplicable) {
    ctx.save();
    ctx.font = `${VALUE_WEIGHT} ${theme.fontSize.body}px ${theme.font}`;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = align;
    ctx.textBaseline = 'middle';
    ctx.fillText(NOT_APPLICABLE_LABEL, align === 'right' ? x + maxWidth : x, middleY);
    const w = textWidth(ctx, NOT_APPLICABLE_LABEL);
    ctx.restore();
    return { width: w, squareOnly: false, label: NOT_APPLICABLE_LABEL };
  }
  if (options?.gameWindow) {
    ctx.save();
    ctx.font = smallFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = align;
    ctx.textBaseline = 'middle';
    const shown = ellipsise(GAME_WINDOW_MISSING_LABEL, maxWidth, s => textWidth(ctx, s));
    ctx.fillText(shown, align === 'right' ? x + maxWidth : x, middleY);
    const w = textWidth(ctx, shown);
    ctx.restore();
    return { width: w, squareOnly: false, label: GAME_WINDOW_MISSING_LABEL };
  }
  const label = notRecordedLabel(options?.figure);
  ctx.save();
  ctx.font = smallFont;
  const labelW = textWidth(ctx, label);
  const fullW = MISSING_BOX_SIZE + MISSING_BOX_GAP + labelW;
  const showWords = maxWidth >= fullW;
  const totalW = showWords ? fullW : MISSING_BOX_SIZE;
  const boxX = align === 'right' ? Math.max(x, x + maxWidth - totalW) : x;
  const boxY = Math.round(middleY - MISSING_BOX_SIZE / 2);
  drawHatchRect(ctx, boxX, boxY, MISSING_BOX_SIZE, MISSING_BOX_SIZE, 2, theme);
  if (showWords) {
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(label, boxX + MISSING_BOX_SIZE + MISSING_BOX_GAP, middleY);
  }
  ctx.restore();
  return { width: totalW, squareOnly: !showWords, label };
}

export function drawPartialTab(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  theme: FamilyTheme,
): { readonly width: number; readonly height: number } {
  drawHatchRect(ctx, x, Math.round(middleY - PARTIAL_TAB_HEIGHT / 2), PARTIAL_TAB_WIDTH, PARTIAL_TAB_HEIGHT, 1.5, theme);
  return { width: PARTIAL_TAB_WIDTH, height: PARTIAL_TAB_HEIGHT };
}

export function drawGapBand(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: FamilyTheme,
): void {
  if (width <= 0 || height <= 0) return;
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.fillStyle = tokens.headerBand;
  ctx.fillRect(x, y, width, height);
  drawHatchRect(ctx, x, y, width, height, 0, theme);
  ctx.restore();
}

export function drawSeriesMissingStrip(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  color: string,
  theme: FamilyTheme,
  height = SERIES_MISSING_STRIP_HEIGHT,
): void {
  if (width <= 0 || height <= 0) return;
  ctx.save();
  ctx.fillStyle = rgbaColor(color, 0.16);
  ctx.fillRect(x, y, width, height);
  drawHatchRect(ctx, x, y, width, height, 0, {
    hatch: color,
    hatchEdge: color,
    line: theme.line,
    hatchSpacing: Math.max(4, Math.round(theme.hatchSpacing * 0.75)),
  });
  ctx.restore();
}

export function drawPlayheadLine(
  ctx: Canvas2D,
  x: number,
  top: number,
  bottom: number,
  theme: FamilyTheme,
): void {
  if (bottom <= top) return;
  const tokens = resolveKitTokens(theme);
  const px = Math.floor(x) + 0.5;
  ctx.save();
  ctx.strokeStyle = tokens.playhead;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(px, top);
  ctx.lineTo(px, bottom);
  ctx.stroke();
  ctx.restore();
}

export function drawSelectedHourLine(
  ctx: Canvas2D,
  x: number,
  top: number,
  bottom: number,
  theme: FamilyTheme,
): void {
  if (bottom <= top) return;
  const tokens = resolveKitTokens(theme);
  const px = Math.floor(x) + 0.5;
  ctx.save();
  ctx.strokeStyle = tokens.selectedHour;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.moveTo(px, top);
  ctx.lineTo(px, bottom);
  ctx.stroke();
  ctx.fillStyle = theme.accent;
  ctx.beginPath();
  ctx.moveTo(px - 4, top);
  ctx.lineTo(px + 4, top);
  ctx.lineTo(px, top + 5);
  ctx.closePath();
  ctx.fill();
  ctx.restore();
}

export function drawCrosshairLine(
  ctx: Canvas2D,
  x: number,
  top: number,
  bottom: number,
  theme: FamilyTheme,
  dash?: readonly number[],
): void {
  if (bottom <= top) return;
  const tokens = resolveKitTokens(theme);
  const px = Math.floor(x) + 0.5;
  ctx.save();
  ctx.strokeStyle = tokens.crosshair;
  ctx.lineWidth = 1;
  if (dash?.length) ctx.setLineDash([...dash]);
  ctx.beginPath();
  ctx.moveTo(px, top);
  ctx.lineTo(px, bottom);
  ctx.stroke();
  if (dash?.length) ctx.setLineDash([]);
  ctx.restore();
}

export function drawDateTab(
  ctx: Canvas2D,
  text: string,
  cx: number,
  y: number,
  theme: FamilyTheme,
  variant: 'playhead' | 'hover' = 'playhead',
  bounds?: { readonly left: number; readonly right: number },
): { readonly x: number; readonly y: number; readonly width: number; readonly height: number } {
  const tokens = resolveKitTokens(theme);
  const padX = 7;
  const height = 19;
  ctx.save();
  const tabFont = `${VALUE_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
  ctx.font = tabFont;
  const textW = measureCanvasText(ctx, text, tabFont);
  const width = Math.ceil(textW + padX * 2);
  let x = Math.round(cx - width / 2);
  if (bounds) {
    x = Math.max(bounds.left, Math.min(x, bounds.right - width));
  }
  ctx.fillStyle = variant === 'playhead' ? tokens.dateTabBg : tokens.hoverTabBg;
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, 4);
  ctx.fill();
  if (variant === 'playhead') {
    ctx.strokeStyle = tokens.bandDivider;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(x + 0.5, y + 0.5, Math.max(0, width - 1), Math.max(0, height - 1), 4);
    ctx.stroke();
  }
  ctx.fillStyle = variant === 'playhead' ? tokens.dateTabText : tokens.hoverTabText;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(text, x + width / 2, y + height / 2);
  ctx.restore();
  return { x, y, width, height };
}

const MAX_INK_PER_THEME = 512;
const contrastInkCache = new WeakMap<object, { readonly surface: string; readonly text: string; readonly map: Map<string, string> }>();

export function contrastInkForFill(fill: string, theme: FamilyTheme): string {
  let bucket = contrastInkCache.get(theme);
  if (!bucket || bucket.surface !== theme.surface || bucket.text !== theme.text) {
    bucket = { surface: theme.surface, text: theme.text, map: new Map() };
    contrastInkCache.set(theme, bucket);
  }
  const cached = bucket.map.get(fill);
  if (cached !== undefined) return cached;
  const parsed = parseColorAlpha(fill);
  let resolved: string;
  if (!parsed) {
    resolved = theme.text;
  } else {
    const tokens = resolveKitTokens(theme);
    const surface = parsed.alpha < 1 ? parseColor(theme.surface) : null;
    const bg = surface && parsed.alpha < 1 ? mixRgb(surface, parsed.rgb, parsed.alpha) : parsed.rgb;
    const shown = (ink: string) => {
      const over = parseColorAlpha(ink)!;
      return contrast(bg, mixRgb(bg, over.rgb, over.alpha));
    };
    resolved = shown(tokens.darkFillInk) >= shown(tokens.lightFillInk) ? tokens.darkFillInk : tokens.lightFillInk;
  }
  if (bucket.map.size >= MAX_INK_PER_THEME) bucket.map.clear();
  bucket.map.set(fill, resolved);
  return resolved;
}

export function drawChevron(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  open: boolean,
  theme: FamilyTheme,
): void {
  const cx = Math.round(x + 8);
  const cy = Math.round(middleY);
  ctx.save();
  ctx.strokeStyle = theme.muted;
  ctx.lineWidth = 1.5;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.beginPath();
  if (open) {
    ctx.moveTo(cx - 3.5, cy - 2);
    ctx.lineTo(cx, cy + 2);
    ctx.lineTo(cx + 3.5, cy - 2);
  } else {
    ctx.moveTo(cx - 2, cy - 3.5);
    ctx.lineTo(cx + 2, cy);
    ctx.lineTo(cx - 2, cy + 3.5);
  }
  ctx.stroke();
  ctx.restore();
}

export const TIMELINE_NOTCH_WIDTH = 9;
export const TIMELINE_FADE_WIDTH = 12;
export const TIMELINE_BAR_RADIUS = 2.5;
export const TIMELINE_LABEL_MIN_WIDTH = 22;

export interface TimelineBarOptions {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly fill: string;
  readonly theme: FamilyTheme;
  readonly open?: boolean;
  readonly heldBefore?: boolean;
  readonly startBy?: boolean;
  readonly endUnseen?: boolean;
  readonly icon?: ArtSlotState;
  readonly label?: string;
}

export interface TimelineBarResult {
  readonly showedIcon: boolean;
  readonly label: FittedLine | null;
}

function fadeFill(ctx: Canvas2D, x: number, width: number, fill: string, fadeStart: boolean, fadeEnd: boolean): CanvasGradient | string {
  if ((!fadeStart && !fadeEnd) || width <= 2) return fill;
  const grad = ctx.createLinearGradient(x, 0, x + width, 0);
  const stop = Math.min(0.45, TIMELINE_FADE_WIDTH / width);
  const clear = rgbaColor(fill, 0);
  grad.addColorStop(0, fadeStart ? clear : fill);
  if (fadeStart) grad.addColorStop(stop, fill);
  if (fadeEnd) grad.addColorStop(1 - stop, fill);
  grad.addColorStop(1, fadeEnd ? clear : fill);
  return grad;
}

export function drawTimelineBar(ctx: Canvas2D, options: TimelineBarOptions): TimelineBarResult {
  const { x, y, width, height, theme } = options;
  if (width <= 0 || height <= 0) return { showedIcon: false, label: null };
  const fill = options.open ? options.fill : mix(options.fill, theme.surface, 0.5);
  const notch = options.heldBefore && width > TIMELINE_NOTCH_WIDTH + 4 ? TIMELINE_NOTCH_WIDTH : 0;
  const radius = Math.min(TIMELINE_BAR_RADIUS, width / 2, height / 2);
  const left = notch > 0 || options.startBy ? 0 : radius;
  const right = options.open || options.endUnseen ? 0 : radius;
  ctx.save();
  ctx.fillStyle = fadeFill(ctx, x, width, fill, !!options.startBy && notch === 0, !!options.endUnseen);
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, [left, right, right, left]);
  ctx.fill();
  if (notch > 0) {
    const cy = y + height / 2;
    const arm = Math.min(3.5, height * 0.22);
    ctx.strokeStyle = contrastInkForFill(fill, theme);
    ctx.lineWidth = 1.5;
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.beginPath();
    ctx.moveTo(x + 3 + arm * 0.8, cy - arm);
    ctx.lineTo(x + 3, cy);
    ctx.lineTo(x + 3 + arm * 0.8, cy + arm);
    ctx.stroke();
  }
  let showedIcon = false;
  let label: FittedLine | null = null;
  if (width >= TIMELINE_LABEL_MIN_WIDTH) {
    let cursor = x + (notch > 0 ? notch + 2 : 4);
    const limit = x + width - 4;
    if (options.icon && options.icon.kind !== 'none') {
      const size = options.icon.kind === 'missing' ? Math.min(14, height) : height;
      if (cursor + size <= limit) {
        drawArtSlot(ctx, cursor, y + (height - size) / 2, size, size, options.icon, theme, { radius: 2 });
        cursor += size + 6;
        showedIcon = true;
      }
    }
    if (options.label && limit - cursor >= 12) {
      ctx.font = `500 11.5px ${theme.font}`;
      const fitted = fitLine(options.label, limit - cursor, s => textWidth(ctx, s));
      if (fitted.text) {
        ctx.fillStyle = contrastInkForFill(fill, theme);
        ctx.textAlign = 'left';
        ctx.textBaseline = 'middle';
        ctx.fillText(fitted.text, cursor, y + height / 2);
        label = fitted;
      }
    }
  }
  ctx.restore();
  return { showedIcon, label };
}

export interface TimeAxisTick {
  readonly hour: number;
  readonly label: string;
  readonly major?: boolean;
}

const MIN_MARK_SPACING = 4;
const DAY_LABEL_SPACING = 64;
const YEAR_LABEL_SPACING = 48;

function shortMonth(month: number): string {
  return monthNames[month - 1]!.slice(0, 3);
}

function boundaryInView(boundary: number | null, startHour: number, endHour: number): number | null {
  if (boundary === null || boundary > endHour) return null;
  if (boundary >= startHour) return boundary;
  return Math.floor((boundary - HOUR_EPOCH) / 24) === Math.floor((startHour - HOUR_EPOCH) / 24) ? startHour : null;
}

function dayTicks(startHour: number, endHour: number, plotWidth: number, hoursPerDay: number, steps: readonly number[]): TimeAxisTick[] {
  const firstDay = Math.floor((startHour - HOUR_EPOCH) / hoursPerDay);
  const lastDay = Math.floor((endHour - HOUR_EPOCH) / hoursPerDay);
  const days = Math.max(1, (endHour - startHour) / hoursPerDay);
  const fit = Math.max(2, Math.floor(plotWidth / DAY_LABEL_SPACING));
  const labelStep = steps.find(candidate => Math.ceil(days / candidate) <= fit) ?? Math.max(steps.at(-1)!, Math.ceil(niceStep(days, fit)));
  const markStep = steps.find(candidate => candidate <= labelStep && labelStep % candidate === 0 && plotWidth / Math.ceil(days / candidate) >= MIN_MARK_SPACING) ?? labelStep;
  const ticks: TimeAxisTick[] = [];
  let shownYear: number | null = null;
  for (let day = Math.ceil(firstDay / markStep) * markStep; day <= lastDay && ticks.length < 512; day += markStep) {
    const hour = boundaryInView(HOUR_EPOCH + day * hoursPerDay, startHour, endHour);
    if (hour === null) continue;
    const date = gameDate(hour);
    const newYear = date !== null && date.month === 1 && date.day === 1;
    let label = '';
    if (day % labelStep === 0) {
      const dayMonth = date ? `${date.day} ${shortMonth(date.month)}` : formatDateOnly(hour);
      label = newYear ? String(date!.year) : date && shownYear !== date.year ? `${dayMonth} ${date.year}` : dayMonth;
      if (date !== null) shownYear = date.year;
    }
    ticks.push({ hour, label, major: newYear || (date !== null && date.day === 1) });
  }
  return ticks;
}

interface CachedTimeAxisTicks {
  readonly kind: 'month' | 'year';
  readonly validFromEnd: number;
  readonly validBeforeEnd: number;
  readonly ticks: readonly TimeAxisTick[];
}

const TIME_AXIS_CACHE_MAX = 64;
const TIME_AXIS_MAX_HOUR = 2_147_483_647;
const timeAxisTickCache = new Map<string, CachedTimeAxisTicks>();

function storeTimeAxisTicks(key: string, entry: CachedTimeAxisTicks): TimeAxisTick[] {
  if (timeAxisTickCache.size >= TIME_AXIS_CACHE_MAX) {
    const oldest = timeAxisTickCache.keys().next().value;
    if (oldest !== undefined) timeAxisTickCache.delete(oldest);
  }
  timeAxisTickCache.set(key, entry);
  return entry.ticks.slice();
}

export function computeTimeAxisTicks(startHour: number, endHour: number, plotWidth: number, dayHours = 24): TimeAxisTick[] {
  if (plotWidth <= 20 || endHour <= startHour) return [];
  const hoursPerDay = Math.max(1, dayHours);
  const useCache = timeChartCacheState.enabled;
  const cacheKey = useCache ? `${startHour}\u0000${hoursPerDay}\u0000${plotWidth}` : '';
  const span = endHour - startHour;
  if (useCache && Math.floor(endHour) <= TIME_AXIS_MAX_HOUR) {
    const cached = timeAxisTickCache.get(cacheKey);
    if (
      cached
      && endHour >= cached.validFromEnd
      && endHour < cached.validBeforeEnd
      && (cached.kind === 'month'
        ? span > 62 * hoursPerDay && span < 2 * 365 * hoursPerDay
        : span >= 2 * 365 * hoursPerDay)
    ) {
      return cached.ticks.slice();
    }
  }
  const start = gameDate(Math.floor(startHour));
  const end = gameDate(Math.floor(endHour));
  if (!start || !end) return dayTicks(startHour, endHour, plotWidth, hoursPerDay, [1, 2, 5, 10]);
  if (span <= 62 * hoursPerDay) return dayTicks(startHour, endHour, plotWidth, hoursPerDay, [1, 2, 3, 5, 7, 14]);
  const ticks: TimeAxisTick[] = [];
  if (span < 2 * 365 * hoursPerDay) {
    let shownYear: number | null = null;
    for (let year = start.year, month = start.month; year < end.year || (year === end.year && month <= end.month);) {
      const hour = boundaryInView(engineHour({ year, month, day: 1, hour: 1 }), startHour, endHour);
      if (hour !== null) {
        const major = month === 1;
        const quarter = month === 4 || month === 7 || month === 10;
        const label = major ? String(year) : quarter ? (shownYear === year ? shortMonth(month) : `${shortMonth(month)} ${year}`) : '';
        if (label) shownYear = year;
        ticks.push({ hour, label, major });
      }
      month++;
      if (month > 12) { month = 1; year++; }
    }
    if (shownYear === null && ticks.length > 0) {
      const date = gameDate(ticks[0]!.hour)!;
      ticks[0] = { ...ticks[0]!, label: `${shortMonth(date.month)} ${date.year}` };
    }
    if (useCache) {
      const curStart = engineHour({ year: end.year, month: end.month, day: 1, hour: 1 }) ?? endHour;
      const nextY = end.month === 12 ? end.year + 1 : end.year;
      const nextM = end.month === 12 ? 1 : end.month + 1;
      const nextStart = engineHour({ year: nextY, month: nextM, day: 1, hour: 1 }) ?? (TIME_AXIS_MAX_HOUR + 1);
      return storeTimeAxisTicks(cacheKey, {
        kind: 'month',
        validFromEnd: curStart,
        validBeforeEnd: nextStart,
        ticks,
      });
    }
    return ticks;
  }
  const years = end.year - start.year + 1;
  const fit = Math.max(2, Math.floor(plotWidth / YEAR_LABEL_SPACING));
  const yearSteps = [1, 2, 5, 10, 20, 50];
  const markStep = yearSteps.find(candidate => plotWidth / Math.ceil(years / candidate) >= MIN_MARK_SPACING)
    ?? Math.max(100, Math.round(niceStep(years, Math.max(2, Math.floor(plotWidth / MIN_MARK_SPACING)))));
  const labelStep = yearSteps.find(candidate => candidate % markStep === 0 && Math.ceil(years / candidate) <= fit)
    ?? Math.max(markStep, Math.ceil(Math.max(100, Math.round(niceStep(years, fit))) / markStep) * markStep);
  for (let year = Math.ceil(start.year / markStep) * markStep; year <= end.year && ticks.length < 512; year += markStep) {
    const hour = boundaryInView(engineHour({ year, month: 1, day: 1, hour: 1 }), startHour, endHour);
    if (hour !== null) ticks.push({ hour, label: year % labelStep === 0 ? String(year) : '', major: true });
  }
  if (useCache) {
    const curStart = engineHour({ year: end.year, month: 1, day: 1, hour: 1 }) ?? endHour;
    const nextStart = engineHour({ year: end.year + 1, month: 1, day: 1, hour: 1 }) ?? (TIME_AXIS_MAX_HOUR + 1);
    return storeTimeAxisTicks(cacheKey, {
      kind: 'year',
      validFromEnd: curStart,
      validBeforeEnd: nextStart,
      ticks,
    });
  }
  return ticks;
}

export interface TimeAxisFrame {
  readonly plotLeft: number;
  readonly plotRight: number;
  readonly startHour: number;
  readonly endHour: number;
  readonly toX?: (hour: number) => number;
}

export function timeAxisX(frame: TimeAxisFrame, hour: number): number {
  if (frame.toX) return frame.toX(hour);
  const span = Math.max(1, frame.endHour - frame.startHour);
  return frame.plotLeft + ((hour - frame.startHour) / span) * (frame.plotRight - frame.plotLeft);
}

export function drawTimeAxisGrid(
  ctx: Canvas2D,
  ticks: readonly TimeAxisTick[],
  frame: TimeAxisFrame,
  top: number,
  bottom: number,
  theme: FamilyTheme,
): void {
  const tokens = resolveKitTokens(theme);
  ctx.save();
  ctx.strokeStyle = tokens.hairline;
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (const tick of ticks) {
    if (!tick.label) continue;
    const x = Math.floor(timeAxisX(frame, tick.hour)) + 0.5;
    if (x <= frame.plotLeft + 1 || x >= frame.plotRight - 1) continue;
    ctx.moveTo(x, top);
    ctx.lineTo(x, bottom);
  }
  ctx.stroke();
  ctx.restore();
}

export interface TimeAxisOptions extends TimeAxisFrame {
  readonly axisTop: number;
  readonly playhead: number | null;
  readonly playheadLabel: string;
  readonly hover?: number | null;
  readonly hoverLabel?: string;
  readonly playheadLineTop?: number | null;
  readonly hairline?: 'normal' | 'strong' | 'band' | 'none';
  readonly tickMarks?: boolean;
  readonly tickColor?: string;
  readonly firstLabelOnly?: boolean;
  readonly tabBounds?: { readonly left: number; readonly right: number };
  readonly tabY?: number;
  readonly labelY?: number;
  readonly theme: FamilyTheme;
}

export interface PlacedTimeAxisLabel {
  readonly hour: number;
  readonly x: number;
  readonly label: string;
  readonly major: boolean;
}

function dateTabSpan(x: number, width: number, bounds: { readonly left: number; readonly right: number }): readonly [number, number] {
  const left = Math.max(bounds.left, Math.min(bounds.right - width, Math.round(x - width / 2)));
  return [left - 6, left + width + 6];
}

export function timeAxisLabels(
  ticks: readonly TimeAxisTick[],
  options: Omit<TimeAxisOptions, 'axisTop' | 'theme'>,
  measure: (text: string) => number,
): PlacedTimeAxisLabel[] {
  const bounds = options.tabBounds ?? { left: options.plotLeft, right: options.plotRight };
  const keepOut: (readonly [number, number])[] = [];
  if (options.playhead !== null) keepOut.push(dateTabSpan(timeAxisX(options, options.playhead), measure(options.playheadLabel) + 14, bounds));
  if (options.hover != null && options.hoverLabel) keepOut.push(dateTabSpan(timeAxisX(options, options.hover), measure(options.hoverLabel) + 14, bounds));
  const placed: PlacedTimeAxisLabel[] = [];
  let lastRight = -Infinity;
  let placedYear: number | null = null;
  for (const tick of ticks) {
    if (!tick.label || (options.firstLabelOnly && placed.length > 0)) continue;
    const year = gameYear(tick.hour);
    const label = year !== null && placedYear !== year && !tick.label.includes(String(year))
      ? `${tick.label} ${year}`
      : tick.label;
    const x = timeAxisX(options, tick.hour);
    const half = measure(label) / 2;
    if (x - half < options.plotLeft - 4 || x + half > options.plotRight + 4) continue;
    if (keepOut.some(([l, r]) => x + half > l && x - half < r)) continue;
    if (x - half < lastRight + 8) continue;
    placed.push({ hour: tick.hour, x, label, major: tick.major !== false });
    if (year !== null) placedYear = year;
    lastRight = x + half;
  }
  if (placed.length === 0 && ticks.length > 0) {
    const midX = (options.plotLeft + options.plotRight) / 2;
    const candidates = ticks
      .map(tick => ({ tick, x: timeAxisX(options, tick.hour) }))
      .sort((a, b) => Math.abs(a.x - midX) - Math.abs(b.x - midX));
    for (const { tick, x } of candidates) {
      const date = gameDate(tick.hour);
      const fallback = tick.label
        ? (date !== null && !tick.label.includes(String(date.year)) ? `${tick.label} ${date.year}` : tick.label)
        : date !== null
          ? (date.month === 1 && date.day === 1
            ? String(date.year)
            : date.day === 1
              ? `${shortMonth(date.month)} ${date.year}`
              : `${date.day} ${shortMonth(date.month)} ${date.year}`)
          : formatDateOnly(tick.hour);
      if (!fallback) continue;
      const half = measure(fallback) / 2;
      if (x - half < options.plotLeft - 4 || x + half > options.plotRight + 4) continue;
      if (keepOut.some(([l, r]) => x + half > l && x - half < r)) continue;
      placed.push({ hour: tick.hour, x, label: fallback, major: tick.major !== false });
      break;
    }
  }
  return placed;
}

export function drawTimeAxis(ctx: Canvas2D, ticks: readonly TimeAxisTick[], options: TimeAxisOptions): void {
  const { plotLeft, plotRight, axisTop, theme } = options;
  const tokens = resolveKitTokens(theme);
  const hairline = options.hairline ?? 'band';
  if (hairline !== 'none') drawRowHairline(ctx, plotLeft, plotRight, axisTop, theme, hairline);
  const tabBounds = options.tabBounds ?? { left: plotLeft, right: plotRight };
  const tabY = options.tabY ?? axisTop + 4;
  const labelY = options.labelY ?? axisTop + 14;
  const axisFont = `400 ${theme.fontSize.small}px ${theme.font}`;
  ctx.save();
  ctx.font = axisFont;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.strokeStyle = options.tickColor ?? tokens.hairlineStrong;
  ctx.lineWidth = 1;
  if (options.tickMarks !== false) {
    for (const tick of ticks) {
      const x = Math.floor(timeAxisX(options, tick.hour)) + 0.5;
      ctx.beginPath();
      ctx.moveTo(x, axisTop);
      ctx.lineTo(x, axisTop + (tick.major === false ? 3 : 5));
      ctx.stroke();
    }
  }
  for (const placed of timeAxisLabels(ticks, options, text => measureCanvasText(ctx, text, axisFont))) {
    ctx.fillStyle = placed.major ? tokens.textSecondary : theme.muted;
    ctx.fillText(placed.label, placed.x, labelY);
  }
  ctx.restore();
  if (options.playhead !== null) {
    const px = timeAxisX(options, options.playhead);
    if (options.playheadLineTop !== null) {
      drawPlayheadLine(ctx, px, options.playheadLineTop ?? 0, axisTop, theme);
    }
    drawDateTab(ctx, options.playheadLabel, px, tabY, theme, 'playhead', tabBounds);
  }
  if (options.hover != null && options.hoverLabel) {
    drawDateTab(ctx, options.hoverLabel, timeAxisX(options, options.hover), tabY, theme, 'hover', tabBounds);
  }
}

export function winRateColor(rate: number, theme: FamilyTheme): string {
  const tokens = resolveKitTokens(theme);
  if (rate <= 0.35) return tokens.shortage;
  if (rate >= 0.65) return tokens.goodInk;
  if (rate < 0.5) return mix(tokens.shortage, tokens.textSecondary, (rate - 0.35) / 0.15);
  return mix(tokens.textSecondary, tokens.goodInk, (rate - 0.5) / 0.15);
}

export function drawWinRateCell(
  ctx: Canvas2D,
  left: number,
  right: number,
  middleY: number,
  won: number | null,
  lost: number | null,
  theme: FamilyTheme,
): { readonly pct: number | null } {
  const total = won !== null && lost !== null ? won + lost : 0;
  ctx.save();
  ctx.textBaseline = 'middle';
  if (total <= 0) {
    ctx.font = `${LABEL_WEIGHT} 13.5px ${theme.font}`;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'right';
    ctx.fillText(NOT_APPLICABLE_LABEL, right, middleY);
    ctx.restore();
    return { pct: null };
  }
  const rate = Math.max(0, Math.min(1, won! / total));
  const tokens = resolveKitTokens(theme);
  const pct = Math.round(rate * 100);
  ctx.font = `600 13.5px ${theme.font}`;
  ctx.fillStyle = winRateColor(rate, theme);
  ctx.textAlign = 'right';
  ctx.fillText(`${pct}%`, right, middleY);
  const barW = 40;
  const barH = 4;
  const pctSlot = 38;
  const gap = 9;
  const barX = right - pctSlot - gap - barW;
  if (barX >= left) {
    const barY = Math.round(middleY - barH / 2);
    const hasBoth = won! > 0 && lost! > 0;
    const splitGap = hasBoth ? 1 : 0;
    const usable = barW - splitGap;
    const wonW = won! > 0 ? Math.max(2, Math.round(usable * rate)) : 0;
    const lostW = lost! > 0 ? Math.max(2, usable - wonW) : 0;
    if (wonW > 0) {
      ctx.fillStyle = tokens.goodFill;
      ctx.beginPath();
      ctx.roundRect(barX, barY, wonW, barH, [2, hasBoth ? 0 : 2, hasBoth ? 0 : 2, 2]);
      ctx.fill();
    }
    if (lostW > 0) {
      ctx.fillStyle = tokens.shortFill;
      ctx.beginPath();
      ctx.roundRect(barX + wonW + splitGap, barY, lostW, barH, [hasBoth ? 0 : 2, 2, 2, hasBoth ? 0 : 2]);
      ctx.fill();
    }
  }
  ctx.restore();
  return { pct };
}

export function drawOutOfFigure(
  ctx: Canvas2D,
  valueText: string,
  maxText: string,
  left: number,
  right: number,
  middleY: number,
  theme: FamilyTheme,
  options?: { readonly muted?: boolean; readonly fontSize?: number },
): void {
  const size = options?.fontSize ?? 13.5;
  const suffix = ` / ${maxText}`;
  const valFont = `500 ${size}px ${theme.font}`;
  const sufFont = `400 ${Math.max(11, size - 1.5)}px ${theme.font}`;
  ctx.save();
  ctx.textBaseline = 'middle';
  ctx.textAlign = 'right';
  ctx.font = sufFont;
  const sufW = textWidth(ctx, suffix);
  const avail = Math.max(0, right - left);
  if (sufW < avail) {
    ctx.fillStyle = theme.muted;
    ctx.fillText(suffix, right, middleY);
    ctx.font = valFont;
    ctx.fillStyle = options?.muted ? theme.muted : theme.text;
    ctx.fillText(ellipsise(valueText, Math.max(0, avail - sufW), s => textWidth(ctx, s)), right - sufW, middleY);
  } else {
    ctx.font = valFont;
    ctx.fillStyle = options?.muted ? theme.muted : theme.text;
    ctx.fillText(ellipsise(`${valueText}${suffix}`, avail, s => textWidth(ctx, s)), right, middleY);
  }
  ctx.restore();
}

export function drawSkillFigure(
  ctx: Canvas2D,
  left: number,
  right: number,
  middleY: number,
  valueText: string,
  iconState: ArtSlotState,
  theme: FamilyTheme,
  options?: { readonly badge?: boolean; readonly muted?: boolean },
): void {
  const iconW = options?.badge ? 18 : 24;
  const iconH = options?.badge ? 26 : 22;
  const gap = 6;
  const font = `${options?.badge ? 600 : 500} 13.5px ${theme.font}`;
  ctx.save();
  ctx.font = font;
  ctx.textAlign = 'right';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = options?.muted ? theme.muted : theme.text;
  const numW = Math.max(14, textWidth(ctx, valueText));
  ctx.fillText(valueText, right, middleY);
  const iconX = right - numW - gap - iconW;
  if (iconX >= left) {
    drawArtSlot(ctx, iconX, Math.round(middleY - iconH / 2), iconW, iconH, iconState, theme, { radius: 0 });
  }
  ctx.restore();
}

export interface IconStripItem {
  readonly state: ArtSlotState;
  readonly name: string;
  readonly count?: number;
}

export function iconStripSlotWidth(state: ArtSlotState, size: number): number {
  const width = state.kind === 'ready' || state.kind === 'pending' ? (state.width ?? 0) : 0;
  const height = state.kind === 'ready' || state.kind === 'pending' ? (state.height ?? 0) : 0;
  if (width <= 0 || height <= 0) return size;
  return Math.max(1, Math.min(size, Math.round(size * width / height)));
}

export function fitIconStripCount(
  available: number,
  count: number,
  slotWidth: (index: number) => number,
  itemCount: (index: number) => number | undefined,
  measure: (text: string, font: string) => number,
  themeFont: string,
): number {
  if (count <= 0 || available <= 0) return 0;
  const gap = ICON_STRIP_GAP;
  const countFont = `600 11px ${themeFont}`;
  const moreFont = `500 11.5px ${themeFont}`;
  const totals: number[] = [];
  let used = 0;
  let overflowed = false;
  for (let i = 0; i < count; i++) {
    const n = itemCount(i);
    const countW = n && n > 1 ? measure(`\u00d7${n}`, countFont) + 3 : 0;
    const total = slotWidth(i) + countW;
    totals.push(total);
    used += (i > 0 ? gap : 0) + total;
    if (used > available) {
      overflowed = true;
      break;
    }
  }
  if (!overflowed) return count;
  let drawnCount = 0;
  let fit = 0;
  for (let i = 0; i < totals.length; i++) {
    const nextUsed = fit + (i > 0 ? gap : 0) + totals[i]!;
    const remaining = count - (i + 1);
    const moreW = remaining > 0 ? gap + measure(`+${remaining}`, moreFont) : 0;
    if (nextUsed + moreW > available) break;
    fit = nextUsed;
    drawnCount = i + 1;
  }
  return drawnCount;
}

export function drawIconStrip(
  ctx: Canvas2D,
  left: number,
  right: number,
  middleY: number,
  items: readonly IconStripItem[],
  theme: FamilyTheme,
  options?: { readonly kind?: 'traits' | 'medals' },
): { readonly drawnCount: number; readonly overflowCount: number } {
  const size = options?.kind === 'medals' ? MEDAL_ICON_SIZE : TRAIT_ICON_SIZE;
  const gap = ICON_STRIP_GAP;
  const y = Math.round(middleY - size / 2);
  const countFont = `600 11px ${theme.font}`;
  const moreFont = `500 11.5px ${theme.font}`;
  const available = Math.max(0, right - left);
  ctx.save();
  const measure = (text: string, font: string): number => {
    ctx.font = font;
    return textWidth(ctx, text);
  };
  const drawnCount = fitIconStripCount(
    available,
    items.length,
    i => iconStripSlotWidth(items[i]!.state, size),
    i => items[i]!.count,
    measure,
    theme.font,
  );
  ctx.restore();
  let cursor = left;
  for (let i = 0; i < drawnCount; i++) {
    const item = items[i]!;
    const width = iconStripSlotWidth(item.state, size);
    const countText = item.count && item.count > 1 ? `\u00d7${item.count}` : '';
    drawArtSlot(ctx, cursor, y, width, size, item.state, theme, { radius: 3 });
    cursor += width;
    if (countText) {
      ctx.save();
      ctx.font = countFont;
      const countW = textWidth(ctx, countText) + 3;
      ctx.fillStyle = theme.text;
      ctx.textAlign = 'left';
      ctx.textBaseline = 'middle';
      ctx.fillText(countText, cursor + 2, middleY);
      ctx.restore();
      cursor += countW;
    }
    cursor += gap;
  }
  const overflowCount = items.length - drawnCount;
  if (overflowCount > 0 && cursor <= right) {
    ctx.save();
    ctx.font = moreFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(`+${overflowCount}`, cursor, middleY);
    ctx.restore();
  }
  return { drawnCount, overflowCount };
}

let playPath: Path2D | null = null;

export interface ActionButtonShape {
  readonly padLeft: number;
  readonly padRight: number;
  readonly gap: number;
  readonly height: number;
}

export function drawActionButton(
  ctx: Canvas2D,
  right: number,
  middleY: number,
  label: string,
  active: boolean,
  theme: FamilyTheme,
  icon?: 'play',
  shape: ActionButtonShape = { padLeft: 8, padRight: 8, gap: 5, height: 24 },
): { readonly x: number; readonly y: number; readonly width: number; readonly height: number } {
  const tokens = resolveKitTokens(theme);
  const iconSize = 13;
  const drawIcon = (ix: number, iy: number, color: string, alpha: number) => {
    ctx.save();
    ctx.globalAlpha = alpha;
    if (icon === 'play') {
      playPath ??= new Path2D('M5.2 3.4v9.2c0 .5.5.8.9.5l7-4.6c.4-.3.4-.8 0-1.1l-7-4.6c-.4-.2-.9 0-.9.6z');
      ctx.fillStyle = color;
      ctx.translate(ix, iy);
      ctx.scale(iconSize / 16, iconSize / 16);
      ctx.fill(playPath);
      ctx.restore();
      return;
    }
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.3;
    ctx.beginPath();
    ctx.roundRect(ix + 0.75, iy + 1.25, iconSize - 1.5, iconSize - 2.5, 2);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(ix + 1, iy + 4.5);
    ctx.lineTo(ix + iconSize - 1, iy + 4.5);
    ctx.stroke();
    ctx.restore();
  };
  if (!active) {
    const ix = right - iconSize;
    const iy = Math.round(middleY - iconSize / 2);
    drawIcon(ix, iy, theme.muted, 0.55);
    return { x: ix - 4, y: iy - 4, width: iconSize + 8, height: iconSize + 8 };
  }
  const font = `500 12px ${theme.font}`;
  ctx.save();
  ctx.font = font;
  const labelW = textWidth(ctx, label);
  const { padLeft, padRight, gap, height: h } = shape;
  const w = Math.ceil(padLeft + padRight + iconSize + gap + labelW);
  const x = right - w;
  const y = Math.round(middleY - h / 2);
  ctx.fillStyle = tokens.chipFill;
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, ROW_CORNER_RADIUS);
  ctx.fill();
  drawIcon(x + padLeft, Math.round(middleY - iconSize / 2), theme.text, 0.9);
  ctx.fillStyle = theme.text;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  ctx.fillText(label, x + padLeft + iconSize + gap, middleY);
  ctx.restore();
  return { x, y, width: w, height: h };
}

export const NET_BAR_HEIGHT = 12;

export function drawNetBar(
  ctx: Canvas2D,
  x: number,
  width: number,
  rowY: number,
  rowHeight: number,
  share: number | null,
  theme: FamilyTheme,
): void {
  const tokens = resolveKitTokens(theme);
  const middleY = rowY + rowHeight / 2;
  const centerX = Math.floor(x + width / 2) + 0.5;
  ctx.save();
  ctx.strokeStyle = tokens.netEdge;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(centerX, rowY);
  ctx.lineTo(centerX, rowY + rowHeight);
  ctx.stroke();
  ctx.restore();
  if (share === null || share === 0) return;
  const half = Math.max(0, width / 2 - 2);
  const barW = Math.max(BEHIND_BAR_MIN_WIDTH, Math.round(half * Math.min(1, Math.abs(share))));
  const barY = Math.round(middleY - NET_BAR_HEIGHT / 2);
  if (share < 0) drawBar(ctx, centerX - barW, barY, barW, NET_BAR_HEIGHT, tokens.shortFill, 'left', 1);
  else drawBar(ctx, centerX, barY, barW, NET_BAR_HEIGHT, tokens.netBar, 'right', 1);
}

export interface ClockSummaryData {
  readonly totalText: string;
  readonly playingText: string;
  readonly pausedText: string;
  readonly playingFraction: number | null;
  readonly pausedFraction: number | null;
  readonly playingPct: string | null;
  readonly pausedPct: string | null;
  readonly complete: boolean;
  readonly coverageLabel: string;
  readonly spanText: string | null;
}

export function drawClockSummary(
  ctx: Canvas2D,
  width: number,
  height: number,
  summary: ClockSummaryData,
  theme: FamilyTheme,
): void {
  const tokens = resolveKitTokens(theme);
  const padX = 22;
  const innerW = Math.max(1, width - padX * 2);
  ctx.save();
  ctx.fillStyle = theme.surface;
  ctx.fillRect(0, 0, width, height);
  ctx.textAlign = 'left';

  const figureFont = `600 36px ${theme.font}`;
  const captionFont = `400 13px ${theme.font}`;
  const labelFont = `400 13px ${theme.font}`;
  const timeFont = `600 13px ${theme.font}`;
  const secondary = tokens.textSecondary;

  ctx.textBaseline = 'alphabetic';
  ctx.font = figureFont;
  ctx.fillStyle = theme.text;
  const figure = ellipsise(summary.totalText, innerW, s => textWidth(ctx, s));
  ctx.fillText(figure, padX, 57);
  const figureW = textWidth(ctx, figure);
  ctx.font = captionFont;
  ctx.fillStyle = theme.muted;
  const captionX = padX + figureW + 12;
  if (captionX < padX + innerW) {
    ctx.fillText(ellipsise('Recorded, in real time', padX + innerW - captionX, s => textWidth(ctx, s)), captionX, 57);
  }

  const barY = 83;
  const barH = 8;
  const playColor = tokens.playFill;
  const pauseColor = tokens.pauseFill;
  if (summary.playingFraction !== null && summary.pausedFraction !== null) {
    const gap = summary.playingFraction > 0 && summary.pausedFraction > 0 ? 2 : 0;
    const usable = Math.max(0, innerW - gap);
    const playW = summary.playingFraction > 0 ? Math.max(3, Math.round(usable * summary.playingFraction)) : 0;
    const pauseW = summary.pausedFraction > 0 ? Math.max(3, usable - playW) : 0;
    if (playW > 0) {
      ctx.fillStyle = playColor;
      ctx.beginPath();
      ctx.roundRect(padX, barY, playW, barH, [4, gap ? 0 : 4, gap ? 0 : 4, 4]);
      ctx.fill();
    }
    if (pauseW > 0) {
      ctx.fillStyle = pauseColor;
      ctx.beginPath();
      ctx.roundRect(padX + playW + gap, barY, pauseW, barH, [gap ? 0 : 4, 4, 4, gap ? 0 : 4]);
      ctx.fill();
    }
  } else {
    drawHatchRect(ctx, padX, barY, innerW, barH, 4, theme);
  }

  ctx.textBaseline = 'middle';
  const statWidth = (label: string, time: string) => {
    ctx.font = labelFont;
    const lw = textWidth(ctx, label);
    ctx.font = timeFont;
    return { lw, total: lw + 8 + textWidth(ctx, time) };
  };
  const drawStat = (x: number, cy: number, label: string, time: string) => {
    const { lw } = statWidth(label, time);
    ctx.font = labelFont;
    ctx.fillStyle = secondary;
    ctx.fillText(label, x, cy);
    ctx.font = timeFont;
    ctx.fillStyle = theme.text;
    ctx.fillText(time, x + lw + 8, cy);
  };
  const playing = statWidth('Playing', summary.playingText);
  const paused = statWidth('Paused', summary.pausedText);
  let coverageY = 152;
  drawStat(padX, 117, 'Playing', summary.playingText);
  if (playing.total + 24 + paused.total <= innerW) {
    drawStat(padX + innerW - paused.total, 117, 'Paused', summary.pausedText);
  } else {
    drawStat(padX, 141, 'Paused', summary.pausedText);
    coverageY = 176;
  }

  ctx.fillStyle = summary.complete ? theme.muted : theme.amber;
  ctx.beginPath();
  ctx.arc(padX + 3.5, coverageY, 3.5, 0, Math.PI * 2);
  ctx.fill();
  ctx.font = labelFont;
  ctx.fillStyle = secondary;
  ctx.fillText(summary.coverageLabel, padX + 15, coverageY);
  if (summary.spanText) {
    const spanX = padX + 15 + textWidth(ctx, summary.coverageLabel) + 22;
    if (spanX < padX + innerW) {
      ctx.fillStyle = theme.muted;
      ctx.fillText(ellipsise(summary.spanText, padX + innerW - spanX, s => textWidth(ctx, s)), spanX, coverageY);
    }
  }
  ctx.restore();
}

export function drawGradientLegend(
  ctx: Canvas2D,
  x: number,
  middleY: number,
  lowLabel: string,
  highLabel: string,
  stops: readonly string[],
  theme: FamilyTheme,
  options?: { readonly barWidth?: number; readonly barHeight?: number; readonly gap?: number },
): { readonly width: number } {
  const barWidth = options?.barWidth ?? 64;
  const barHeight = options?.barHeight ?? 8;
  const gap = options?.gap ?? 6;
  const fontSize = theme.fontSize.small;
  ctx.save();
  ctx.font = `${LABEL_WEIGHT} ${fontSize}px ${theme.font}`;
  ctx.fillStyle = theme.muted;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  const lowW = lowLabel ? textWidth(ctx, lowLabel) : 0;
  const highW = highLabel ? textWidth(ctx, highLabel) : 0;
  let cursor = x;
  if (lowLabel) {
    ctx.fillText(lowLabel, cursor, middleY);
    cursor += lowW + gap;
  }
  const barY = Math.round(middleY - barHeight / 2);
  if (stops.length >= 2 && barWidth > 0) {
    const gradient = ctx.createLinearGradient(cursor, 0, cursor + barWidth, 0);
    const last = stops.length - 1;
    for (let i = 0; i < stops.length; i++) {
      gradient.addColorStop(i / last, stops[i]!);
    }
    ctx.fillStyle = gradient;
    ctx.beginPath();
    ctx.roundRect(cursor, barY, barWidth, barHeight, 2);
    ctx.fill();
  } else if (stops.length === 1 && barWidth > 0) {
    ctx.fillStyle = stops[0]!;
    ctx.beginPath();
    ctx.roundRect(cursor, barY, barWidth, barHeight, 2);
    ctx.fill();
  }
  cursor += barWidth;
  if (highLabel) {
    cursor += gap;
    ctx.fillStyle = theme.muted;
    ctx.fillText(highLabel, cursor, middleY);
    cursor += highW;
  }
  ctx.restore();
  return { width: cursor - x };
}

export interface LegendStripGradient {
  readonly lowLabel: string;
  readonly highLabel: string;
  readonly stops: readonly string[];
}

export interface LegendStripEntry {
  readonly label: string;
  readonly color?: string;
  readonly line?: boolean;
  readonly dash?: readonly number[];
  readonly hatch?: boolean;
  readonly outlined?: boolean;
  readonly swatch?: 'line' | 'square';
  readonly flag?: ArtSlotState;
  readonly muted?: boolean;
}

export interface LegendEntrySource {
  readonly length: number;
  at(index: number): LegendStripEntry;
  readonly trailing?: readonly LegendStripEntry[];
}

export interface LegendStripSpec {
  readonly gradient?: LegendStripGradient;
  readonly entries?: readonly LegendStripEntry[] | LegendEntrySource;
  readonly rightNote?: string;
  readonly padX?: number;
  readonly entryGap?: number;
  readonly hairline?: boolean;
}

export type LegendMeasure = (text: string, font: string) => number;

function legendEntryWidth(entry: LegendStripEntry, font: string, measure: LegendMeasure): number {
  const isLine = Boolean(entry.line || entry.swatch === 'line' || entry.dash?.length);
  const gap = 6;
  let width = 0;
  if (entry.hatch) width += 10 + gap;
  else if (entry.outlined) width += 9 + gap;
  else if (isLine) width += 14 + gap;
  else if (entry.color) width += 9 + gap;
  if (entry.flag && entry.flag.kind !== 'none') width += 16 + gap;
  return width + measure(entry.label, font);
}

function drawEntryItem(ctx: Canvas2D, cursor: number, middleY: number, entry: LegendStripEntry, theme: FamilyTheme): number {
  return drawLegendEntry(ctx, cursor, middleY, entry.label, entry.color ?? '', theme, {
    ...(entry.line ? { line: true } : {}),
    ...(entry.swatch ? { swatch: entry.swatch } : {}),
    ...(entry.dash?.length ? { dash: entry.dash } : {}),
    ...(entry.hatch ? { hatch: true } : {}),
    ...(entry.outlined ? { outlined: true } : {}),
    ...(entry.flag ? { flag: entry.flag } : {}),
    ...(entry.muted ?? (entry.hatch || entry.outlined) ? { muted: true } : {}),
  }).width;
}

export interface LegendFit {
  readonly entriesLeft: number;
  readonly shown: readonly LegendStripEntry[];
  readonly hidden: number;
  readonly missing: readonly LegendStripEntry[];
}

const GRADIENT_BAR_WIDTH = 64;
const GRADIENT_GAP = 6;

function legendFont(theme: FamilyTheme): string {
  return `${LABEL_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
}

export function fitLegendStrip(x: number, width: number, spec: LegendStripSpec, theme: FamilyTheme, measure: LegendMeasure): LegendFit {
  const font = legendFont(theme);
  const padX = spec.padX ?? 0;
  const gap = spec.entryGap ?? 12;
  let cursor = x + padX;
  if (spec.gradient) {
    const { lowLabel, highLabel } = spec.gradient;
    cursor += (lowLabel ? measure(lowLabel, font) + GRADIENT_GAP : 0) + GRADIENT_BAR_WIDTH + (highLabel ? GRADIENT_GAP + measure(highLabel, font) : 0) + 14;
  }
  const rightLimit = x + width - padX;
  const limit = spec.rightNote ? Math.max(cursor, rightLimit - measure(spec.rightNote, font) - gap) : rightLimit;
  const avail = Math.max(0, limit - cursor);
  const empty: LegendFit = { entriesLeft: cursor, shown: [], hidden: 0, missing: [] };
  if (!spec.entries) return empty;
  const list = Array.isArray(spec.entries) ? spec.entries as readonly LegendStripEntry[] : null;
  const source = list ? null : spec.entries as LegendEntrySource;
  const ordinary = list ? list.filter(entry => !entry.hatch) : null;
  const ordinaryCount = ordinary ? ordinary.length : source!.length;
  const ordinaryAt = (index: number) => ordinary ? ordinary[index]! : source!.at(index);
  const missing = list ? list.filter(entry => entry.hatch) : source!.trailing ?? [];
  const missingWidth = missing.reduce((sum, entry, index) => sum + (index > 0 ? gap : 0) + legendEntryWidth(entry, font, measure), 0);
  const total = ordinaryCount + missing.length;
  if (total === 0) return empty;
  const prefix: number[] = [0];
  const fetched: LegendStripEntry[] = [];
  for (let index = 0; index < ordinaryCount && prefix[index]! <= avail; index++) {
    const entry = ordinaryAt(index);
    fetched.push(entry);
    prefix.push(prefix[index]! + (index > 0 ? gap : 0) + legendEntryWidth(entry, font, measure));
  }
  const after = (used: number, next: number) => used + (used > 0 && next > 0 ? gap : 0) + next;
  if (fetched.length === ordinaryCount && after(prefix[ordinaryCount]!, missingWidth) <= avail) {
    return { entriesLeft: cursor, shown: fetched, hidden: 0, missing };
  }
  const more = (count: number) => measure(`+${count} more`, font);
  const keptMissing = missing.length > 0 && after(more(ordinaryCount), missingWidth) <= avail ? missing : [];
  const tail = keptMissing.length ? missingWidth : 0;
  let shown = 0;
  for (let count = fetched.length; count >= 0; count--) {
    const hidden = total - count - keptMissing.length;
    if (hidden > 0 && after(after(prefix[count]!, more(hidden)), tail) <= avail) {
      shown = count;
      break;
    }
    if (count === 0) return { entriesLeft: cursor, shown: [], hidden: 0, missing: [] };
  }
  return { entriesLeft: cursor, shown: fetched.slice(0, shown), hidden: total - shown - keptMissing.length, missing: keptMissing };
}

export function drawLegendStrip(
  ctx: Canvas2D,
  x: number,
  y: number,
  width: number,
  height: number,
  spec: LegendStripSpec,
  theme: FamilyTheme,
  fit: LegendFit = fitLegendStrip(x, width, spec, theme, (text, font) => measureCanvasText(ctx, text, font)),
): void {
  if (width <= 0 || height <= 0) return;
  const font = legendFont(theme);
  const gap = spec.entryGap ?? 12;
  const middleY = Math.round(y + height / 2);
  if (spec.gradient) {
    drawGradientLegend(ctx, x + (spec.padX ?? 0), middleY, spec.gradient.lowLabel, spec.gradient.highLabel, spec.gradient.stops, theme,
      { barWidth: GRADIENT_BAR_WIDTH, gap: GRADIENT_GAP });
  }
  let cursor = fit.entriesLeft;
  const list = Array.isArray(spec.entries) ? spec.entries as readonly LegendStripEntry[] : null;
  const source = list || !spec.entries ? null : spec.entries as LegendEntrySource;
  const liveOrdinary = list ? list.filter(entry => !entry.hatch) : null;
  const liveMissing = list ? list.filter(entry => entry.hatch) : source?.trailing ?? null;
  ctx.save();
  ctx.font = font;
  for (let i = 0; i < fit.shown.length; i++) {
    const entry = liveOrdinary
      ? (liveOrdinary[i] ?? fit.shown[i]!)
      : source && i < source.length
        ? source.at(i)
        : fit.shown[i]!;
    cursor += drawEntryItem(ctx, cursor, middleY, entry, theme) + gap;
  }
  if (fit.hidden > 0) {
    const label = `+${fit.hidden} more`;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(label, cursor, middleY);
    cursor += measureCanvasText(ctx, label, font) + gap;
  }
  for (let i = 0; i < fit.missing.length; i++) {
    const entry = liveMissing?.[i] ?? fit.missing[i]!;
    cursor += drawEntryItem(ctx, cursor, middleY, entry, theme) + gap;
  }
  ctx.restore();
  const rightLimit = x + width - (spec.padX ?? 0);
  if (spec.rightNote) {
    const maxW = Math.max(0, rightLimit - cursor);
    if (maxW >= 24) {
      ctx.save();
      ctx.font = font;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'right';
      ctx.textBaseline = 'middle';
      ctx.fillText(ellipsise(spec.rightNote, maxW, text => measureCanvasText(ctx, text, font)), rightLimit, middleY);
      ctx.restore();
    }
  }
  if (spec.hairline) {
    drawRowHairline(ctx, x, x + width, y + height - 1, theme, 'normal');
  }
}

export const ROW_SECTION_HEIGHT = 20;
export const ROW_REMOVE_SIZE = 22;
export const ROW_GRIP_WIDTH = 8;
export const ROW_GRIP_HEIGHT = 12;

export function drawRowSectionLabel(ctx: Canvas2D, x: number, y: number, height: number, label: string, theme: FamilyTheme): void {
  ctx.save();
  ctx.font = `${LABEL_WEIGHT} 11px ${theme.font}`;
  ctx.fillStyle = theme.muted;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  ctx.fillText(label, x, y + height / 2);
  ctx.restore();
}

export function rowGripBox(nameX: number, middleY: number): { readonly x: number; readonly y: number; readonly width: number; readonly height: number } {
  return { x: Math.max(0, nameX - ROW_GRIP_WIDTH - 1), y: Math.round(middleY - ROW_GRIP_HEIGHT / 2), width: ROW_GRIP_WIDTH, height: ROW_GRIP_HEIGHT };
}

export function drawRowGrip(ctx: Canvas2D, nameX: number, middleY: number, theme: FamilyTheme): void {
  const box = rowGripBox(nameX, middleY);
  ctx.save();
  ctx.fillStyle = theme.muted;
  for (let column = 0; column < 2; column++) {
    for (let line = 0; line < 3; line++) {
      ctx.fillRect(box.x + 1 + column * 4, box.y + 1 + line * 4, 2, 2);
    }
  }
  ctx.restore();
}

export function rowRemoveBox(right: number, middleY: number): { readonly x: number; readonly y: number; readonly width: number; readonly height: number } {
  return { x: right - ROW_REMOVE_SIZE, y: Math.round(middleY - ROW_REMOVE_SIZE / 2), width: ROW_REMOVE_SIZE, height: ROW_REMOVE_SIZE };
}

export function drawRowRemoveButton(ctx: Canvas2D, right: number, middleY: number, theme: FamilyTheme): void {
  const box = rowRemoveBox(right, middleY);
  ctx.save();
  ctx.fillStyle = mix(theme.surface, theme.text, 0.08);
  ctx.beginPath();
  ctx.roundRect(box.x, box.y, box.width, box.height, 4);
  ctx.fill();
  ctx.fillStyle = theme.text;
  ctx.font = `500 10px ${theme.font}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText('\u00d7', box.x + box.width / 2, middleY);
  ctx.restore();
}

export function drawAddRow(
  ctx: Canvas2D,
  nameX: number,
  middleY: number,
  lineLeft: number,
  lineRight: number,
  label: string,
  theme: FamilyTheme,
): void {
  const tokens = resolveKitTokens(theme);
  const plus = 12;
  const cx = nameX + plus / 2;
  const cy = Math.round(middleY) + 0.5;
  ctx.save();
  ctx.strokeStyle = theme.accent;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.moveTo(cx - plus / 2 + 1, cy);
  ctx.lineTo(cx + plus / 2 - 1, cy);
  ctx.moveTo(cx, cy - plus / 2 + 1);
  ctx.lineTo(cx, cy + plus / 2 - 1);
  ctx.stroke();
  ctx.font = `500 12px ${theme.font}`;
  ctx.fillStyle = theme.accent;
  ctx.textAlign = 'left';
  ctx.textBaseline = 'middle';
  ctx.fillText(label, nameX + plus + 6, middleY);
  if (lineRight > lineLeft) {
    ctx.strokeStyle = tokens.hairlineStrong;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(lineLeft, cy);
    ctx.lineTo(lineRight, cy);
    ctx.stroke();
  }
  ctx.restore();
}

export interface SharedContextSlot {
  readonly widgetKey: object;
  readonly slotBw: number;
  readonly slotBh: number;
}

const sharedContextSlots = new WeakMap<object, SharedContextSlot>();

export function setContextSlot(ctx: object, slot: SharedContextSlot): void {
  sharedContextSlots.set(ctx, slot);
}

export function contextWidgetKey(ctx: object): object {
  return sharedContextSlots.get(ctx)?.widgetKey ?? ctx;
}

export function contextSlotBacking(ctx: object): { readonly bw: number; readonly bh: number } | null {
  const slot = sharedContextSlots.get(ctx);
  return slot ? { bw: slot.slotBw, bh: slot.slotBh } : null;
}
