import { LABEL_WEIGHT, VALUE_WEIGHT } from '../charts/draw.ts';
import { NOT_APPLICABLE_LABEL, NOT_RECORDED_LABEL, PARTIAL_LABEL, type FormattedValue } from './valueFormat.ts';

export type ValueCanvasContext = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

export interface ValueCanvasTheme {
  readonly text: string;
  readonly muted: string;
  readonly line: string;
  readonly hatch: string;
  readonly hatchEdge?: string;
  readonly font: string;
  readonly fontSize: { readonly small: number; readonly body: number };
  readonly hatchSpacing: number;
}

export const MISSING_BOX_SIZE = 10;
export const MISSING_BOX_GAP = 6;
export const PARTIAL_TAB_WIDTH = 6;
export const PARTIAL_TAB_HEIGHT = 13;
export const PARTIAL_TAB_GAP = 5;
export const PARTIAL_WORD_GAP = 4;

export function drawHatchLines(
  ctx: ValueCanvasContext,
  x: number,
  y: number,
  width: number,
  height: number,
  theme: Pick<ValueCanvasTheme, 'hatch' | 'hatchSpacing'>,
): void {
  ctx.save();
  ctx.strokeStyle = theme.hatch;
  ctx.globalAlpha = 0.55;
  ctx.lineWidth = 1.2;
  const step = Math.max(3, theme.hatchSpacing);
  for (let at = x - height; at < x + width; at += step) {
    ctx.beginPath();
    ctx.moveTo(at, y + height);
    ctx.lineTo(at + height, y);
    ctx.stroke();
  }
  ctx.restore();
}

export function drawHatchRect(
  ctx: ValueCanvasContext,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
  theme: Pick<ValueCanvasTheme, 'hatch' | 'hatchEdge' | 'line' | 'hatchSpacing'>,
): void {
  if (width <= 0 || height <= 0) return;
  ctx.save();
  ctx.beginPath();
  ctx.roundRect(x, y, width, height, radius);
  ctx.clip();
  drawHatchLines(ctx, x, y, width, height, theme);
  ctx.strokeStyle = theme.hatchEdge ?? theme.hatch;
  ctx.globalAlpha = 0.35;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.roundRect(x + 0.5, y + 0.5, Math.max(0, width - 1), Math.max(0, height - 1), radius);
  ctx.stroke();
  ctx.restore();
}

export function drawFormattedValue(
  ctx: ValueCanvasContext,
  formatted: FormattedValue,
  bounds: { readonly left: number; readonly right: number; readonly middle: number; readonly endAligned: boolean; readonly muted?: boolean; readonly compactPartial?: boolean },
  theme: ValueCanvasTheme,
  measure: (text: string, font: string) => number,
  fit: (text: string, font: string, maxWidth: number) => string,
): void {
  const { left, right, middle, endAligned, muted = false } = bounds;
  const available = Math.max(0, right - left);
  const bodyFont = `${VALUE_WEIGHT} ${theme.fontSize.body}px ${theme.font}`;
  const smallFont = `${LABEL_WEIGHT} ${theme.fontSize.small}px ${theme.font}`;
  ctx.textBaseline = 'middle';

  if (formatted.kind === 'pending') {
    ctx.font = smallFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = endAligned ? 'right' : 'left';
    ctx.fillText(fit(formatted.label, smallFont, available), endAligned ? right : left, middle);
    return;
  }

  if (formatted.kind === 'notRecorded') {
    const label = formatted.label || NOT_RECORDED_LABEL;
    if (formatted.gameWindow) {
      ctx.font = smallFont;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = endAligned ? 'right' : 'left';
      ctx.fillText(fit(label, smallFont, available), endAligned ? right : left, middle);
      return;
    }
    const labelWidth = measure(label, smallFont);
    const fullWidth = MISSING_BOX_SIZE + MISSING_BOX_GAP + labelWidth;
    const showWords = available >= fullWidth;
    const totalWidth = showWords ? fullWidth : MISSING_BOX_SIZE;
    const boxX = endAligned ? Math.max(left, right - totalWidth) : left;
    const boxY = Math.round(middle - MISSING_BOX_SIZE / 2);
    drawHatchRect(ctx, boxX, boxY, MISSING_BOX_SIZE, MISSING_BOX_SIZE, 2, theme);
    if (showWords) {
      ctx.font = smallFont;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'left';
      ctx.fillText(label, boxX + MISSING_BOX_SIZE + MISSING_BOX_GAP, middle);
    }
    return;
  }

  if (formatted.kind === 'notApplicable') {
    ctx.font = bodyFont;
    ctx.fillStyle = theme.muted;
    ctx.textAlign = endAligned ? 'right' : 'left';
    ctx.fillText(NOT_APPLICABLE_LABEL, endAligned ? right : left, middle);
    return;
  }

  if (formatted.kind === 'partial') {
    const valueWidth = measure(formatted.text, bodyFont);
    const wordWidth = measure(PARTIAL_LABEL, smallFont);
    const tabSpan = PARTIAL_TAB_GAP + PARTIAL_TAB_WIDTH;
    const wordSpan = PARTIAL_WORD_GAP + wordWidth;
    const showWord = !bounds.compactPartial && valueWidth + tabSpan + wordSpan <= available;
    const total = valueWidth + tabSpan + (showWord ? wordSpan : 0);
    const startX = endAligned ? Math.max(left, right - total) : left;
    const maxValueWidth = Math.max(1, available - tabSpan - (showWord ? wordSpan : 0));
    const shownText = valueWidth <= maxValueWidth ? formatted.text : fit(formatted.text, bodyFont, maxValueWidth);
    const drawnWidth = shownText === formatted.text ? valueWidth : measure(shownText, bodyFont);
    ctx.font = bodyFont;
    ctx.fillStyle = muted ? theme.muted : theme.text;
    ctx.textAlign = 'left';
    ctx.fillText(shownText, startX, middle);
    const tabX = startX + drawnWidth + PARTIAL_TAB_GAP;
    const tabY = Math.round(middle - PARTIAL_TAB_HEIGHT / 2);
    drawHatchRect(ctx, tabX, tabY, PARTIAL_TAB_WIDTH, PARTIAL_TAB_HEIGHT, 1.5, theme);
    if (showWord) {
      ctx.font = smallFont;
      ctx.fillStyle = theme.muted;
      ctx.textAlign = 'left';
      ctx.fillText(PARTIAL_LABEL, tabX + PARTIAL_TAB_WIDTH + PARTIAL_WORD_GAP, middle);
    }
    return;
  }

  ctx.font = bodyFont;
  ctx.fillStyle = muted ? theme.muted : theme.text;
  ctx.textAlign = endAligned ? 'right' : 'left';
  ctx.fillText(fit(formatted.text, bodyFont, available), endAligned ? right : left, middle);
}
