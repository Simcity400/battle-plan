import type { PanelKey } from '../../generated/dashboard';
import { countryFromKey, countryKey } from '../../dashboard/countryKey.ts';
import type { BoundedReadView, FamilyPlanContext, SourceCoverage, SourcePlan } from '../contract.ts';
import { groupSelection } from '../groupSelection.ts';
import type { TooltipListItem } from '../host/tooltips.ts';
import { knownValue, notRecorded, partialValue, type Value } from '../value.ts';
import { formatValueState, type ValueFormat } from '../valueFormat.ts';
import type { ForceAtReply, ForceAtRow, ForcePart, NavalLossesAtReply } from '../../generated/dashboard';
import { partColours } from './partColour.ts';
import { barsLabelText, countryRowPresentation, type BarsDataRow, type BarsModel, type BarsSegment, type CountryRowPresentation } from './model.ts';
import type { SeriesRaceBaseOptions } from './seriesRace.ts';


export interface ForceAtRaceBaseOptions extends SeriesRaceBaseOptions {
  readonly split: boolean;
}

export const GROUP_EMPTY = 'No country of this group at this date.';

export interface ForceSelection {
  readonly params: { readonly countries: readonly (string | number)[]; readonly top?: number; readonly group?: unknown };
  readonly coverage: SourceCoverage;
  readonly none: boolean;
}

export function forceSelection(ctx: FamilyPlanContext): ForceSelection {
  const top = ctx.countries.top;
  const all = ctx.countries.all;
  const group = top == null && !all ? groupSelection(ctx.widget.countries) : null;
  const countries = top == null && !all && !group ? ctx.countries.tags.map(countryFromKey) : [];
  const coverage: SourceCoverage = top != null
    ? { mode: 'population', form: 'top', limit: top }
    : all || group
      ? { mode: 'population', form: 'all' }
      : { mode: 'keyed', keys: ctx.countries.tags };
  return {
    params: { countries, ...(top != null ? { top } : {}), ...(group ? { group } : {}) },
    coverage,
    none: top == null && !all && !group && !countries.length,
  };
}

export function forceAtSources(
  ctx: FamilyPlanContext,
  owner: string,
  product: PanelKey,
  extra: Readonly<Record<string, unknown>>,
  validate: (value: unknown) => unknown = validateForceAtReply,
): SourcePlan {
  const selection = forceSelection(ctx);
  const params = { branchId: ctx.branch.id, ...selection.params, ...extra };
  return {
    owner,
    entries: selection.none ? [] : [{
      name: 'main',
      source: ctx.track.source,
      branch: ctx.branch.id,
      offset: ctx.track.axisOffset,
      read: {
        shape: 'point',
        channel: 'panel',
        coverage: selection.coverage,
        product,
        followQuery: { panel: product, ...params },
        params,
        validate,
      },
    }],
    allowEmptyFollow: selection.none,
  };
}

export interface ForceRowIdentity {
  readonly key: string;
  readonly tag: string;
  readonly pres: CountryRowPresentation;
}

export function forceRowIdentity(readView: BoundedReadView, row: ForceAtRow): ForceRowIdentity {
  const key = countryKey(row.slot ?? row.country);
  const pres = countryRowPresentation(readView, key);
  const identity = readView.who('country', key);
  const tag = identity.tag?.status === 'ready' ? identity.tag.value : row.tag;
  return { key, tag, pres };
}

