import { LABEL_WEIGHT } from '../../charts/draw.ts';
import { textWidth } from '../../charts/textWidth.ts';
import { splitBarWidths } from '../../draw/kit.ts';
import type { BarsPlaybackForm } from '../contract.ts';
import type { LaneBinding, LaneFamily, LaneFrame } from '../laneRuntime.ts';
import type { MotionSampleDiagnostic } from '../motion.ts';
import { glideDurationForSpeed } from '../table/painter.ts';
import { barsAuditWindow, barsRangeForBand, barsScrollHeight, layoutFamilyBars, visibleBarsRange, type BarsLayout } from './layout.ts';
import { assertBarsModelValid, barsLabelText, deriveBarsAudit, deriveBarsMirror, deriveBarsTooltip, formatBarsValueField, isSingleValueOrdered, type BarsDataRow, type BarsModel, type BarsRow, type BarsValueField } from './model.ts';
import { collectVisibleBarsIcons, drawFamilyBars, easeBarFraction, hitFamilyBars, orderedBarGeometry, type BarsEasedRow, type BarsGlideOffset } from './painter.ts';
import { hitPickList, pickAudit, pickedKey, pickNav, pickTooltip } from './pickList.ts';
import { barsRegionRows } from './shareRows.ts';
import { regionHitKey } from '../regions/regionRows.ts';
import type { ValueFormat } from '../valueFormat.ts';

export interface FamilyBarsBinding<TOptions = unknown> extends LaneBinding<TOptions> {
  readonly family: 'bars';
  readonly width: number;
  readonly height: number;
  readonly reducedMotion?: boolean;
  readonly pauseFlush?: number;
}

type BarsFrame = LaneFrame<FamilyBarsBinding, BarsModel, BarsLayout> & {
  readonly playbackValues?: ReadonlyMap<string, BarsEasedRow>;
};

interface Glide {
  readonly duration: number;
  readonly offsets: ReadonlyMap<string, BarsGlideOffset>;
  readonly moving: boolean;
}

const STILL: Glide = { duration: 0, offsets: new Map(), moving: false };
const glides = new WeakMap<BarsLayout, { readonly continuity: number; readonly scrollTop: number; readonly offsets: ReadonlyMap<string, BarsGlideOffset> }>();

function glideOf(frame: BarsFrame): Glide {
  if (frame.exactPlayback) return STILL;
  const { model, layout, before, binding } = frame;
  const duration = model.animateOrder ? glideDurationForSpeed(frame.speed, binding.reducedMotion, binding.motion?.glideDurationMs) : 0;
  if (duration === 0 || !before || before.hour === frame.hour || frame.now - frame.changedAt >= duration) return { ...STILL, duration };
  let held = glides.get(layout);
  if (!held || held.continuity !== frame.continuity || held.scrollTop !== binding.scrollTop) {
    const visible = visibleBarsRange(layout, binding.scrollTop, 0);
    const bandTop = binding.scrollTop - layout.bodyHeight;
    const bandBottom = binding.scrollTop + layout.bodyHeight * 2;
    const priorRange = barsRangeForBand(before.layout, bandTop, bandBottom, 0);
    const prior = new Map<string, number>();
    for (let i = priorRange.first; i < priorRange.last; i++) {
      prior.set(before.model.rows[i]!.key, before.layout.rowTops[i]!);
    }
    const offsets = new Map<string, BarsGlideOffset>();
    for (let i = visible.first; i < visible.last; i++) {
      const key = model.rows[i]!.key;
      const beforeRowTop = prior.get(key);
      const nextRowTop = layout.rowTops[i]!;
      if (beforeRowTop !== undefined && Math.abs(beforeRowTop - nextRowTop) > 1 && Math.abs(beforeRowTop - nextRowTop) <= layout.bodyHeight) {
        offsets.set(key, { fromY: layout.bodyTop + beforeRowTop - binding.scrollTop, startedAt: frame.changedAt });
      }
    }
    glides.set(layout, held = { continuity: frame.continuity, scrollTop: binding.scrollTop, offsets });
  }
  return { duration, offsets: held.offsets, moving: held.offsets.size > 0 };
}

