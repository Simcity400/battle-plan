import type { BoundedReadView } from '../contract.ts';
import type { TooltipListItem } from '../host/tooltips.ts';
import { notRecorded, type Value } from '../value.ts';
import { formatShare, formatValueState, type ValueFormat } from '../valueFormat.ts';
import type { ForceAtReply, ForcePart } from '../../generated/dashboard';
import { forcePartLabel, forceReading, forceRowIdentity, formatScaled, GROUP_EMPTY, witnessedPartItems } from './forceAtRace.ts';
import { barsLabelText, type BarsLegendItem, type BarsModel, type BarsSegment } from './model.ts';
import { partColours } from './partColour.ts';
import { buildShareRowsModel, type ShareRowEntry } from './shareRows.ts';

export interface ForceAtMixOptions {
  readonly top: number | null;
  readonly all?: boolean;
  readonly group: boolean;
  readonly surface: string;
}

export interface ForceAtMixConfig {
  readonly decimals: number;
  readonly valueFormat: ValueFormat;
  readonly valueLabel: string;
  readonly unit: string;
  readonly partsLabel: string;
  readonly emptyMessage: string;
  readonly referenceLabel?: string;
  readonly note?: string;
}

function sharePct(share: number): string {
  return formatShare(share);
}

export function projectForceAtMix(
  readView: BoundedReadView,
  reply: ForceAtReply,
  options: ForceAtMixOptions,
  config: ForceAtMixConfig,
): BarsModel {
  const hour = readView.hour;

  if (reply.missing !== null) {
    return buildShareRowsModel({
      hour,
      rows: [],
      empty: reply.missing,
      emptyKind: 'unavailable',
      warnings: reply.warnings,
    });
  }

  const colours = partColours(reply.parts.map((_, at) => reply.ordinals[at]), options.surface);
  const usedParts = new Set<number>();
  const rows: ShareRowEntry[] = [];

  for (const item of reply.rows) {
    const { key, tag: displayTag, pres } = forceRowIdentity(readView, item);
    const { value, numeric } = forceReading(item.total, config.decimals, item.missing, item.partial);
    const formatted = formatValueState(value, config.valueFormat);
    const countText = formatted.kind === 'partial' ? { primary: `${formatted.text} (partial)`, spoken: formatted.spoken } : formatted.audit;
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

    if (value.kind === 'notRecorded') {
      if (item.values.some(raw => raw !== null)) tooltipItems.push(...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
      rows.push({
        key,
        subject: displayTag,
        label: pres.label,
        media: pres.media,
        segments: [],
        missing: value,
        format: config.valueFormat,
        tooltip: {
          title: barsLabelText(pres.label),
          hour,
          missingReason: value.reason,
          items: tooltipItems,
          ...(config.note ? { note: config.note } : {}),
        },
        meta: { tag: displayTag, key },
      });
      continue;
    }

    if (numeric === 0) {
      if (item.partsPartial) tooltipItems.push(...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
      rows.push({
        key,
        subject: displayTag,
        label: pres.label,
        media: pres.media,
        segments: [],
        segmentsIncomplete: value.kind === 'partial' || item.partsMissing !== null || item.partsPartial?.some(Boolean)
          || item.values.length !== reply.parts.length || item.values.some(raw => raw === null),
        countText,
        tooltip: {
          title: barsLabelText(pres.label),
          hour,
          items: tooltipItems,
          ...(config.note ? { note: config.note } : {}),
        },
        meta: { tag: displayTag, key },
      });
      continue;
    }

    if (item.partsMissing !== null) {
      tooltipItems.push({ label: config.partsLabel, missingReason: item.partsMissing }, ...witnessedPartItems(reply, item, config.decimals, config.valueFormat));
      rows.push({
        key,
        subject: displayTag,
        label: pres.label,
        media: pres.media,
        segments: [],
        countText,
        missing: notRecorded(item.partsMissing),
        format: config.valueFormat,
        tooltip: {
          title: barsLabelText(pres.label),
          hour,
          missingReason: item.partsMissing,
          items: tooltipItems,
          ...(config.note ? { note: config.note } : {}),
        },
        meta: { tag: displayTag, key },
      });
      continue;
    }

    const segments: BarsSegment[] = [];
    let anyMissing = reply.parts.length === 0 || item.values.length !== reply.parts.length;
    const rawParts: { readonly index: number; readonly part: ForcePart; readonly scaled: { readonly value: Value<bigint | number>; readonly numeric: number | null } }[] = [];
    if (!anyMissing) {
      for (let i = 0; i < reply.parts.length; i++) {
        const rawPart = item.values[i] ?? null;
        if (rawPart === null) {
          anyMissing = true;
          break;
        }
        const reason = item.partsPartial?.[i] ?? (reply.parts.length === 1 ? item.partial : undefined);
        if (rawPart <= 0 && !reason) continue;
        rawParts.push({ index: i, part: reply.parts[i]!, scaled: forceReading(rawPart, config.decimals, null, reason) });
      }
    }

    if (anyMissing) {
      const reason = 'Part breakdown not recorded at this hour';
      tooltipItems.push({ label: config.partsLabel, missingReason: reason });
      rows.push({
        key,
        subject: displayTag,
        label: pres.label,
        media: pres.media,
        segments: [],
        countText,
        missing: notRecorded(reason),
        format: config.valueFormat,
        tooltip: {
          title: barsLabelText(pres.label),
          hour,
          missingReason: reason,
          items: tooltipItems,
          ...(config.note ? { note: config.note } : {}),
        },
        meta: { tag: displayTag, key },
      });
      continue;
    }

    const denom = numeric ?? 0;
    if (denom > 0) {
      for (const partItem of rawParts) {
        const partNum = partItem.scaled.numeric ?? 0;
        const share = partNum / denom;
        const label = forcePartLabel(partItem.part);
        const color = colours[partItem.index]!;
        if (share > 0) {
          usedParts.add(partItem.index);
          segments.push({
            key: partItem.part.key,
            label,
            share,
            value: partItem.scaled.value,
            format: config.valueFormat,
            color,
            inlineLabel: `${label} ${sharePct(share)}`,
          });
        }
        tooltipItems.push({
          label,
          swatch: color,
          value: `${formatScaled(partItem.scaled.value, config.valueFormat)} (${sharePct(share)})`,
          ...(partItem.scaled.value.kind === 'partial' ? { partialReason: partItem.scaled.value.reason } : {}),
        });
      }
    }

    rows.push({
      key,
      subject: displayTag,
      label: pres.label,
      media: pres.media,
      segments,
      segmentsIncomplete: value.kind === 'partial' || item.partsPartial?.some(Boolean),
      countText,
      tooltip: {
        title: barsLabelText(pres.label),
        hour,
        items: tooltipItems,
        ...(config.note ? { note: config.note } : {}),
      },
      meta: { tag: displayTag, key },
    });
  }

  const legend: BarsLegendItem[] = [];
  for (let i = 0; i < reply.parts.length; i++) {
    if (!usedParts.has(i)) continue;
    const part = reply.parts[i]!;
    legend.push({ key: part.key, label: forcePartLabel(part), color: colours[i]! });
  }

  return buildShareRowsModel({
    hour,
    rows,
    legend,
    unit: config.unit,
    warnings: reply.warnings,
    ...(!rows.length ? { empty: options.group ? GROUP_EMPTY : config.emptyMessage, emptyKind: 'empty' as const } : {}),
  });
}
