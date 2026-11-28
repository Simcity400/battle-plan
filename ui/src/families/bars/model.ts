import { artTooltipItem } from '../art/adapters.ts';
import type { ActiveRegions } from '../../generated/dashboard';
import { auditCell, auditRow, type FamilyAuditRow } from '../auditRow.ts';
import { flagArtHandle, flagChoiceArt } from '../art/provider.ts';
import { artHandleKey, type ArtResult } from '../art/types.ts';
import { canonicalAuditValue } from '../audit.ts';
import type { BarsPlaybackForm, BoundedReadView, FamilyProjection, MirrorModel, MirrorRow } from '../contract.ts';
import type { FamilyActionPayload, FamilyTooltipPayload, TooltipListItem } from '../host/tooltips.ts';
import { resolveName } from '../table/identityCell.ts';
import { assertValue, hasMeasuredValue, type Value } from '../value.ts';
import { formatShare, formatValueState, notRecordedLabel, type ValueFormat } from '../valueFormat.ts';

export type BarsForm = 'ordered' | 'two-sided' | 'share' | 'net' | 'stacked';

export type BarsTwoSidedLayout = 'paired' | 'split-sides' | 'stacked-pair';

export function barsLabelMissing(label: Value<string> | undefined): boolean {
  return label !== undefined && label.kind === 'notRecorded';
}

export function barsLabelMuted(label: Value<string> | undefined): boolean {
  return label !== undefined && (label.kind === 'notRecorded' || label.kind === 'notApplicable' || label.kind === 'pending');
}

export function barsLabelText(label: Value<string> | undefined, figure = 'Name', unrecordedText?: string): string {
  if (label === undefined) return '';
  const formatted = formatValueState(label, undefined, { figure });
  if (formatted.kind === 'notRecorded') return unrecordedText ?? notRecordedLabel(figure);
  if (formatted.kind === 'notApplicable' || formatted.kind === 'pending') return formatted.label;
  return formatted.text;
}

export function barsLabelSpoken(label: Value<string> | undefined, figure = 'Name'): string {
  if (label === undefined) return '';
  return formatValueState(label, undefined, { figure }).spoken;
}

export function barsLabelAudit(label: Value<string> | undefined): string {
  return label === undefined ? '' : canonicalAuditValue(label);
}

export interface BarsRowMedia {
  readonly kind: 'none' | 'flag' | 'art';
  readonly art: ArtResult;
  readonly tag?: string;
  readonly resolvedName?: string;
  readonly dimmed?: boolean;
}

export interface CountryRowPresentation {
  readonly label: Value<string>;
  readonly color?: string;
  readonly media: BarsRowMedia;
  readonly flagKey?: string;
}

export function countryRowPresentation(
  readView: BoundedReadView,
  tag: string,
  fallbackColor?: string,
  dimmed?: boolean,
  atHour?: number,
): CountryRowPresentation {
  const { value, who: identity } = resolveName(readView, 'country', tag, atHour === undefined ? {} : { atHour });
  const color = identity.colour?.status === 'ready' ? identity.colour.value : fallbackColor;
  const art: ArtResult = flagChoiceArt(readView, identity.flagChoice);
  const flagKey = identity.flagChoice?.status === 'ready' ? artHandleKey(flagArtHandle(identity.flagChoice.value, 'panel')) : undefined;
  return {
    label: value,
    color,
    media: { kind: 'flag', art, tag, dimmed },
    ...(flagKey ? { flagKey } : {}),
  };
}

export interface BarsSegment {
  readonly key: string;
  readonly label: string;
  readonly share: number;
  readonly value: Value;
  readonly format?: ValueFormat;
  readonly color: string;
  readonly hatched?: boolean;
  readonly inlineLabel?: string;
}

export interface BarsValueField {
  readonly value: Value;
  readonly format?: ValueFormat;
  readonly unit?: string;
  readonly compact?: boolean;
  readonly figure?: string;
  readonly label?: string;
  readonly caption?: string;
  readonly tone?: 'default' | 'shortage' | 'muted' | 'positive';
}

export interface BarsNeedTarget {
  readonly numeric: number;
  readonly value: Value;
  readonly format?: ValueFormat;
  readonly shortfall?: Value;
  readonly shortfallNumeric?: number;
}

export interface BarsSideMeasure {
  readonly value: BarsValueField;
  readonly numeric: number | null;
  readonly color?: string;
  readonly segments?: readonly BarsSegment[];
}