function isMeasuredField(field: BarsValueField | undefined): boolean {
  if (!field) return true;
  const kind = formatBarsValueField(field).kind;
  return kind !== 'notRecorded' && kind !== 'notApplicable';
}

function rowFractions(row: BarsDataRow, form: BarsModel['form'], layout: BarsLayout): BarsEasedRow {
  const scaleMax = Math.max(1e-6, layout.scaleMax);
  const scaleLeft = Math.max(1e-6, layout.scaleLeftMax);
  const scaleRight = Math.max(1e-6, layout.scaleRightMax);
  const out: {
    numericFrac?: number;
    leftFrac?: number;
    rightFrac?: number;
    rightSideFrac?: number;
    targetFrac?: number;
    segmentShares?: Record<string, number>;
  } = {};
  if (form === 'share' || form === 'stacked') {
    if (isMeasuredField(row.value) && row.segments?.length) {
      const shares: Record<string, number> = {};
      for (const seg of row.segments) {
        if (!isMeasuredField({ value: seg.value })) continue;
        shares[seg.key] = seg.share;
      }
      if (Object.keys(shares).length > 0) {
        out.segmentShares = shares;
      }
    }
  } else if (form === 'two-sided' && layout.twoSidedLayout === 'split-sides') {
    if (isMeasuredField(row.value) && row.numeric !== undefined && row.numeric !== null) {
      out.numericFrac = row.numeric / scaleLeft;
    }
    if (row.rightSide && isMeasuredField(row.rightSide.value) && row.rightSide.numeric !== undefined && row.rightSide.numeric !== null) {
      out.rightSideFrac = row.rightSide.numeric / scaleRight;
    }
  } else if (form === 'two-sided') {
    const leftScale = layout.twoSidedLayout === 'stacked-pair' ? scaleMax : scaleLeft;
    const rightScale = layout.twoSidedLayout === 'stacked-pair' ? scaleMax : scaleRight;
    if (row.left && isMeasuredField(row.left.value) && row.left.numeric !== undefined && row.left.numeric !== null) {
      out.leftFrac = row.left.numeric / leftScale;
    }
    if (row.right && isMeasuredField(row.right.value) && row.right.numeric !== undefined && row.right.numeric !== null) {
      out.rightFrac = row.right.numeric / rightScale;
    }
  } else {
    if (isMeasuredField(row.value) && row.numeric !== undefined && row.numeric !== null) {
      out.numericFrac = row.numeric / scaleMax;
    }
    if (row.target && row.target.numeric > 0) {
      out.targetFrac = row.target.numeric / scaleMax;
    }
  }
  return out;
}

