import { LABEL_WEIGHT, VALUE_WEIGHT } from '../../charts/draw.ts';
import { readableColour } from '../../charts/palette.ts';
import { disambiguateLabels, ellipsise, labelFitsTwoLines, labelLineHeight } from '../../charts/labels.ts';
import { textWidth, type Canvas2D } from '../../charts/textWidth.ts';
import { exactDecimal, linearTicks } from '../../charts/valueAxis.ts';
import {
  barCornerRadius,
  contrastInkForFill,
  drawShareBar,
  drawBar,
  drawLegendStrip,
  drawMissingMark,
  drawNameSlot,
  drawPartialTab,
  drawRowFocusRing,
  drawRowHairline,
  drawRowHover,
  drawRowSelected,
  drawScrollFade,
  drawSplitBar,
  resolveKitTokens,
  drawAddRow,
  drawRowGrip,
  drawRowRemoveButton,
  drawRowSectionLabel,
  ROW_REMOVE_SIZE,
  rowGripBox,
  rowRemoveBox,
} from '../../draw/kit.ts';
import { drawMediaSlot } from '../art/adapters.ts';
import type { LaneArtCache } from '../art/laneCache.ts';
import { collectArtUrls, type ArtUrlInput } from '../art/provider.ts';
import { drawHatchRect } from '../valueCanvas.ts';
import { formatCompactScalar, formatScalar, formatShare, formatValueState, notRecordedLabel } from '../valueFormat.ts';
import { mix } from '../../shell/appearance/color.ts';
import type { FamilyTheme } from '../contract.ts';
import {
  BARS_PAD_X,
  BARS_VALUE_GAP,
  fitOrderedNameWidth,
  isWholeScaleFormat,
  visibleBarsRange,
  type BarsLayout,
} from './layout.ts';
import {
  barsLabelMissing,
  barsLabelMuted,
  barsLabelText,
  formatBarsValueField,
  type BarsDataRow,
  type BarsModel,
  type BarsValueField,
} from './model.ts';
import { drawCaptionSides, drawPickList } from './pickList.ts';

export interface BarsGlideOffset {
  readonly fromY: number;
  readonly startedAt: number;
}

export interface BarsEasedRow {
  readonly numericFrac?: number;
  readonly leftFrac?: number;
  readonly rightFrac?: number;
  readonly rightSideFrac?: number;
  readonly targetFrac?: number;
  readonly orderedGeometry?: OrderedBarGeometry;
  readonly segmentWidths?: readonly number[];
  readonly segmentShares?: Readonly<Record<string, number>>;
}

interface OrderedBarGeometry {
  readonly fill: number;
  readonly gapStart: number;
  readonly gapEnd: number;
  readonly gapStroke: number;
  readonly labelEnd: number;
}

export function orderedBarGeometry(plotWidth: number, fillFraction: number, targetFraction?: number): OrderedBarGeometry {
  const target = targetFraction === undefined ? plotWidth : Math.min(plotWidth, Math.max(2, targetFraction * plotWidth));
  const fill = fillFraction > 0 ? Math.min(target, Math.max(2, fillFraction * plotWidth)) : 0;
  const labelEnd = targetFraction === undefined ? fill : target;
  if (labelEnd <= fill + 1) return { fill, gapStart: fill, gapEnd: fill, gapStroke: 0, labelEnd };
  const gapStart = fill > 0 ? fill + 1 : 0;
  const gapEnd = gapStart + Math.max(2, target - gapStart);
  return { fill, gapStart, gapEnd, gapStroke: 1, labelEnd };
}

export interface DrawFamilyBarsInput {
  readonly model: BarsModel;
  readonly layout: BarsLayout;
  readonly theme: FamilyTheme;
  readonly scrollTop: number;
  readonly now: number;
  readonly glideDuration: number;
  readonly glide?: ReadonlyMap<string, BarsGlideOffset>;
  readonly easedValues?: ReadonlyMap<string, BarsEasedRow>;
  readonly artCache?: LaneArtCache;
  readonly inspectedKey?: string | null;
  readonly selectedKey?: string | null;
  readonly focusedKey?: string | null;
}

export interface BarsPaintStats {
  readonly paintedRows: number;
  readonly truncatedKeys: readonly string[];
}

export interface BarsHitResult {
  readonly kind: 'none' | 'row' | 'right-row' | 'remove-region' | 'grip-region' | 'add-region';
  readonly rowKey?: string;
  readonly segmentKey?: string;
  readonly side?: 'left' | 'right';
}

export function easeOutCubic(t: number): number {
  const u = 1 - Math.max(0, Math.min(1, t));
  return 1 - u * u * u;
}

export function easeBarFraction(
  prevFrac: number | null | undefined,
  curFrac: number | null,
  progress: number,
): number | null {
  if (curFrac === null || !Number.isFinite(curFrac)) return curFrac;
  if (prevFrac === null || prevFrac === undefined || !Number.isFinite(prevFrac) || progress >= 1) return curFrac;
  if (progress <= 0) return prevFrac;
  return prevFrac + (curFrac - prevFrac) * easeOutCubic(progress);
}

function paintedRowY(
  key: string,
  baseY: number,
  glide: ReadonlyMap<string, BarsGlideOffset> | undefined,
  now: number,
  glideDuration: number,
): number {
  if (!glide || glideDuration <= 0) return baseY;
  const entry = glide.get(key);
  if (!entry) return baseY;
  const progress = (now - entry.startedAt) / glideDuration;
  if (progress >= 1) return baseY;
  return entry.fromY + (baseY - entry.fromY) * easeOutCubic(progress);
}