export interface BarsOppositeRow {
  readonly key: string;
  readonly subject?: string;
  readonly label: Value<string>;
  readonly media?: BarsRowMedia;
  readonly color?: string;
  readonly value: BarsValueField;
  readonly numeric: number | null;
  readonly tooltip?: FamilyTooltipPayload;
}

export interface BarsReadout {
  readonly primary: string;
  readonly spoken?: string;
  readonly secondary?: string;
  readonly tone?: 'default' | 'shortage' | 'muted';
  readonly shortfall?: string;
  readonly icon?: ArtResult;
  readonly iconLabel?: string;
}

export interface BarsStackedPair {
  readonly topLabel: string;
  readonly topValue: Value;
  readonly bottomLabel: string;
  readonly bottomValue: Value;
  readonly format?: ValueFormat;
}

export interface BarsDataRow {
  readonly kind: 'data';
  readonly key: string;
  readonly subject?: string;
  readonly label?: Value<string>;
  readonly media?: BarsRowMedia;
  readonly color?: string;
  readonly muted?: boolean;
  readonly region?: number;
  readonly group?: 'picked' | 'auto';
  readonly value?: BarsValueField;
  readonly numeric?: number | null;
  readonly target?: BarsNeedTarget;
  readonly segments?: readonly BarsSegment[];
  readonly segmentsIncomplete?: true;
  readonly left?: BarsSideMeasure;
  readonly right?: BarsSideMeasure;
  readonly rightSide?: BarsOppositeRow;
  readonly readout?: BarsReadout;
  readonly pair?: BarsStackedPair;
  readonly tooltip?: FamilyTooltipPayload;
  readonly action?: Omit<FamilyActionPayload, 'hour'>;
  readonly meta?: unknown;
}

export interface BarsSectionRow {
  readonly kind: 'section';
  readonly key: string;
  readonly label: string;
  readonly count?: number;
}

export interface BarsAddRegionRow {
  readonly kind: 'add-region';
  readonly key: string;
  readonly label: string;
}

export type BarsRow = BarsSectionRow | BarsDataRow | BarsAddRegionRow;

export interface BarsLegendItem {
  readonly key: string;
  readonly label: string;
  readonly color: string;
  readonly value?: string;
  readonly share?: string;
  readonly hatched?: boolean;
}

export interface BarsPickSide {
  readonly fraction: number | null;
  readonly color?: string;
}

export interface BarsPickRow {
  readonly key: string;
  readonly label: string;
  readonly left: BarsPickSide;
  readonly right: BarsPickSide;
  readonly divided?: boolean;
  readonly tooltip?: FamilyTooltipPayload;
}

export interface BarsPickList {
  readonly rows: readonly BarsPickRow[];
  readonly selectedKey: string | null;
}

export interface BarsCaptionSide {
  readonly media: BarsRowMedia;
  readonly label: string;
  readonly value: string;
}

export interface BarsModel {
  readonly form: BarsForm;
  readonly pickList?: BarsPickList;
  readonly captionSides?: readonly BarsCaptionSide[];
  readonly hour: number;
  readonly rows: readonly BarsRow[];
  readonly scaleMax?: number | null;
  readonly scaleLeftMax?: number | null;
  readonly scaleRightMax?: number | null;
  readonly scaleUnit?: string;
  readonly scaleFormat?: ValueFormat;
  readonly showAxis?: boolean;
  readonly animateOrder?: boolean;
  readonly twoSidedLayout?: BarsTwoSidedLayout;
  readonly leftHeader?: string;
  readonly rightHeader?: string;
  readonly readoutHeader?: string;
  readonly caption?: string;
  readonly captionRight?: string;
  readonly legend?: readonly BarsLegendItem[];
  readonly rowHeight?: number;
  readonly barHeight?: number;
  readonly summaryText?: string;
  readonly empty?: string;
  readonly emptyDetail?: string;
  readonly emptyKind?: 'empty' | 'unavailable';
  readonly regionsActive?: ActiveRegions;
  readonly notes?: readonly string[];
  readonly warnings?: readonly string[];
  readonly nextChangeAt?: number;
  readonly dataHours?: readonly number[];
}

export interface BarsProjection<TOptions = unknown> extends FamilyProjection<BarsModel, TOptions> {
  readonly family: 'bars';
  project(readView: BoundedReadView, options: TOptions): BarsModel;
  nextChange?(readView: BoundedReadView, options: TOptions): number;
}