function easedBarsOf(frame: BarsFrame): { readonly map?: ReadonlyMap<string, BarsEasedRow>; readonly moving: boolean } {
  const motion = frame.binding.motion;
  if (!motion?.easeValues || frame.binding.reducedMotion) return { moving: false };
  const duration = glideDurationForSpeed(frame.speed, false, motion.glideDurationMs);
  if (duration <= 0 || !frame.before || frame.before.hour === frame.hour || frame.before.model.form !== frame.model.form) {
    return { moving: false };
  }
  const progress = (frame.now - frame.changedAt) / duration;
  if (progress >= 1) return { moving: false };
  const prior = new Map<string, BarsEasedRow>();
  for (const item of frame.before.model.rows) {
    if (item.kind === 'data') {
      prior.set(item.key, rowFractions(item, frame.before.model.form, frame.before.layout));
    }
  }
  const eased = new Map<string, BarsEasedRow>();
  let moving = false;
  for (const item of frame.model.rows) {
    if (item.kind !== 'data') continue;
    const prev = prior.get(item.key);
    if (!prev) continue;
    const cur = rowFractions(item, frame.model.form, frame.layout);
    const next: {
      numericFrac?: number;
      leftFrac?: number;
      rightFrac?: number;
      rightSideFrac?: number;
      targetFrac?: number;
      segmentShares?: Record<string, number>;
    } = {};
    const step = (p: number | undefined, c: number | undefined): number | undefined => {
      if (p === undefined || c === undefined) return c;
      if (Math.abs(p - c) > 1e-4) moving = true;
      return easeBarFraction(p, c, progress) ?? c;
    };
    const nf = step(prev.numericFrac, cur.numericFrac);
    if (nf !== undefined) next.numericFrac = nf;
    const lf = step(prev.leftFrac, cur.leftFrac);
    if (lf !== undefined) next.leftFrac = lf;
    const rf = step(prev.rightFrac, cur.rightFrac);
    if (rf !== undefined) next.rightFrac = rf;
    const rsf = step(prev.rightSideFrac, cur.rightSideFrac);
    if (rsf !== undefined) next.rightSideFrac = rsf;
    const tf = step(prev.targetFrac, cur.targetFrac);
    if (tf !== undefined) next.targetFrac = tf;
    if (cur.segmentShares) {
      if (prev.segmentShares) {
        const shares: Record<string, number> = {};
        for (const [segKey, curShare] of Object.entries(cur.segmentShares)) {
          const prevShare = prev.segmentShares[segKey];
          const val = step(prevShare, curShare);
          shares[segKey] = val ?? curShare;
        }
        next.segmentShares = shares;
      } else {
        next.segmentShares = cur.segmentShares;
      }
    }
    eased.set(item.key, next);
  }
  return moving ? { map: eased, moving: true } : { moving: false };
}

function inspected(inspect: string | null): { readonly key: string; readonly side?: 'right' } | null {
  if (inspect?.startsWith('row:')) return { key: inspect.slice(4) };
  if (inspect?.startsWith('right:')) return { key: inspect.slice(6), side: 'right' };
  if (inspect?.startsWith('pick:')) return { key: inspect };
  return null;
}

function sameFormat(first: ValueFormat | undefined, second: ValueFormat | undefined): boolean {
  return first === second || (typeof first === 'object' && typeof second === 'object'
    && first.decimals === second.decimals && first.maxDecimals === second.maxDecimals
    && first.fixedDecimals === second.fixedDecimals && first.signed === second.signed
    && first.percent === second.percent && first.unit === second.unit);
}

function measuredTarget(target: NonNullable<BarsDataRow['target']>): boolean {
  return Number.isFinite(target.numeric) && target.numeric > 0
    && (target.value.kind === 'known' || target.value.kind === 'zero') && typeof target.value.value !== 'string';
}

function compatibleSegments(first: NonNullable<BarsDataRow['segments']>, second: NonNullable<BarsDataRow['segments']>): boolean {
  return first.length === second.length && first.every((segment, i) => {
    const next = second[i]!;
    return segment.key === next.key && segment.label === next.label && segment.color === next.color
      && Boolean(segment.hatched) === Boolean(next.hatched) && sameFormat(segment.format, next.format)
      && segment.share > 0 && next.share > 0
      && segment.value.kind === 'known' && next.value.kind === 'known';
  });
}