function drawStackedPair(ctx: Canvas2D, row: BarsDataRow, layout: BarsLayout, theme: FamilyTheme, fill: string, middleY: number, eased?: BarsEasedRow): void {
  const span = layout.plotWidth;
  const h = layout.barHeight;
  const solidY = Math.round(middleY - h - 1.5);
  const outlineY = Math.round(middleY + 1.5);
  const sides = [
    { measure: row.right, y: solidY, solid: true, frac: eased?.rightFrac },
    { measure: row.left, y: outlineY, solid: false, frac: eased?.leftFrac },
  ];
  const missing = sides.filter(side => side.measure && !measured(side.measure.value));
  if (missing.length === 2) {
    const kind = formatBarsValueField(row.right!.value).kind;
    drawMissingMark(ctx, layout.plotLeft, middleY, span, theme, { notApplicable: kind === 'notApplicable' });
  } else {
    for (const side of sides) {
      if (!side.measure) continue;
      if (!measured(side.measure.value)) {
        drawHatchRect(ctx, layout.plotLeft, side.y, Math.min(span, 10), h, 0, theme);
        continue;
      }
      const rawFrac = side.frac !== undefined ? side.frac : (side.measure.numeric ?? 0) / layout.scaleMax;
      if (rawFrac <= 0) continue;
      const w = Math.min(span, Math.max(2, rawFrac * span));
      if (side.solid) {
        drawBar(ctx, layout.plotLeft, side.y, w, h, fill, 'right');
      } else {
        ctx.save();
        ctx.strokeStyle = fill;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.roundRect(layout.plotLeft + 0.5, side.y + 0.5, Math.max(1, w - 1), h - 1, [0, 2, 2, 0]);
        ctx.stroke();
        ctx.restore();
      }
    }
  }
  if (layout.showReadout && row.readout) {
    ctx.save();
    ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
    ctx.fillStyle = theme.text;
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';
    ctx.fillText(ellipsise(row.readout.primary, layout.readoutWidth, t => textWidth(ctx, t)), layout.readoutX + layout.readoutWidth, middleY);
    ctx.restore();
  }
}

function measured(field: BarsValueField): boolean {
  const kind = formatBarsValueField(field).kind;
  return kind !== 'notRecorded' && kind !== 'notApplicable' && kind !== 'pending';
}