export function isSingleValueOrdered(model: BarsModel, form: BarsPlaybackForm = true): boolean {
  return model.form === 'ordered' && !model.pickList && !model.twoSidedLayout
    && model.rows.every(row => row.kind === 'section' || (row.kind === 'data' && row.value !== undefined
      && !row.left && !row.right && !row.rightSide && !row.pair
      && (form === 'ordered-segments' || row.segments === undefined)
      && (form === true ? !row.target && !row.readout
        : form === 'ordered-segments' ? !row.target
          : !!row.target || (row.value.value.kind === 'zero' && row.numeric === 0))));
}

export class FutureBarsDataError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FutureBarsDataError';
  }
}

export class InvalidBarsModelError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'InvalidBarsModelError';
  }
}

function validateValueAndNumeric(field: BarsValueField, numeric: number | null | undefined, context: string): void {
  assertValue(field.value, context);
  if (!hasMeasuredValue(field.value)) {
    if (numeric !== undefined && numeric !== null) {
      throw new InvalidBarsModelError(`${context}: absent value (${field.value.kind}) cannot carry a numeric bar magnitude (${numeric})`);
    }
    return;
  }
  if (numeric !== undefined && numeric !== null && !Number.isFinite(numeric)) {
    throw new InvalidBarsModelError(`${context}: numeric bar magnitude must be finite`);
  }
}

export function assertBarsModelValid(model: BarsModel, displayedHour: number): void {
  if (!Number.isFinite(model.hour) || model.hour > displayedHour) {
    throw new FutureBarsDataError(`Bars model hour ${model.hour} exceeds displayed hour ${displayedHour}`);
  }
  if (model.dataHours) {
    for (let i = 0; i < model.dataHours.length; i++) {
      const h = model.dataHours[i]!;
      if (!Number.isFinite(h) || h > displayedHour) {
        throw new FutureBarsDataError(`Bars model carries data at hour ${h} after displayed hour ${displayedHour}`);
      }
    }
  }
  for (let i = 0; i < model.rows.length; i++) {
    const row = model.rows[i]!;
    if (row.kind !== 'data') continue;
    const prefix = `bars[${row.key}]`;
    if (row.label !== undefined) {
      assertValue(row.label, `${prefix}.label`);
    } else if (
      model.form !== 'two-sided'
      || model.twoSidedLayout !== 'split-sides'
      || !row.rightSide
      || row.subject !== undefined
      || row.media !== undefined
      || row.color !== undefined
      || row.muted !== undefined
      || row.region !== undefined
      || row.group !== undefined
      || row.value !== undefined
      || row.numeric !== undefined
      || row.target !== undefined
      || row.segments !== undefined
      || row.left !== undefined
      || row.right !== undefined
      || row.readout !== undefined
      || row.pair !== undefined
      || row.tooltip !== undefined
      || row.action !== undefined
      || row.meta !== undefined
    ) {
      throw new InvalidBarsModelError(`${prefix}.label: data row without label must be an empty split-side placeholder`);
    }
    if (row.value) validateValueAndNumeric(row.value, row.numeric, `${prefix}.value`);
    if (row.left) validateValueAndNumeric(row.left.value, row.left.numeric, `${prefix}.left`);
    if (row.right) validateValueAndNumeric(row.right.value, row.right.numeric, `${prefix}.right`);
    if (row.rightSide) {
      assertValue(row.rightSide.label, `${prefix}.rightSide.label`);
      validateValueAndNumeric(row.rightSide.value, row.rightSide.numeric, `${prefix}.rightSide`);
    }
    if (row.target) {
      assertValue(row.target.value, `${prefix}.target`);
      if (row.target.shortfall) assertValue(row.target.shortfall, `${prefix}.target.shortfall`);
    }
    if (row.segments) {
      for (let s = 0; s < row.segments.length; s++) {
        const seg = row.segments[s]!;
        assertValue(seg.value, `${prefix}.segments[${seg.key}]`);
        if (!Number.isFinite(seg.share) || seg.share < 0) {
          throw new InvalidBarsModelError(`${prefix}.segments[${seg.key}]: invalid share ${seg.share}`);
        }
      }
    }
    if (row.pair) {
      assertValue(row.pair.topValue, `${prefix}.pair.top`);
      assertValue(row.pair.bottomValue, `${prefix}.pair.bottom`);
    }
  }
}