function matchingRows(first: BarsModel, second: BarsModel, diagnostic?: MotionSampleDiagnostic): readonly BarsRow[] | null {
  if (first === second) return second.rows;
  if (first.showAxis !== second.showAxis || first.rowHeight !== second.rowHeight || first.barHeight !== second.barHeight
    || first.caption !== second.caption || first.scaleUnit !== second.scaleUnit || !sameFormat(first.scaleFormat, second.scaleFormat)
    || first.leftHeader !== second.leftHeader || first.rightHeader !== second.rightHeader || first.readoutHeader !== second.readoutHeader) return null;
  if (first.rows.length !== second.rows.length) {
    if (diagnostic) diagnostic.rowChange = 'membership';
    return null;
  }
  const reordered = first.rows.some((row, i) => row.key !== second.rows[i]!.key);
  let endpoints = second.rows;
  if (reordered) {
    const byKey = new Map<string, { row: BarsRow; section: number }>();
    let section = -1;
    for (const [i, row] of second.rows.entries()) {
      if (row.kind === 'section') section = i;
      byKey.set(row.key, { row, section });
    }
    const matched: BarsRow[] = [];
    let sectionMismatch = false;
    section = -1;
    for (const [i, row] of first.rows.entries()) {
      if (row.kind === 'section') section = i;
      const endpoint = byKey.get(row.key);
      if (!endpoint) {
        if (diagnostic) diagnostic.rowChange = 'membership';
        return null;
      }
      sectionMismatch ||= section !== endpoint.section;
      matched.push(endpoint.row);
      byKey.delete(row.key);
    }
    if (sectionMismatch) return null;
    endpoints = matched;
  }
  for (const [i, row] of first.rows.entries()) {
    const endpoint = endpoints[i]!;
    if (row.kind !== endpoint.kind) return null;
    if (row.kind !== 'data' || endpoint.kind !== 'data') {
      if (row.label !== endpoint.label) {
        if (diagnostic) diagnostic.rowChange = 'label';
        return null;
      }
      if (reordered && row.kind === 'section' && endpoint.kind === 'section' && row.count !== endpoint.count) return null;
      continue;
    }
    if (row.label?.kind !== endpoint.label?.kind || barsLabelText(row.label) !== barsLabelText(endpoint.label)) {
      if (diagnostic) diagnostic.rowChange = 'label';
      return null;
    }
    if (row.subject !== endpoint.subject || row.group !== endpoint.group || row.region !== endpoint.region
      || row.media?.kind !== endpoint.media?.kind || !sameFormat(row.value?.format, endpoint.value?.format)
      || Boolean(row.target) !== Boolean(endpoint.target) || !sameFormat(row.target?.format, endpoint.target?.format)
      || Boolean(row.readout) !== Boolean(endpoint.readout)) return null;
  }
  if (diagnostic && reordered) diagnostic.rowChange = 'reorder';
  return endpoints;
}