export interface ForceAtProjectConfig {
  readonly decimals: number;
  readonly valueFormat: ValueFormat;
  readonly scaleFormat: ValueFormat;
  readonly scaleUnit: string;
  readonly valueLabel: string;
  readonly partsLabel: string;
  readonly emptyMessage: string;
  readonly referenceLabel?: string;
  readonly note?: string;
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isTextOrNull = (v: unknown): v is string | null => v === null || typeof v === 'string';
const isNumOrNull = (v: unknown): v is number | null => v === null || (typeof v === 'number' && Number.isFinite(v));

function isForcePart(v: unknown): v is ForcePart {
  return isObj(v) && typeof v.key === 'string' && isTextOrNull(v.name) && typeof v.unresolved === 'boolean';
}

function isForceAtRow(v: unknown): v is ForceAtRow {
  return isObj(v)
    && (typeof v.country === 'string' || typeof v.country === 'number')
    && (v.slot === null || (typeof v.slot === 'number' && Number.isInteger(v.slot)))
    && typeof v.tag === 'string'
    && isNumOrNull(v.total)
    && (v.referenceTotal === undefined || isNumOrNull(v.referenceTotal))
    && isTextOrNull(v.missing)
    && Array.isArray(v.values) && v.values.every(isNumOrNull)
    && isTextOrNull(v.partsMissing)
    && (v.partial === undefined || (typeof v.partial === 'string' && v.partial.length > 0 && v.total !== null))
    && (v.partsPartial === undefined || (Array.isArray(v.partsPartial) && v.partsPartial.length === v.values.length && v.partsPartial.every(isTextOrNull)));
}

function isOrdinal(v: unknown): v is number | null {
  return v === null || (typeof v === 'number' && Number.isInteger(v) && v >= 0);
}

export function validateForceAtReply(value: unknown): ForceAtReply {
  validateForceShape(value);
  if (!Array.isArray(value.ordinals) || value.ordinals.length !== value.parts.length || !value.ordinals.every(isOrdinal)) {
    throw new Error('The force snapshot reply is malformed.');
  }
  return value as unknown as ForceAtReply;
}

function validateForceShape(value: unknown): asserts value is Record<string, unknown> & { parts: ForcePart[] } {
  if (
    !isObj(value)
    || !Array.isArray(value.parts) || !value.parts.every(isForcePart)
    || !Array.isArray(value.rows) || !value.rows.every(isForceAtRow)
    || !isTextOrNull(value.missing)
    || !Array.isArray(value.warnings) || !value.warnings.every(w => typeof w === 'string')
  ) {
    throw new Error('The force snapshot reply is malformed.');
  }
}

const NAVAL_LOSS_PARTS: readonly string[] = ['ships', 'convoys'];

export function validateNavalLossesAtReply(value: unknown): NavalLossesAtReply & Pick<ForceAtReply, 'ordinals'> {
  if (
    !isObj(value)
    || typeof value.firstHour !== 'number' || !Number.isInteger(value.firstHour)
    || typeof value.lastHour !== 'number' || !Number.isInteger(value.lastHour)
  ) {
    throw new Error('The naval losses snapshot reply is malformed.');
  }
  validateForceShape(value);
  const reply = value as unknown as NavalLossesAtReply;
  return { ...reply, ordinals: reply.parts.map(part => NAVAL_LOSS_PARTS.includes(part.key) ? NAVAL_LOSS_PARTS.indexOf(part.key) : null) };
}

export function witnessedPartItems(reply: ForceAtReply, item: ForceAtRow, decimals: number, format: ValueFormat): TooltipListItem[] {
  return reply.parts.flatMap((part, at): TooltipListItem[] => {
    const raw = item.values[at] ?? null;
    if (raw === null) return item.values.length ? [{ label: forcePartLabel(part), missingReason: 'Not recorded at this hour' }] : [];
    const reason = item.partsPartial?.[at] ?? (reply.parts.length === 1 ? item.partial : undefined);
    return raw > 0 || reason ? [{
      label: forcePartLabel(part), value: formatScaled(forceReading(raw, decimals, null).value, format),
      ...(reason ? { partialReason: reason } : {}),
    }] : [];
  });
}

export function forcePartLabel(part: ForcePart): string {
  return part.name ?? (part.unresolved ? 'Name unavailable' : part.key);
}

export function forceReading(raw: number | null, decimals: number, missingReason: string | null, partial?: string | null): { readonly value: Value<number>; readonly numeric: number | null } {
  if (raw === null) return { value: notRecorded(missingReason ?? 'Not recorded at this hour'), numeric: null };
  const numeric = raw / (10 ** decimals);
  return { value: partial ? partialValue(numeric, partial, { atLeast: true }) : knownValue(numeric), numeric };
}

export function formatScaled(value: Value<bigint | number>, format: ValueFormat): string {
  const fmt = formatValueState(value, format);
  if (fmt.kind === 'known' || fmt.kind === 'zero' || fmt.kind === 'partial') return fmt.exact ?? fmt.text;
  return fmt.label;
}

export function projectForceAtReply(
  readView: BoundedReadView,
  reply: ForceAtReply,
  options: ForceAtRaceBaseOptions,
  config: ForceAtProjectConfig,
): BarsModel {
  const hour = readView.hour;
  const baseModel = {
    form: 'ordered' as const,
    hour,
    animateOrder: true,
    showAxis: true,
    scaleUnit: config.scaleUnit,
    scaleFormat: config.scaleFormat,
  };

  if (reply.missing !== null) {
    return { ...baseModel, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings };
  }

  const colours = partColours(reply.parts.map((_, at) => reply.ordinals[at]), options.surface);
  const rows: BarsDataRow[] = [];
  for (const item of reply.rows) {
    const { key, tag: displayTag, pres } = forceRowIdentity(readView, item);
    const { value, numeric } = forceReading(item.total, config.decimals, item.missing, item.partial);
    const totalItem: TooltipListItem = value.kind === 'notRecorded'
      ? { label: config.valueLabel, missingReason: value.reason }
      : { label: config.valueLabel, value: formatScaled(value, config.valueFormat), ...(item.partial ? { partialReason: item.partial } : {}) };
    const tooltipItems: TooltipListItem[] = [totalItem];

    if (config.referenceLabel && item.referenceTotal !== undefined) {
      if (item.referenceTotal === null) {
        tooltipItems.push({ label: config.referenceLabel, missingReason: 'Not recorded at this hour' });
      } else {
        const refScaled = forceReading(item.referenceTotal, config.decimals, null);
        tooltipItems.push({ label: config.referenceLabel, value: formatScaled(refScaled.value, config.valueFormat) });
      }
    }

    if (value.kind === 'notRecorded' && item.values.some(raw => raw !== null)) tooltipItems.push(...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
    if (options.split && numeric === 0 && item.partsPartial) tooltipItems.push(...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
    let segments: BarsSegment[] | undefined;
    let segmentsIncomplete = options.split && (item.partsMissing !== null || item.partsPartial?.some(Boolean) === true);
    if (options.split && numeric !== null && numeric > 0) {
      if (item.partsMissing !== null) {
        tooltipItems.push({ label: config.partsLabel, missingReason: item.partsMissing }, ...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
      } else if (reply.parts.length > 0 && item.values.length === reply.parts.length) {
        const built: BarsSegment[] = [];
        let anyMissing = false;
        const partEntries: { readonly part: ForcePart; readonly color: string; readonly scaled: { readonly value: Value<bigint | number>; readonly numeric: number | null } }[] = [];
        for (let i = 0; i < reply.parts.length; i++) {
          const rawPart = item.values[i] ?? null;
          if (rawPart === null) {
            anyMissing = true;
            break;
          }
          const reason = item.partsPartial?.[i] ?? (reply.parts.length === 1 ? item.partial : undefined);
          if (rawPart <= 0 && !reason) continue;
          const scaled = forceReading(rawPart, config.decimals, null, reason);
          partEntries.push({ part: reply.parts[i]!, color: colours[i]!, scaled });
        }
        if (anyMissing) {
          segmentsIncomplete = true;
          tooltipItems.push({ label: config.partsLabel, missingReason: 'Parts not recorded at this hour' });
        } else {
          for (const entry of partEntries) {
            const partNum = entry.scaled.numeric ?? 0;
            const partName = forcePartLabel(entry.part);
            if (partNum > 0) built.push({
              key: entry.part.key,
              label: partName,
              share: partNum / numeric,
              value: entry.scaled.value,
              format: config.valueFormat,
              color: entry.color,
            });
            tooltipItems.push({ label: partName, swatch: entry.color, value: formatScaled(entry.scaled.value, config.valueFormat), ...(entry.scaled.value.kind === 'partial' ? { partialReason: entry.scaled.value.reason } : {}) });
          }
          if (built.length) segments = built;
        }
      } else segmentsIncomplete = true;
    }

    rows.push({
      kind: 'data',
      key,
      subject: displayTag,
      label: pres.label,
      media: pres.media,
      ...(pres.color ? { color: pres.color } : {}),
      value: { value, format: config.valueFormat, label: config.valueLabel },
      numeric,
      ...(segments ? { segments } : {}),
      ...(segmentsIncomplete ? { segmentsIncomplete: true } : {}),
      tooltip: {
        title: barsLabelText(pres.label),
        hour,
        items: tooltipItems,
        ...(config.note ? { note: config.note } : {}),
      },
      meta: { tag: displayTag, key },
    });
  }

  return {
    ...baseModel,
    rows,
    warnings: reply.warnings,
    ...(!rows.length ? { empty: options.group ? GROUP_EMPTY : config.emptyMessage, emptyKind: 'empty' as const } : {}),
  };
}