export function formatBarsValueField(field: BarsValueField): { readonly text: string; readonly exact: string; readonly spoken: string; readonly missingReason?: string; readonly partialReason?: string; readonly kind: ReturnType<typeof formatValueState>['kind'] } {
  const formatted = formatValueState(field.value, field.format, { compact: field.compact, figure: field.figure });
  const suffix = field.unit ? ` ${field.unit}` : '';
  if (formatted.kind === 'notRecorded' || formatted.kind === 'notApplicable' || formatted.kind === 'pending') {
    return {
      kind: formatted.kind,
      text: formatted.label,
      exact: formatted.label,
      spoken: formatted.spoken,
      ...(formatted.kind === 'pending' ? {} : { missingReason: formatted.reason }),
    };
  }
  const text = `${formatted.text}${suffix}`;
  const exact = `${formatted.exact ?? formatted.text}${suffix}`;
  return {
    kind: formatted.kind,
    text,
    exact,
    spoken: formatted.spoken,
    ...(formatted.kind === 'partial' ? { partialReason: formatted.reason } : {}),
  };
}

const rowIndexes = new WeakMap<readonly BarsRow[], ReadonlyMap<string, number>>();

export function barsRowIndex(model: BarsModel, key: string): number {
  let index = rowIndexes.get(model.rows);
  if (!index) {
    const built = new Map<string, number>();
    for (let i = 0; i < model.rows.length; i++) {
      const row = model.rows[i]!;
      if (!built.has(row.key)) built.set(row.key, i);
      if (row.kind === 'data' && row.rightSide && !built.has(row.rightSide.key)) built.set(row.rightSide.key, i);
    }
    index = built;
    rowIndexes.set(model.rows, index);
  }
  return index.get(key) ?? -1;
}

function withPendingLabelItem(label: Value<string> | undefined, tooltip: FamilyTooltipPayload): FamilyTooltipPayload {
  if (label?.kind !== 'pending') return tooltip;
  const items = tooltip.items ?? [];
  if (items.some(item => item.label === 'Name' && item.pendingReason !== undefined)) return tooltip;
  return {
    ...tooltip,
    items: [{ label: 'Name', pendingReason: label.reason }, ...items],
  };
}