function orderedMotion(frame: BarsFrame, next: BarsModel, fraction: number, nextLayout: () => BarsLayout, form: BarsPlaybackForm, diagnostic?: MotionSampleDiagnostic): BarsFrame | false | null {
  const current = frame.model;
  const endpoints = matchingRows(current, next, diagnostic);
  if (!endpoints) return null;
  const start = frame.layout;
  const endLayout = nextLayout();
  const holdPlot = start.showReadout && endLayout.plotRight > start.plotRight;
  const endPlotLeft = holdPlot ? start.plotLeft : endLayout.plotLeft;
  const endPlotWidth = holdPlot ? start.plotWidth : endLayout.plotWidth;
  const playbackValues = new Map<string, BarsEasedRow>();
  const interpolate = (from: number, to: number) => from + (to - from) * fraction;
  let changed = start.plotLeft !== endPlotLeft
    || (start.showAxis && (start.plotWidth !== endPlotWidth || start.scaleMax !== endLayout.scaleMax));
  for (let i = 0; i < current.rows.length; i++) {
    const row = current.rows[i]!;
    const end = endpoints[i]!;
    if (row.kind === 'section') continue;
    if (row.kind !== 'data' || end.kind !== 'data' || row.numeric == null || end.numeric == null
      || !row.value || !end.value
      || (row.value.value.kind !== 'zero' && row.value.value.kind !== 'known')
      || (end.value.value.kind !== 'zero' && end.value.value.kind !== 'known')) return null;
    if (form === 'ordered-segments' && (row.segmentsIncomplete || end.segmentsIncomplete || (row.numeric < 0) !== (end.numeric < 0))) return null;
    if ((row.target && !measuredTarget(row.target)) || (end.target && !measuredTarget(end.target))) return null;
    const from = orderedBarGeometry(start.plotWidth, row.numeric / start.scaleMax, row.target && row.target.numeric / start.scaleMax);
    const to = orderedBarGeometry(endPlotWidth, end.numeric / endLayout.scaleMax, end.target && end.target.numeric / endLayout.scaleMax);
    const firstSegments = row.segments;
    const lastSegments = end.segments;
    let segmentWidths: readonly number[] | undefined;
    if (firstSegments?.length || lastSegments?.length) {
      if (!firstSegments || !lastSegments || !compatibleSegments(firstSegments, lastSegments)) return null;
      const startWidths = splitBarWidths(start.plotLeft, from.fill, firstSegments.map(segment => segment.share));
      const endWidths = splitBarWidths(endPlotLeft, to.fill, lastSegments.map(segment => segment.share));
      segmentWidths = startWidths.map((width, i) => interpolate(width, endWidths[i]!));
      changed ||= startWidths.some((width, i) => width !== endWidths[i]);
    }
    playbackValues.set(row.key, { segmentWidths, orderedGeometry: {
      fill: interpolate(from.fill, to.fill),
      gapStart: interpolate(from.gapStart, to.gapStart),
      gapEnd: interpolate(from.gapEnd, to.gapEnd),
      gapStroke: interpolate(from.gapStroke, to.gapStroke),
      labelEnd: interpolate(from.labelEnd, to.labelEnd),
    } });
    changed ||= from.fill !== to.fill || from.gapStart !== to.gapStart || from.gapEnd !== to.gapEnd
      || from.gapStroke !== to.gapStroke || from.labelEnd !== to.labelEnd;
  }
  if (!changed) return false;
  const plotWidth = interpolate(start.plotWidth, endPlotWidth);
  const plotLeft = interpolate(start.plotLeft, endPlotLeft);
  const pixelsPerUnit = interpolate(start.plotWidth / start.scaleMax, endPlotWidth / endLayout.scaleMax);
  return {
    ...frame,
    layout: { ...start, plotWidth, plotLeft, plotRight: plotLeft + plotWidth, scaleMax: plotWidth / pixelsPerUnit },
    playbackValues,
  };
}

