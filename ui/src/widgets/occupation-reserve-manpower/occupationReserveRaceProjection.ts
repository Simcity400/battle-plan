import { dayLabel } from '../../charts/draw.ts';
import { colour } from '../../charts/palette.ts';
import { countryKey } from '../../dashboard/countryKey.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { forceAtSources, GROUP_EMPTY } from '../../families/bars/forceAtRace.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings, type SeriesRaceBaseOptions } from '../../families/bars/seriesRace.ts';
import { barsLabelText, countryRowPresentation, type BarsDataRow, type BarsModel, type BarsPartReading, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notRecorded, zeroValue, type Value } from '../../families/value.ts';
import type { Country, OccupationReserveReply, ReserveHolder } from '../../generated/dashboard';
import { persons, validateOccupationReserve } from './occupationReserveData.ts';

export type OccupationReserveRaceOptions = SeriesRaceBaseOptions;

interface OccupationReserveRowMeta {
  readonly tag: string;
  readonly id: number;
  readonly key: string;
}

function sourcePresentation(readView: BoundedReadView, source: Country, surface: string): {
  readonly key: string;
  readonly label: string;
  readonly value: Value<string>;
  readonly color: string;
} {
  if (source.tag === null) {
    return { key: `unresolved:${source.id}`, label: 'Source country unavailable', value: notRecorded('Source country was not recorded'), color: colour(surface, 0) };
  }
  const pres = countryRowPresentation(readView, source.tag);
  return {
    key: source.tag,
    label: pres.label.kind === 'known' ? pres.label.value : pres.label.kind === 'pending' ? barsLabelText(pres.label) : 'Source country unavailable',
    value: pres.label,
    color: pres.color ?? colour(surface, 0),
  };
}

function sourceTooltipItems(src: ReturnType<typeof sourcePresentation>, amountText: string, withSwatch: boolean): readonly TooltipListItem[] {
  if (src.value.kind === 'pending') {
    return [
      { label: 'Source country', ...(withSwatch ? { swatch: src.color } : {}), pendingReason: src.value.reason },
      { label: 'Manpower', indent: true, value: amountText },
    ];
  }
  return [{ label: src.label, ...(withSwatch ? { swatch: src.color } : {}), value: amountText }];
}

function makeHolderRow(readView: BoundedReadView, holder: ReserveHolder, readHour: number | null, surface: string): BarsDataRow | null {
  const tag = holder.country.tag;
  if (tag === null) return null;
  const key = holder.slot === null ? tag : countryKey(holder.slot);
  const pres = countryRowPresentation(readView, key);
  const total = holder.total;
  const value = total === 0 ? zeroValue() : knownValue(total);

  const bySource = new Map<string, { readonly label: string; readonly color: string; amount: number }>();
  const partReadings: BarsPartReading[] = [];
  const positiveItems: TooltipListItem[] = [];
  const negativeItems: TooltipListItem[] = [];
  for (const entry of holder.entries) {
    const src = sourcePresentation(readView, entry.source, surface);
    partReadings.push({ key: String(entry.ordinal), label: src.label, value: knownValue(entry.amount) });
    if (entry.amount > 0) {
      const existing = bySource.get(src.key);
      if (existing) existing.amount += entry.amount;
      else bySource.set(src.key, { label: src.label, color: src.color, amount: entry.amount });
      positiveItems.push(...sourceTooltipItems(src, persons(entry.amount), true));
    } else if (entry.amount === 0) {
      positiveItems.push(...sourceTooltipItems(src, '0', true));
    } else {
      negativeItems.push(...sourceTooltipItems(src, persons(entry.amount), false));
    }
  }

  let segments: BarsSegment[] | undefined;
  if (total > 0 && bySource.size > 0) {
    const ordered = [...bySource.entries()].sort((a, b) => b[1].amount - a[1].amount || a[0].localeCompare(b[0]));
    segments = ordered.map(([srcKey, item]) => ({
      key: srcKey,
      label: item.label,
      share: item.amount / total,
      value: knownValue(item.amount),
      format: { decimals: 0 },
      color: item.color,
    }));
  }

  const tooltipItems: TooltipListItem[] = [
    { label: 'Reserve manpower held', value: persons(total) },
    ...positiveItems,
    ...(negativeItems.length ? [{ label: 'Negative entries', value: '' }, ...negativeItems] : []),
    ...(readHour !== null ? [{ label: 'As of', value: dayLabel(readHour) }] : []),
  ];

  const meta: OccupationReserveRowMeta = { tag, id: holder.country.id, key };
  return {
    kind: 'data',
    key,
    subject: key,
    label: pres.label,
    media: pres.media,
    ...(pres.color ? { color: pres.color } : {}),
    value: { value, format: { decimals: 0 }, label: 'Reserve manpower' },
    numeric: total,
    ...(segments ? { segments } : {}),
    ...(partReadings.length ? { partReadings } : {}),
    tooltip: {
      title: barsLabelText(pres.label),
      hour: readView.hour,
      items: tooltipItems,
    },
    meta,
  };
}

export const occupationReserveRaceProjection: BarsProjection<OccupationReserveRaceOptions> = registerFamilyProjection({
  kind: 'occupation-reserve-manpower',
  family: 'bars',
  singleValueBars: () => 'ordered-segments',
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: OccupationReserveRaceOptions): BarsModel {
    const hour = readView.hour;
    const baseModel = {
      form: 'ordered' as const,
      hour,
      animateOrder: true,
      showAxis: true,
      scaleUnit: 'Manpower',
      scaleFormat: { decimals: 0 },
    };

    const refused = readView.refused('main');
    if (refused) {
      return { ...baseModel, rows: [], empty: refused, emptyKind: 'unavailable' };
    }

    const panel = readView.panel('main');
    if (panel === undefined) {
      return { ...baseModel, rows: [] };
    }
    const reply: OccupationReserveReply = validateOccupationReserve(panel);
    if (reply.missing !== null) {
      return { ...baseModel, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings };
    }

    const rows: BarsDataRow[] = [];
    for (const holder of reply.holders) {
      const row = makeHolderRow(readView, holder, reply.readHour, options.surface);
      if (row) rows.push(row);
    }

    const warnings = [
      ...reply.warnings,
      ...(reply.unnamed > 0 ? [`${reply.unnamed} holders the recording names no tag for are not shown.`] : []),
    ];

    return {
      ...baseModel,
      rows,
      warnings,
      ...(rows.length ? {} : reply.noneMissing !== null
        ? { empty: reply.noneMissing, emptyKind: 'unavailable' as const }
        : { empty: options.group ? GROUP_EMPTY : 'No reserve manpower is held at this date.', emptyKind: 'empty' as const }),
    };
  },
}, 'main');

function occupationReserveSources(ctx: FamilyPlanContext): SourcePlan {
  const owner = `occupation-reserve:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}`;
  return forceAtSources(ctx, owner, 'occupation-reserve-manpower', {}, validateOccupationReserve);
}

export const occupationReserveRaceDeclaration: BarsWidgetDeclaration<OccupationReserveRaceOptions> = {
  family: 'bars',
  kind: 'occupation-reserve-manpower',
  label: 'Occupation reserve manpower',
  subject: 'occupation manpower',
  emptyCampaign: 'Load a campaign to see occupation manpower.',
  icon: 'flag',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: occupationReserveSources,
  projection: occupationReserveRaceProjection,
  projectionOptions: seriesRaceSelection,
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