export function deriveBarsTooltip(model: BarsModel, rowKey: string, side?: 'left' | 'right'): FamilyTooltipPayload | null {
  const row = model.rows[barsRowIndex(model, rowKey)];
  if (row?.kind !== 'data') return null;
  const tipHour = model.dataHours?.length === 1 ? model.dataHours[0]! : model.hour;
  if (side === 'right' && row.rightSide) {
    if (row.rightSide.tooltip) {
      const tip = row.rightSide.tooltip.hour !== undefined ? row.rightSide.tooltip : { ...row.rightSide.tooltip, hour: tipHour };
      return withPendingLabelItem(row.rightSide.label, tip);
    }
    const formatted = formatBarsValueField(row.rightSide.value);
    const items: TooltipListItem[] = [];
    if (row.rightSide.label.kind === 'pending') {
      items.push({ label: 'Name', pendingReason: row.rightSide.label.reason });
    }
    if (!formatted.missingReason) items.push({ label: model.rightHeader ?? 'Value', value: formatted.exact });
    return {
      title: barsLabelText(row.rightSide.label),
      hour: tipHour,
      ...(formatted.missingReason ? { missingReason: formatted.missingReason } : {}),
      ...(items.length ? { items } : {}),
    };
  }
  if (row.tooltip) {
    const tip = row.tooltip.hour !== undefined ? row.tooltip : { ...row.tooltip, hour: tipHour };
    return withPendingLabelItem(row.label, tip);
  }
  if (row.label === undefined && !row.value && !row.left && !row.right && !row.pair && !row.segments?.length && !row.readout) {
    return null;
  }
  const title = barsLabelText(row.label);
  const items: TooltipListItem[] = [];
  let missingReason: string | undefined;
  let exactValue: string | undefined;

  if (row.value) {
    const formatted = formatBarsValueField(row.value);
    if (formatted.missingReason) {
      missingReason = formatted.missingReason;
    } else {
      if (formatted.exact !== formatted.text) exactValue = formatted.exact;
      const label = row.value.label ?? row.value.caption ?? model.scaleUnit ?? 'Value';
      items.push({
        label,
        value: formatted.exact,
        ...(row.value.tone === 'shortage' ? { tone: 'negative' as const } : {}),
      });
    }
  }
  if (row.target) {
    const targetFmt = formatValueState(row.target.value, row.target.format);
    if (targetFmt.kind === 'notRecorded' || targetFmt.kind === 'notApplicable') {
      items.push({ label: 'Need', missingReason: targetFmt.reason });
    } else {
      items.push({ label: 'Need', value: targetFmt.kind === 'pending' ? targetFmt.label : targetFmt.text });
    }
    if (row.target.shortfall) {
      const shortFmt = formatValueState(row.target.shortfall, row.target.format);
      if (shortFmt.kind === 'known' || shortFmt.kind === 'partial') {
        items.push({ label: 'Shortfall', value: shortFmt.text, tone: 'negative' });
      }
    }
  }
  if (row.left) {
    const leftFmt = formatBarsValueField(row.left.value);
    items.push(leftFmt.missingReason
      ? { label: model.leftHeader ?? 'Left', missingReason: leftFmt.missingReason }
      : { label: model.leftHeader ?? 'Left', value: leftFmt.exact, swatch: row.left.color ?? row.color, ...(leftFmt.partialReason ? { partialReason: leftFmt.partialReason } : {}) });
  }
  if (row.right) {
    const rightFmt = formatBarsValueField(row.right.value);
    items.push(rightFmt.missingReason
      ? { label: model.rightHeader ?? 'Right', missingReason: rightFmt.missingReason }
      : { label: model.rightHeader ?? 'Right', value: rightFmt.exact, swatch: row.right.color ?? row.color, ...(rightFmt.partialReason ? { partialReason: rightFmt.partialReason } : {}) });
  }
  if (row.pair) {
    const topFmt = formatValueState(row.pair.topValue, row.pair.format);
    const botFmt = formatValueState(row.pair.bottomValue, row.pair.format);
    items.push(topFmt.kind === 'notRecorded' || topFmt.kind === 'notApplicable'
      ? { label: row.pair.topLabel, missingReason: topFmt.reason }
      : { label: row.pair.topLabel, value: topFmt.kind === 'pending' ? topFmt.label : topFmt.text });
    items.push(botFmt.kind === 'notRecorded' || botFmt.kind === 'notApplicable'
      ? { label: row.pair.bottomLabel, missingReason: botFmt.reason }
      : { label: row.pair.bottomLabel, value: botFmt.kind === 'pending' ? botFmt.label : botFmt.text });
  }
  if (row.segments?.length) {
    for (let i = 0; i < row.segments.length; i++) {
      const seg = row.segments[i]!;
      const segFmt = formatValueState(seg.value, seg.format);
      items.push(segFmt.kind === 'notRecorded' || segFmt.kind === 'notApplicable'
        ? { label: seg.label, swatch: seg.color, missingReason: segFmt.reason }
        : { label: seg.label, swatch: seg.color, value: `${segFmt.kind === 'pending' ? segFmt.label : segFmt.text} (${formatShare(seg.share)})` });
    }
  }
  if (row.readout?.primary && !items.length && !missingReason) {
    items.push({ label: row.readout.secondary ?? 'Count', value: row.readout.primary });
  }
  if (row.label?.kind === 'pending') {
    items.unshift({ label: 'Name', pendingReason: row.label.reason });
  }
  const art = artTooltipItem(row.media?.kind === 'flag' ? 'Flag' : 'Art', row.media?.art);
  if (art) items.push(art);

  return {
    title,
    fullText: title,
    hour: tipHour,
    ...(exactValue ? { exactValue } : {}),
    ...(missingReason ? { missingReason } : {}),
    ...(items.length ? { items } : {}),
  };
}