export const barsLane: LaneFamily<FamilyBarsBinding, BarsModel, BarsLayout> = {
  family: 'bars',
  boundary(model, hour) {
    assertBarsModelValid(model, hour);
    return model.nextChangeAt ?? Infinity;
  },
  redraw: () => Infinity,
  playback: {
    enabled: binding => !binding.reducedMotion && !binding.motion,
    supports: isSingleValueOrdered,
    unavailable: model => model.emptyKind === 'unavailable',
    frame: orderedMotion,
  },
  status: model => (model.rows.length > 0 || model.empty !== undefined ? 'picture' : 'scene'),
  view: [],
  layout(model, _binding, surface, theme, measure) {
    const font = `${LABEL_WEIGHT} ${theme.fontSize.body}px ${theme.font}`;
    return layoutFamilyBars(model, surface.width, surface.height, theme, text => {
      measure.font = font;
      return textWidth(measure, text);
    });
  },
  paint(ctx, frame: BarsFrame) {
    const glide = glideOf(frame);
    const eased = easedBarsOf(frame);
    const values = frame.playbackValues ?? eased.map;
    drawFamilyBars(ctx, {
      model: frame.model,
      layout: frame.layout,
      theme: frame.theme,
      scrollTop: frame.binding.scrollTop,
      now: frame.now,
      glideDuration: glide.duration,
      glide: glide.offsets,
      ...(values ? { easedValues: values } : {}),
      artCache: frame.art,
      inspectedKey: inspected(frame.inspect)?.key ?? null,
      selectedKey: frame.binding.selectedKey,
      focusedKey: frame.binding.focusedKey,
    });
    return glide.moving || eased.moving;
  },
  hit(frame, point) {
    if (frame.model.pickList) {
      const picked = hitPickList(frame.model.pickList, frame.layout, { x: point[0], y: point[1] });
      if (picked) return picked;
      if (point[0] < frame.layout.bodyLeft) return null;
    }
    const glide = glideOf(frame);
    const shown = new Set<string>();
    const hovered = inspected(frame.inspect)?.key;
    if (hovered) shown.add(hovered);
    if (frame.binding.focusedKey) shown.add(frame.binding.focusedKey);
    const res = hitFamilyBars(frame.layout, frame.model, { x: point[0], y: point[1] }, frame.binding.scrollTop, glide.moving ? glide.offsets : undefined, frame.now, glide.duration, shown);
    if (res.kind === 'none' || !res.rowKey) return null;
    if (res.kind === 'add-region') return regionHitKey('add', res.rowKey);
    if (res.kind === 'remove-region') return regionHitKey('remove', res.rowKey);
    if (res.kind === 'grip-region') return regionHitKey('grip', res.rowKey);
    if (res.kind === 'right-row') return `right:${res.rowKey}`;
    return `row:${res.rowKey}`;
  },
  tooltip(frame, key, focus) {
    const target = focus ? { key } : inspected(key);
    if (!target) return null;
    const picked = pickedKey(target.key);
    const dataHour = frame.model.dataHours?.length === 1 ? frame.model.dataHours[0]! : undefined;
    const tipHour = dataHour ?? frame.model.hour;
    if (picked !== null) {
      const tip = frame.model.pickList ? pickTooltip(frame.model.pickList, picked, tipHour) : null;
      if (!tip) return null;
      const fixedHour = dataHour ?? (tip.hour !== undefined && tip.hour !== frame.model.hour ? tip.hour : undefined);
      return { rowKey: target.key, payload: tip, ...(fixedHour !== undefined ? { hour: fixedHour } : {}) };
    }
    const payload = deriveBarsTooltip(frame.model, target.key, target.side);
    if (!payload) return null;
    const fixedHour = dataHour ?? (payload.hour !== undefined && payload.hour !== frame.model.hour ? payload.hour : undefined);
    return { rowKey: target.key, payload, ...(fixedHour !== undefined ? { hour: fixedHour } : {}) };
  },
  structure({ model, layout, exactPlayback }) {
    const regions = barsRegionRows(model, layout);
    return {
      ...(regions ? { regions } : {}),
      meta: {
        form: model.form,
        count: model.rows.length,
        totalHeight: layout.totalHeight,
        scrollHeight: barsScrollHeight(layout),
        rowViewport: { top: layout.bodyTop, height: layout.bodyHeight },
        bodyHeight: layout.bodyHeight,
        rowHeight: layout.dataRowHeight,
        bodyTop: layout.bodyTop,
        summaryText: model.summaryText ?? null,
        empty: model.empty ?? null,
        emptyDetail: model.emptyDetail ?? null,
        emptyKind: model.emptyKind ?? 'empty',
        ...(exactPlayback && model.emptyKind === 'unavailable' ? { playbackUnavailable: true } : {}),
        notes: model.notes ?? [],
        warnings: model.warnings ?? [],
      },
      nav: [
        ...(model.pickList && layout.pickWidth > 0 ? pickNav(model.pickList) : []),
        ...model.rows.flatMap((row, index) => (row.kind === 'section' ? [] : [[row.key, row.kind === 'data' ? barsLabelText(row.label) : row.label, index, -1, '', row.kind === 'data' ? (row.meta ?? null) : null, layout.rowTops[index]!, layout.rowHeights[index]!] as const])),
      ],
    };
  },
  mirror(frame) {
    const shown = visibleBarsRange(frame.layout, frame.binding.scrollTop);
    return deriveBarsMirror(frame.model, { first: shown.first, count: Math.max(0, shown.last - shown.first) }, frame.binding.focusedKey);
  },
  audit: frame => [...(frame.model.pickList && frame.layout.pickWidth > 0 ? pickAudit(frame.model.pickList) : []), ...deriveBarsAudit(frame.model, { window: barsAuditWindow(frame.layout, frame.binding.scrollTop) })],
  visibleArt: frame => collectVisibleBarsIcons(frame.model, frame.layout, frame.binding.scrollTop),
};