function drawHeader(ctx: Canvas2D, model: BarsModel, layout: BarsLayout, theme: FamilyTheme): void {
  if (layout.headerHeight <= 0) return;
  const middleY = layout.headerTop + layout.headerHeight / 2;
  ctx.save();
  ctx.textBaseline = 'middle';
  if (model.form === 'two-sided' && layout.twoSidedLayout === 'stacked-pair') {
    const legend = model.legend ?? [];
    if (legend.length > 0) {
      drawLegendStrip(ctx, layout.nameX, layout.headerTop, Math.max(0, layout.width - layout.nameX - BARS_PAD_X), layout.headerHeight, {
        entries: {
          length: legend.length,
          at: at => ({
            label: legend[at]!.label,
            color: theme.text,
            ...(at > 0 ? { outlined: true } : {}),
            muted: true,
          }),
        },
        entryGap: 18,
      }, theme);
    }
  } else if (model.form === 'two-sided' && layout.twoSidedLayout === 'paired') {
    ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
    ctx.fillStyle = theme.text;
    ctx.textAlign = 'center';
    const leftMid = (layout.plotLeft + layout.centerX) / 2;
    const rightMid = (layout.centerX + layout.plotRight) / 2;
    if (model.leftHeader) {
      ctx.fillText(ellipsise(model.leftHeader, layout.centerX - layout.plotLeft - 8, s => textWidth(ctx, s)), leftMid, middleY);
    }
    if (model.rightHeader) {
      ctx.fillText(ellipsise(model.rightHeader, layout.plotRight - layout.centerX - 8, s => textWidth(ctx, s)), rightMid, middleY);
    }
    if (layout.showReadout && model.readoutHeader) {
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'right';
      ctx.fillText(model.readoutHeader, layout.readoutX + layout.readoutWidth, middleY);
    }
  } else {
    if (model.caption) {
      ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
      ctx.fillStyle = theme.text;
      ctx.textAlign = 'left';
      ctx.fillText(ellipsise(model.caption, layout.width - layout.bodyLeft - BARS_PAD_X * 2, s => textWidth(ctx, s)), layout.bodyLeft + BARS_PAD_X, middleY);
    }
    if (model.captionRight && !model.captionSides?.length) {
      ctx.font = `${LABEL_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'right';
      ctx.fillText(model.captionRight, layout.width - BARS_PAD_X, middleY);
    }
  }
  ctx.restore();
}

function formatBarsAxisLabels(ticks: readonly number[], format: BarsModel['scaleFormat']): string[] {
  const compact = ticks.map(tick => formatCompactScalar(tick, format).text);
  const seenLabels = new Set<string>();
  const seenValues = new Set<number>();
  let collides = false;
  for (let i = 0; i < ticks.length; i++) {
    const tick = ticks[i]!;
    if (seenValues.has(tick)) continue;
    seenValues.add(tick);
    const label = compact[i]!;
    if (seenLabels.has(label)) {
      collides = true;
      break;
    }
    seenLabels.add(label);
  }
  if (!collides) return compact;
  const obj = typeof format === 'object' && format !== null ? format : null;
  const scale = obj?.decimals ?? (format === 'fixed5Amount' || format === 'fixed5Percent' || format === 'fixed5Width' ? 5 : 0);
  const scalePercent = obj?.percent === true;
  const percent = scalePercent || format === 'fixed5Percent' || format === 'percent2' || format === 'recordedPercent';
  const unit = obj?.unit;
  const allWhole = ticks.every(Number.isInteger);
  const scaleDiv = scale > 0 ? 10 ** scale : 1;
  let places = 0;
  for (const tick of ticks) {
    if (!Number.isFinite(tick)) continue;
    const displayVal = scale > 0 ? (scalePercent ? (tick * 100) / scaleDiv : tick / scaleDiv) : tick;
    places = Math.max(places, exactDecimal(displayVal).scale);
  }
  const maxDecimals = Math.min(12, places);
  return ticks.map(tick => {
    if (obj !== null && scale > 0 && allWhole) {
      return formatScalar(tick, {
        decimals: scale,
        maxDecimals,
        fixedDecimals: maxDecimals > 0,
        ...(scalePercent ? { percent: true } : {}),
        ...(unit ? { unit } : {}),
      });
    }
    const displayVal = scale > 0 ? (scalePercent ? (tick * 100) / scaleDiv : tick / scaleDiv) : tick;
    return formatScalar(displayVal, {
      decimals: 0,
      maxDecimals,
      fixedDecimals: maxDecimals > 0,
      ...(percent ? { percent: true } : {}),
      ...(unit ? { unit } : {}),
    });
  });
}

function drawAxis(ctx: Canvas2D, model: BarsModel, layout: BarsLayout, theme: FamilyTheme): void {
  if (!layout.showAxis || layout.axisHeight <= 0) return;
  const tokens = resolveKitTokens(theme);
  const ticks = linearTicks(0, layout.scaleMax, 4, isWholeScaleFormat(model.scaleFormat));
  const labels = formatBarsAxisLabels(ticks, model.scaleFormat);
  const y = layout.axisTop + layout.axisHeight / 2;
  ctx.save();
  ctx.font = `${LABEL_WEIGHT} 11px ${theme.font}`;
  ctx.textBaseline = 'middle';
  if (model.scaleUnit) {
    ctx.fillStyle = theme.muted;
    ctx.textAlign = 'left';
    ctx.fillText(ellipsise(model.scaleUnit, Math.max(0, layout.plotLeft - BARS_PAD_X - 6), s => textWidth(ctx, s)), BARS_PAD_X, y);
  }
  for (let i = 0; i < ticks.length; i++) {
    const tick = ticks[i]!;
    if (tick < 0 || tick > layout.scaleMax) continue;
    const tx = layout.plotLeft + (tick / layout.scaleMax) * layout.plotWidth;
    ctx.strokeStyle = tokens.gridLine;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(Math.floor(tx) + 0.5, layout.bodyTop);
    ctx.lineTo(Math.floor(tx) + 0.5, layout.bodyBottom);
    ctx.stroke();

    ctx.fillStyle = theme.muted;
    ctx.textAlign = i === 0 ? 'left' : i === ticks.length - 1 ? 'right' : 'center';
    ctx.fillText(labels[i]!, tx, y);
  }
  ctx.restore();
}

export function drawFamilyBars(ctx: Canvas2D, input: DrawFamilyBarsInput): BarsPaintStats {
  const { model, layout, theme, scrollTop, now, glideDuration, glide, easedValues, artCache, inspectedKey, selectedKey, focusedKey } = input;
  const tokens = resolveKitTokens(theme);
  ctx.clearRect(0, 0, layout.width, layout.height);

  drawHeader(ctx, model, layout, theme);
  if (model.captionSides?.length && layout.headerHeight > 0) {
    drawCaptionSides(ctx, model.captionSides, layout.width - BARS_PAD_X, layout.headerTop + layout.headerHeight / 2, theme, artCache);
  }
  if (layout.showAxis) drawAxis(ctx, model, layout, theme);

  const { first, last } = visibleBarsRange(layout, scrollTop);
  const truncatedKeys: string[] = [];
  let paintedRows = 0;

  const disambiguatedNames = new Map<string, string>();
  const disambiguatedRightNames = new Map<string, string>();
  const nameFontSize = theme.fontSize.body;
  const nameLineH = labelLineHeight(nameFontSize);
  const nameWidth = model.form === 'ordered' && layout.showReadout
    ? fitOrderedNameWidth(layout.nameWidth, layout.nameX, layout.plotLeft) : layout.nameWidth;
  if (nameWidth > 0) {
    const singleKeys: string[] = [];
    const singleLabels: string[] = [];
    const singleWidths: number[] = [];
    for (let i = first; i < last; i++) {
      const item = model.rows[i]!;
      if (item.kind !== 'data' || item.label === undefined || barsLabelMuted(item.label)) continue;
      if (labelFitsTwoLines(layout.rowHeights[i]!, nameLineH)) continue;
      const isHovered = inspectedKey === item.key || (item.rightSide && inspectedKey === item.rightSide.key);
      const isFocused = focusedKey === item.key || (item.rightSide && focusedKey === item.rightSide.key);
      const reserveRemove = item.region !== undefined && (isHovered || isFocused) ? ROW_REMOVE_SIZE + 4 : 0;
      singleKeys.push(item.key);
      singleLabels.push(barsLabelText(item.label));
      singleWidths.push(Math.max(12, nameWidth - reserveRemove));
    }
    if (singleLabels.length > 1) {
      ctx.save();
      ctx.font = `${model.form === 'net' ? 500 : 400} ${nameFontSize}px ${theme.font}`;
      const fitted = disambiguateLabels(singleLabels, singleWidths, s => textWidth(ctx, s));
      ctx.restore();
      for (let i = 0; i < singleKeys.length; i++) disambiguatedNames.set(singleKeys[i]!, fitted[i]!);
    }
  }
  if (model.form === 'two-sided' && layout.twoSidedLayout === 'split-sides' && layout.rightNameWidth > 0) {
    const rightKeys: string[] = [];
    const rightLabels: string[] = [];
    for (let i = first; i < last; i++) {
      const item = model.rows[i]!;
      if (item.kind !== 'data' || !item.rightSide || barsLabelMuted(item.rightSide.label)) continue;
      if (labelFitsTwoLines(layout.rowHeights[i]!, nameLineH)) continue;
      rightKeys.push(item.rightSide.key);
      rightLabels.push(barsLabelText(item.rightSide.label));
    }
    if (rightLabels.length > 1) {
      ctx.save();
      ctx.font = `${LABEL_WEIGHT} ${nameFontSize}px ${theme.font}`;
      const fitted = disambiguateLabels(rightLabels, layout.rightNameWidth, s => textWidth(ctx, s));
      ctx.restore();
      for (let i = 0; i < rightKeys.length; i++) disambiguatedRightNames.set(rightKeys[i]!, fitted[i]!);
    }
  }

  ctx.save();
  ctx.beginPath();
  ctx.rect(layout.bodyLeft, layout.bodyTop, layout.width - layout.bodyLeft, layout.bodyHeight);
  ctx.clip();

  if (model.form === 'net' || (model.form === 'two-sided' && layout.twoSidedLayout !== 'stacked-pair')) {
    const px = Math.floor(layout.centerX) + 0.5;
    ctx.save();
    ctx.strokeStyle = model.form === 'net' ? tokens.zeroLine : tokens.hairlineStrong;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(px, layout.bodyTop);
    ctx.lineTo(px, layout.bodyBottom);
    ctx.stroke();
    ctx.restore();
  }

  for (let i = first; i < last; i++) {
    const item = model.rows[i]!;
    const baseY = layout.bodyTop + layout.rowTops[i]! - scrollTop;
    const rowH = layout.rowHeights[i]!;
    const y = paintedRowY(item.key, baseY, glide, now, glideDuration);
    const middleY = y + rowH / 2;
    paintedRows++;

    if (item.kind === 'section') {
      drawRowSectionLabel(ctx, layout.nameX, y, rowH, item.label, theme);
      continue;
    }

    if (item.kind === 'add-region') {
      const hovered = inspectedKey === item.key;
      if (hovered) drawRowHover(ctx, layout.bodyLeft, y, layout.width - layout.bodyLeft, rowH, theme);
      if (focusedKey === item.key) drawRowFocusRing(ctx, layout.bodyLeft, y, layout.width - layout.bodyLeft, rowH, theme);
      drawAddRow(ctx, layout.nameX, middleY, layout.plotLeft, layout.plotRight, item.label, theme);
      continue;
    }

    const row: BarsDataRow = item;
    const isHovered = inspectedKey === row.key || (row.rightSide && inspectedKey === row.rightSide.key);
    const isSelected = selectedKey === row.key || (row.rightSide && selectedKey === row.rightSide.key);
    const isFocused = focusedKey === row.key || (row.rightSide && focusedKey === row.rightSide.key);

    if (isSelected) drawRowSelected(ctx, layout.bodyLeft, y, layout.width - layout.bodyLeft, rowH, theme);
    else if (isHovered) drawRowHover(ctx, layout.bodyLeft, y, layout.width - layout.bodyLeft, rowH, theme);
    if (isFocused) drawRowFocusRing(ctx, layout.bodyLeft, y, layout.width - layout.bodyLeft, rowH, theme);
    if (model.form === 'net' && i < model.rows.length - 1) {
      drawRowHairline(ctx, layout.bodyLeft + BARS_PAD_X, layout.width - BARS_PAD_X, y + rowH - 1, theme, 'normal');
    }

    drawMediaSlot(ctx, layout.bodyLeft + BARS_PAD_X, middleY, layout.mediaWidth, layout.mediaHeight, row.media, theme, { artCache, radius: 2 });

    if (nameWidth > 0 && row.label !== undefined) {
      const regionTools = row.region !== undefined && (isHovered || isFocused);
      const reserveRemove = regionTools ? ROW_REMOVE_SIZE + 4 : 0;
      const labelMuted = barsLabelMuted(row.label);
      const nameColor = labelMuted || row.muted ? theme.muted : row.group === 'auto' ? tokens.autoName : theme.text;
      const rawLabel = barsLabelText(row.label);
      const shownLabel = labelMuted ? rawLabel : (disambiguatedNames.get(row.key) ?? rawLabel);
      const fitted = drawNameSlot(
        ctx,
        shownLabel,
        layout.nameX,
        y,
        Math.max(12, nameWidth - reserveRemove),
        rowH,
        theme,
        { color: nameColor, weight: model.form === 'net' ? 500 : 400 },
      );
      if (fitted.truncated || shownLabel !== rawLabel) truncatedKeys.push(row.key);
      if (regionTools) {
        drawRowGrip(ctx, layout.nameX, middleY, theme);
        drawRowRemoveButton(ctx, layout.nameX + layout.nameWidth, middleY, theme);
      }
    }

    const barY = Math.round(middleY - layout.barHeight / 2);
    const barColor = barsLabelMissing(row.label) ? theme.grey : readableColour(row.color ?? theme.accent, theme.surface);
    const eased = easedValues?.get(row.key);

    if (model.form === 'net') {
      const formatted = row.value ? formatBarsValueField(row.value) : null;
      const halfSpan = Math.max(1, (layout.plotRight - layout.plotLeft) / 2);
      if (formatted && (formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable')) {
        drawMissingMark(ctx, layout.plotLeft, middleY, layout.plotWidth, theme, {
          figure: row.value?.figure,
          notApplicable: formatted.kind === 'notApplicable',
        });
      } else if (eased?.numericFrac !== undefined) {
        if (Math.abs(eased.numericFrac) > 1e-6) {
          const rawW = Math.abs(eased.numericFrac) * halfSpan;
          const barW = Math.min(halfSpan, Math.max(2, rawW));
          if (eased.numericFrac > 0) {
            drawBar(ctx, layout.centerX, barY, barW, layout.barHeight, barColor, 'right');
          } else {
            drawBar(ctx, layout.centerX - barW, barY, barW, layout.barHeight, tokens.shortFill, 'left');
          }
        }
      } else if (row.numeric !== undefined && row.numeric !== null && row.numeric !== 0) {
        const rawW = (Math.abs(row.numeric) / layout.scaleMax) * halfSpan;
        const barW = Math.min(halfSpan, Math.max(2, rawW));
        if (row.numeric > 0) {
          drawBar(ctx, layout.centerX, barY, barW, layout.barHeight, barColor, 'right');
        } else {
          drawBar(ctx, layout.centerX - barW, barY, barW, layout.barHeight, tokens.shortFill, 'left');
        }
      }

      if (layout.showReadout && formatted) {
        const rightEdge = layout.readoutX + layout.readoutWidth;
        if (formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable') {
          drawMissingMark(ctx, layout.readoutX, middleY, layout.readoutWidth, theme, {
            align: 'right',
            notApplicable: formatted.kind === 'notApplicable',
          });
        } else {
          ctx.save();
          ctx.textAlign = 'right';
          ctx.textBaseline = 'middle';
          ctx.font = `600 13px ${theme.font}`;
          ctx.fillStyle = row.value?.tone === 'shortage' || (row.numeric ?? 0) < 0
            ? tokens.shortage
            : (row.numeric ?? 0) === 0
              ? theme.muted
              : theme.text;
          const captioned = layout.showPair;
          ctx.fillText(ellipsise(formatted.text, layout.readoutWidth, s => textWidth(ctx, s)), rightEdge, captioned ? middleY - 7 : middleY);
          if (captioned) {
            const sub = row.value?.caption ?? ((row.numeric ?? 0) < 0 ? 'Short a day' : (row.numeric ?? 0) > 0 ? 'Surplus a day' : 'Balanced');
            ctx.font = `${LABEL_WEIGHT} 11px ${theme.font}`;
            ctx.fillStyle = theme.muted;
            ctx.fillText(ellipsise(sub, layout.readoutWidth, s => textWidth(ctx, s)), rightEdge, middleY + 8);
          }
          ctx.restore();
        }
      }

      if (layout.showPair && row.pair) {
        const topFmt = formatValueState(row.pair.topValue, row.pair.format);
        const botFmt = formatValueState(row.pair.bottomValue, row.pair.format);
        const topVal = topFmt.kind === 'notRecorded' || topFmt.kind === 'notApplicable' || topFmt.kind === 'pending' ? topFmt.label : topFmt.text;
        const botVal = botFmt.kind === 'notRecorded' || botFmt.kind === 'notApplicable' || botFmt.kind === 'pending' ? botFmt.label : botFmt.text;
        const rightEdge = layout.pairX + layout.pairWidth;
        ctx.save();
        ctx.textBaseline = 'middle';
        ctx.font = `${LABEL_WEIGHT} 11px ${theme.font}`;
        ctx.fillStyle = theme.muted;
        ctx.textAlign = 'left';
        ctx.fillText(row.pair.topLabel, layout.pairX, middleY - 7);
        ctx.fillText(row.pair.bottomLabel, layout.pairX, middleY + 8);
        ctx.font = `${VALUE_WEIGHT} 12px ${theme.font}`;
        ctx.fillStyle = theme.text;
        ctx.textAlign = 'right';
        ctx.fillText(topVal, rightEdge, middleY - 7);
        ctx.fillText(botVal, rightEdge, middleY + 8);
        ctx.restore();
      }
      continue;
    }

    if (model.form === 'share' || model.form === 'stacked') {
      const valueMissing = row.value && (row.value.value.kind === 'notRecorded' || row.value.value.kind === 'notApplicable');
      if (valueMissing || (!row.segments?.length && row.value?.value.kind === 'notRecorded')) {
        drawHatchRect(ctx, layout.plotLeft, barY, layout.plotWidth, layout.barHeight, 2.5, theme);
        const msg = row.value?.value.kind === 'notApplicable' ? '\u2014' : notRecordedLabel(row.value?.figure);
        ctx.save();
        ctx.font = `${LABEL_WEIGHT} 11px ${theme.font}`;
        ctx.fillStyle = theme.muted;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(ellipsise(msg, Math.max(0, layout.plotWidth - 8), s => textWidth(ctx, s)), layout.plotLeft + layout.plotWidth / 2, middleY);
        ctx.restore();
      } else if (row.segments?.length) {
        drawShareBar(
          ctx,
          layout.plotLeft,
          barY,
          layout.plotWidth,
          layout.barHeight,
          row.segments.map(seg => {
            const share = eased?.segmentShares?.[seg.key] ?? seg.share;
            return {
              share,
              color: seg.color,
              hatched: seg.hatched,
              label: seg.inlineLabel ?? `${seg.label} ${formatShare(seg.share)}`,
              shortLabel: formatShare(seg.share),
            };
          }),
          theme,
          2.5,
        );
      } else {
        ctx.save();
        ctx.fillStyle = tokens.barTrack;
        ctx.beginPath();
        ctx.roundRect(layout.plotLeft, barY, layout.plotWidth, layout.barHeight, 2.5);
        ctx.fill();
        ctx.restore();
      }
      if (layout.showReadout && row.readout) {
        ctx.save();
        ctx.font = `${LABEL_WEIGHT} ${theme.fontSize.body}px ${theme.font}`;
        ctx.fillStyle = row.readout.tone === 'shortage' ? tokens.shortage : theme.muted;
        ctx.textAlign = 'right';
        ctx.textBaseline = 'middle';
        ctx.fillText(
          ellipsise(row.readout.primary, layout.readoutWidth, s => textWidth(ctx, s)),
          layout.readoutX + layout.readoutWidth,
          middleY,
        );
        ctx.restore();
      }
      continue;
    }

    if (model.form === 'two-sided' && layout.twoSidedLayout === 'split-sides') {
      if (row.value) {
        const leftFmt = formatBarsValueField(row.value);
        const leftSpan = Math.max(1, layout.plotRight - layout.plotLeft);
        if (leftFmt.kind === 'notRecorded' || leftFmt.kind === 'notApplicable') {
          drawMissingMark(ctx, layout.plotLeft, middleY, leftSpan, theme, { align: 'right', notApplicable: leftFmt.kind === 'notApplicable' });
        } else {
          const leftFrac = eased?.numericFrac !== undefined ? eased.numericFrac : (row.numeric ?? 0) / layout.scaleLeftMax;
          if (leftFrac > 0) {
            const w = Math.min(leftSpan, Math.max(2, leftFrac * leftSpan));
            drawBar(ctx, layout.plotRight - w, barY, w, layout.barHeight, barColor, 'left');
          }
          ctx.save();
          ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
          ctx.fillStyle = leftFmt.kind === 'zero' ? theme.muted : theme.text;
          ctx.textAlign = 'right';
          ctx.textBaseline = 'middle';
          ctx.fillText(leftFmt.text, layout.centerX - 18, middleY);
          ctx.restore();
        }
      }
      if (row.rightSide) {
        const right = row.rightSide;
        const rightFmt = formatBarsValueField(right.value);
        const rightSpan = Math.max(1, layout.rightPlotRight - layout.rightPlotLeft);
        const rightMuted = barsLabelMuted(right.label);
        const rightColor = barsLabelMissing(right.label) ? theme.grey : (right.color ?? theme.accent);
        if (rightFmt.kind === 'notRecorded' || rightFmt.kind === 'notApplicable') {
          drawMissingMark(ctx, layout.rightPlotLeft, middleY, rightSpan, theme, { align: 'left', notApplicable: rightFmt.kind === 'notApplicable' });
        } else {
          ctx.save();
          ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
          ctx.fillStyle = rightFmt.kind === 'zero' ? theme.muted : theme.text;
          ctx.textAlign = 'left';
          ctx.textBaseline = 'middle';
          ctx.fillText(rightFmt.text, layout.centerX + 18, middleY);
          ctx.restore();
          const rightFrac = eased?.rightSideFrac !== undefined ? eased.rightSideFrac : (right.numeric ?? 0) / layout.scaleRightMax;
          if (rightFrac > 0) {
            const w = Math.min(rightSpan, Math.max(2, rightFrac * rightSpan));
            drawBar(ctx, layout.rightPlotLeft, barY, w, layout.barHeight, rightColor, 'right');
          }
        }
        const rawRightLabel = barsLabelText(right.label);
        const shownRightLabel = rightMuted ? rawRightLabel : (disambiguatedRightNames.get(right.key) ?? rawRightLabel);
        const rightName = drawNameSlot(
          ctx,
          shownRightLabel,
          layout.rightNameX + layout.rightNameWidth,
          y,
          layout.rightNameWidth,
          rowH,
          theme,
          { align: 'right', color: rightMuted ? theme.muted : theme.text },
        );
        if (rightName.truncated || shownRightLabel !== rawRightLabel) truncatedKeys.push(right.key);
        drawMediaSlot(ctx, layout.width - BARS_PAD_X - layout.mediaWidth, middleY, layout.mediaWidth, layout.mediaHeight, right.media, theme, { artCache, radius: 2 });
      }
      continue;
    }

    if (model.form === 'two-sided' && layout.twoSidedLayout === 'stacked-pair') {
      drawStackedPair(ctx, row, layout, theme, barColor, middleY, eased);
      continue;
    }

    if (model.form === 'two-sided') {
      const leftAvail = Math.max(1, layout.centerX - layout.plotLeft - layout.valueLeftGutter);
      const rightAvail = Math.max(1, layout.plotRight - layout.centerX - layout.valueRightGutter);
      const leftFill = row.left?.color ?? mix(barColor, theme.surface, 0.42);
      const rightFill = row.right?.color ?? barColor;

      if (row.left) {
        const leftFmt = formatBarsValueField(row.left.value);
        if (leftFmt.kind === 'notRecorded' || leftFmt.kind === 'notApplicable') {
          drawMissingMark(ctx, layout.plotLeft, middleY, Math.max(12, layout.centerX - layout.plotLeft - 6), theme, {
            align: 'right',
            notApplicable: leftFmt.kind === 'notApplicable',
          });
        } else {
          const leftFrac = eased?.leftFrac !== undefined ? eased.leftFrac : (row.left.numeric ?? 0) / layout.scaleLeftMax;
          const w = leftFrac > 0 ? Math.min(leftAvail, Math.max(2, leftFrac * leftAvail)) : 0;
          if (w > 0) drawBar(ctx, layout.centerX - w, barY, w, layout.barHeight, leftFill, 'left');
          ctx.save();
          ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
          ctx.fillStyle = leftFmt.kind === 'zero' ? theme.muted : theme.text;
          ctx.textAlign = 'right';
          ctx.textBaseline = 'middle';
          ctx.fillText(leftFmt.text, layout.centerX - w - BARS_VALUE_GAP, middleY);
          ctx.restore();
        }
      }
      if (row.right) {
        const rightFmt = formatBarsValueField(row.right.value);
        if (rightFmt.kind === 'notRecorded' || rightFmt.kind === 'notApplicable') {
          drawMissingMark(ctx, layout.centerX + 6, middleY, Math.max(12, layout.plotRight - layout.centerX - 6), theme, {
            align: 'left',
            notApplicable: rightFmt.kind === 'notApplicable',
          });
        } else {
          const rightFrac = eased?.rightFrac !== undefined ? eased.rightFrac : (row.right.numeric ?? 0) / layout.scaleRightMax;
          const w = rightFrac > 0 ? Math.min(rightAvail, Math.max(2, rightFrac * rightAvail)) : 0;
          if (w > 0) drawBar(ctx, layout.centerX, barY, w, layout.barHeight, rightFill, 'right');
          ctx.save();
          ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
          ctx.fillStyle = rightFmt.kind === 'zero' ? theme.muted : theme.text;
          ctx.textAlign = 'left';
          ctx.textBaseline = 'middle';
          ctx.fillText(rightFmt.text, layout.centerX + w + BARS_VALUE_GAP, middleY);
          ctx.restore();
        }
      }
      if (layout.showReadout && row.readout) {
        ctx.save();
        ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
        ctx.fillStyle = theme.text;
        ctx.textAlign = 'right';
        ctx.textBaseline = 'middle';
        ctx.fillText(ellipsise(row.readout.primary, layout.readoutWidth, s => textWidth(ctx, s)), layout.readoutX + layout.readoutWidth, middleY);
        ctx.restore();
      }
      continue;
    }

    const formatted = row.value ? formatBarsValueField(row.value) : null;
    if (formatted && (formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable')) {
      drawMissingMark(ctx, layout.plotLeft, middleY, layout.plotWidth, theme, {
        figure: row.value?.figure,
        notApplicable: formatted.kind === 'notApplicable',
      });
      continue;
    }

    let drawnEndX: number;
    const targetFrac = row.target && row.target.numeric > 0 ? eased?.targetFrac ?? row.target.numeric / layout.scaleMax : undefined;
    if (targetFrac === undefined && row.segments?.length) {
      const totalFrac = eased?.numericFrac ?? (row.numeric ?? 0) / layout.scaleMax;
      const totalBarW = eased?.orderedGeometry?.fill ?? orderedBarGeometry(layout.plotWidth, totalFrac).fill;
      if (totalBarW > 0) {
        drawSplitBar(
          ctx,
          layout.plotLeft,
          barY,
          totalBarW,
          layout.barHeight,
          row.segments.map(seg => ({ fraction: seg.share, fill: seg.color, hatched: seg.hatched })),
          theme,
          { widths: eased?.segmentWidths },
        );
      }
      drawnEndX = layout.plotLeft + totalBarW;
    } else {
      const { fill, gapStart, gapEnd, gapStroke, labelEnd } = eased?.orderedGeometry ?? orderedBarGeometry(
        layout.plotWidth, eased?.numericFrac ?? (row.numeric ?? 0) / layout.scaleMax, targetFrac,
      );
      drawBar(ctx, layout.plotLeft, barY, fill, layout.barHeight, barColor, 'right');
      if (gapEnd > gapStart) {
        const gapX = layout.plotLeft + gapStart;
        const gapW = gapEnd - gapStart;
        const r = barCornerRadius(layout.barHeight);
        ctx.save();
        ctx.fillStyle = tokens.shortFill;
        ctx.globalAlpha = 0.16;
        ctx.beginPath();
        ctx.roundRect(gapX, barY, gapW, layout.barHeight, [0, r, r, 0]);
        ctx.fill();
        ctx.globalAlpha = 0.9;
        ctx.strokeStyle = tokens.shortage;
        ctx.lineWidth = gapStroke;
        ctx.setLineDash([2.5, 2]);
        ctx.beginPath();
        ctx.roundRect(gapX + gapStroke / 2, barY + gapStroke / 2, gapW - gapStroke, Math.max(0, layout.barHeight - gapStroke), [0, r, r, 0]);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.restore();
      }
      drawnEndX = layout.plotLeft + labelEnd;
    }

    if (layout.showReadout && row.readout) {
      const rightEdge = layout.readoutX + layout.readoutWidth;
      const primaryFont = `500 ${theme.fontSize.body}px ${theme.font}`;
      const iconReserve = row.readout.icon ? 21 : 0;
      ctx.save();
      ctx.textBaseline = 'middle';
      ctx.textAlign = 'right';
      ctx.font = primaryFont;
      const primText = ellipsise(row.readout.primary, layout.readoutWidth - iconReserve, s => textWidth(ctx, s));
      const primWidth = textWidth(ctx, primText);
      const extraStart = layout.readoutX + iconReserve + primWidth;
      let cursorRight = rightEdge;
      if (row.readout.shortfall) {
        ctx.font = `600 ${theme.fontSize.body}px ${theme.font}`;
        ctx.fillStyle = tokens.shortage;
        const shortfall = ellipsise(row.readout.shortfall, cursorRight - extraStart - 8, s => textWidth(ctx, s));
        ctx.fillText(shortfall, cursorRight, middleY);
        cursorRight -= shortfall ? textWidth(ctx, shortfall) + 8 : 0;
      }
      if (row.readout.secondary) {
        ctx.font = `400 ${theme.fontSize.body}px ${theme.font}`;
        ctx.fillStyle = theme.muted;
        const secText = ellipsise(row.readout.secondary, cursorRight - extraStart - 4, s => textWidth(ctx, s));
        ctx.fillText(secText, cursorRight, middleY);
        cursorRight -= secText ? textWidth(ctx, secText) + 4 : 0;
      }
      ctx.font = primaryFont;
      ctx.fillStyle = row.readout.tone === 'shortage' ? tokens.shortage : row.readout.tone === 'muted' ? theme.muted : theme.text;
      ctx.fillText(primText, cursorRight, middleY);
      cursorRight -= primWidth;
      ctx.restore();
      if (row.readout.icon) {
        drawMediaSlot(
          ctx,
          Math.round(cursorRight - iconReserve),
          middleY,
          16,
          16,
          { kind: 'art', art: row.readout.icon, resolvedName: row.readout.iconLabel },
          theme,
          { artCache, radius: 2 },
        );
      }
    } else if (formatted) {
      ctx.save();
      ctx.font = `500 ${theme.fontSize.body}px ${theme.font}`;
      ctx.textBaseline = 'middle';
      const labelW = textWidth(ctx, formatted.text);
      const fitsOutside = !layout.valueInsideBar && drawnEndX + BARS_VALUE_GAP + labelW <= layout.width - BARS_PAD_X;
      if (fitsOutside || drawnEndX - layout.plotLeft < labelW + 12) {
        ctx.fillStyle = formatted.kind === 'zero' ? theme.muted : theme.text;
        ctx.textAlign = 'left';
        ctx.fillText(formatted.text, drawnEndX + BARS_VALUE_GAP, middleY);
        if (formatted.kind === 'partial') {
          drawPartialTab(ctx, drawnEndX + BARS_VALUE_GAP + labelW + 5, middleY, theme);
        }
      } else {
        ctx.fillStyle = contrastInkForFill(barColor, theme);
        ctx.textAlign = 'right';
        ctx.fillText(formatted.text, drawnEndX - 6, middleY);
      }
      ctx.restore();
    }
  }

  if (scrollTop > 0) {
    drawScrollFade(ctx, 0, layout.bodyTop, layout.width, 18, theme, 'top');
  }
  if (scrollTop + layout.bodyHeight < layout.totalHeight - 1) {
    drawScrollFade(ctx, 0, layout.bodyBottom - 24, layout.width, 24, theme, 'bottom');
  }
  ctx.restore();

  if (layout.legendHeight > 0 && (model.legend?.length || model.scaleUnit)) {
    const entries = (model.legend ?? []).map(entry => ({
      label: entry.share ? `${entry.label} ${entry.share}` : entry.value ? `${entry.label} ${entry.value}` : entry.label,
      color: entry.color,
      ...(entry.hatched ? { hatch: true } : {}),
    }));
    const rowCount = Math.max(1, Math.round(layout.legendHeight / 22));
    if (rowCount > 1 && entries.length > 1) {
      const rowH = layout.legendHeight / rowCount;
      const entryGap = 14;
      ctx.save();
      ctx.font = `500 ${theme.fontSize.small}px ${theme.font}`;
      const rightReserve = model.scaleUnit ? Math.min(Math.floor(layout.width * 0.3), Math.ceil(textWidth(ctx, model.scaleUnit))) + entryGap : 0;
      const row1Limit = Math.max(80, layout.width - BARS_PAD_X * 2 - rightReserve);
      let used = 0;
      let splitAt = entries.length;
      for (let i = 0; i < entries.length; i++) {
        const w = 18 + Math.ceil(textWidth(ctx, entries[i]!.label));
        const next = used + (i > 0 ? entryGap : 0) + w;
        if (i > 0 && next > row1Limit) {
          splitAt = i;
          break;
        }
        used = next;
      }
      ctx.restore();
      drawLegendStrip(ctx, 0, layout.legendTop, layout.width, rowH, {
        entries: entries.slice(0, splitAt),
        ...(model.scaleUnit ? { rightNote: model.scaleUnit } : {}),
        padX: BARS_PAD_X,
        entryGap,
      }, theme);
      if (splitAt < entries.length) {
        drawLegendStrip(ctx, 0, layout.legendTop + rowH, layout.width, rowH, {
          entries: entries.slice(splitAt),
          padX: BARS_PAD_X,
          entryGap,
        }, theme);
      }
    } else {
      drawLegendStrip(ctx, 0, layout.legendTop, layout.width, layout.legendHeight, {
        entries,
        ...(model.scaleUnit ? { rightNote: model.scaleUnit } : {}),
        padX: BARS_PAD_X,
        entryGap: 14,
      }, theme);
    }
  }

  if (model.pickList) drawPickList(ctx, model.pickList, layout, theme, inspectedKey ?? null, focusedKey ?? null);
  return { paintedRows, truncatedKeys };
}

export function hitFamilyBars(
  layout: BarsLayout,
  model: BarsModel,
  point: { readonly x: number; readonly y: number },
  scrollTop: number,
  glide?: ReadonlyMap<string, BarsGlideOffset>,
  now = 0,
  glideDuration = 0,
  removeShown: ReadonlySet<string> | null = null,
): BarsHitResult {
  if (point.y < layout.bodyTop || point.y > layout.bodyBottom) return { kind: 'none' };
  const { first, last } = visibleBarsRange(layout, scrollTop);
  for (let i = last - 1; i >= first; i--) {
    const item = model.rows[i]!;
    const baseY = layout.bodyTop + layout.rowTops[i]! - scrollTop;
    const rowH = layout.rowHeights[i]!;
    const y = paintedRowY(item.key, baseY, glide, now, glideDuration);
    if (point.y < y || point.y >= y + rowH) continue;
    if (item.kind === 'section') return { kind: 'none' };
    if (item.kind === 'add-region') return { kind: 'add-region', rowKey: item.key };
    if (item.region !== undefined && removeShown?.has(item.key)) {
      const inside = (box: { readonly x: number; readonly y: number; readonly width: number; readonly height: number }) =>
        point.x >= box.x && point.x <= box.x + box.width && point.y >= box.y && point.y <= box.y + box.height;
      if (inside(rowRemoveBox(layout.nameX + layout.nameWidth, y + rowH / 2))) return { kind: 'remove-region', rowKey: item.key };
      if (inside(rowGripBox(layout.nameX, y + rowH / 2))) return { kind: 'grip-region', rowKey: item.key };
    }
    if (model.form === 'two-sided' && layout.twoSidedLayout === 'split-sides' && item.rightSide && point.x >= layout.centerX) {
      return { kind: 'right-row', rowKey: item.rightSide.key, side: 'right' };
    }
    let segmentKey: string | undefined;
    if ((model.form === 'share' || model.form === 'stacked') && item.segments?.length && point.x >= layout.plotLeft && point.x <= layout.plotRight) {
      const active = item.segments.filter(s => s.share > 0);
      const rel = (point.x - layout.plotLeft) / Math.max(1, layout.plotWidth);
      let acc = 0;
      for (let s = 0; s < active.length; s++) {
        acc += active[s]!.share;
        if (rel <= acc || s === active.length - 1) {
          segmentKey = active[s]!.key;
          break;
        }
      }
    }
    return {
      kind: 'row',
      rowKey: item.key,
      ...(segmentKey ? { segmentKey } : {}),
      ...(model.form === 'two-sided' ? { side: point.x < layout.centerX ? 'left' : 'right' } : {}),
    };
  }
  return { kind: 'none' };
}

export function collectVisibleBarsIcons(model: BarsModel, layout: BarsLayout, scrollTop: number): string[] {
  const { first, last } = visibleBarsRange(layout, scrollTop);
  const items: ArtUrlInput[] = [];
  for (let i = first; i < last; i++) {
    const item = model.rows[i]!;
    if (item.kind !== 'data') continue;
    items.push(item.media, item.rightSide?.media, item.readout?.icon);
  }
  for (const side of model.captionSides ?? []) items.push(side.media);
  return collectArtUrls(items);
}