export function deriveBarsMirror(
  model: BarsModel,
  window?: { readonly first: number; readonly count: number },
  focusedKey?: string | null,
): MirrorModel {
  const first = window ? Math.max(0, window.first) : 0;
  const end = window ? Math.min(model.rows.length, first + Math.max(0, window.count)) : model.rows.length;
  const rows: MirrorRow[] = [];
  const pushRow = (index: number) => {
    const item = model.rows[index];
    if (!item) return;
    if (item.kind === 'section') {
      rows.push({ key: item.key, name: item.label, rowIndex: index, groupHeader: true, cells: [] });
      return;
    }
    if (item.kind === 'add-region') {
      rows.push({ key: item.key, name: item.label, rowIndex: index, cells: [{ column: 'action', spoken: item.label }] });
      return;
    }
    const name = barsLabelSpoken(item.label);
    const cells: { readonly column: string; readonly spoken: string }[] = [];
    if (item.value) cells.push({ column: 'value', spoken: formatBarsValueField(item.value).spoken });
    if (item.left) cells.push({ column: model.leftHeader ?? 'left', spoken: formatBarsValueField(item.left.value).spoken });
    if (item.right) cells.push({ column: model.rightHeader ?? 'right', spoken: formatBarsValueField(item.right.value).spoken });
    if (item.rightSide) {
      const rightName = barsLabelSpoken(item.rightSide.label);
      cells.push({ column: 'rightSide', spoken: `${rightName}: ${formatBarsValueField(item.rightSide.value).spoken}` });
    }
    if (item.segments?.length) {
      cells.push({
        column: 'segments',
        spoken: item.segments.map(seg => `${seg.label} ${formatShare(seg.share)}`).join(', '),
      });
    }
    if (item.readout?.primary) {
      const { primary, secondary, spoken } = item.readout;
      cells.push({ column: 'readout', spoken: spoken ?? (secondary ? `${primary} ${secondary}` : primary) });
    }
    rows.push({ key: item.key, name, rowIndex: index, cells });
  };
  let hasFocused = false;
  for (let i = first; i < end; i++) {
    if (focusedKey && model.rows[i]?.key === focusedKey) hasFocused = true;
    pushRow(i);
  }
  if (focusedKey && !hasFocused) {
    const idx = barsRowIndex(model, focusedKey);
    if (idx >= 0 && model.rows[idx]!.key === focusedKey) pushRow(idx);
  }
  return { first, total: model.rows.length, rows };
}

export function deriveBarsAudit(
  model: BarsModel,
  options?: { readonly window?: { readonly first: number; readonly count: number } },
): FamilyAuditRow[] {
  const rows: FamilyAuditRow[] = [];
  if (model.dataHours?.some(h => h !== model.hour)) {
    rows.push(auditCell('__bars__', 'dataHours', JSON.stringify(model.dataHours)));
  }
  if (model.empty !== undefined && model.rows.length === 0) {
    const key = model.emptyKind ?? 'empty';
    rows.push(auditRow(key, model.empty));
    return rows;
  }
  const first = options?.window ? Math.max(0, options.window.first) : 0;
  const end = options?.window ? Math.min(model.rows.length, first + Math.max(0, options.window.count)) : model.rows.length;
  for (let i = first; i < end; i++) {
    const item = model.rows[i]!;
    if (item.kind !== 'data') continue;
    const label = barsLabelAudit(item.label);
    const cell = (row: string, name: string, value: string) => rows.push(auditCell(row, name, value));
    cell(item.key, 'label', label);
    if (item.value) cell(item.key, 'value', canonicalAuditValue(item.value.value));
    if (item.left) cell(item.key, 'left', canonicalAuditValue(item.left.value.value));
    if (item.right) cell(item.key, 'right', canonicalAuditValue(item.right.value.value));
    if (item.rightSide) {
      cell(item.rightSide.key, 'label', barsLabelAudit(item.rightSide.label));
      cell(item.rightSide.key, 'rightSide', canonicalAuditValue(item.rightSide.value.value));
    }
    if (item.target) cell(item.key, 'target', canonicalAuditValue(item.target.value));
    if (item.pair) {
      cell(item.key, 'pairTop', canonicalAuditValue(item.pair.topValue));
      cell(item.key, 'pairBottom', canonicalAuditValue(item.pair.bottomValue));
    }
    if (item.segments?.length) {
      for (let s = 0; s < item.segments.length; s++) {
        const seg = item.segments[s]!;
        cell(item.key, `segment:${seg.key}`, `${Math.round(seg.share * 10000)}:${seg.hatched ? 'hatched:' : ''}${canonicalAuditValue(seg.value)}`);
      }
    }
    if (item.readout) {
      const { primary, spoken, secondary, shortfall, tone } = item.readout;
      cell(item.key, 'readout', JSON.stringify([spoken ?? primary, secondary ?? null, shortfall ?? null, tone ?? 'default']));
    }
  }
  return rows;
}
