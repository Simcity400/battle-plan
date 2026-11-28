import { colour } from '../../charts/palette.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { GROUP_EMPTY } from '../../families/bars/forceAtRace.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings, type SeriesRaceBaseOptions } from '../../families/bars/seriesRace.ts';
import { barsLabelText, type BarsDataRow, type BarsModel, type BarsPartReading, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { BoundedReadView } from '../../families/contract.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, zeroValue } from '../../families/value.ts';
import { formatScalar } from '../../families/valueFormat.ts';
import type { NukeCountry, NukeType } from '../../generated/dashboard';
import { nuclearArsenalSources, nukeRowIdentity, typeName, validateNuclearArsenal } from './nuclearArsenalData.ts';

export interface NuclearArsenalRaceOptions extends SeriesRaceBaseOptions {
  readonly split: boolean;
}

function typeInfoMap(types: readonly NukeType[], surface: string): Map<number, { readonly label: string; readonly color: string }> {
  const map = new Map<number, { readonly label: string; readonly color: string }>();
  for (const type of types) map.set(type.ordinal, { label: typeName(type), color: colour(surface, type.ordinal) });
  return map;
}

function makeRow(
  readView: BoundedReadView,
  row: NukeCountry,
  types: ReadonlyMap<number, { readonly label: string; readonly color: string }>,
  split: boolean,
  surface: string,
): BarsDataRow {
  const { key, pres } = nukeRowIdentity(readView, row);
  const value = row.total === 0 ? zeroValue() : knownValue(row.total);
  const tooltipItems: TooltipListItem[] = [
    { label: 'Nuclear stockpile', value: formatScalar(row.total, 'integer') },
  ];
  const segments: BarsSegment[] = [];
  const partReadings: BarsPartReading[] = [];
  const splitFill = split && row.total > 0;
  for (const count of row.counts) {
    const { label, color } = types.get(count.ordinal) ?? { label: `Unnamed nuclear type ${count.ordinal}`, color: colour(surface, count.ordinal) };
    const reading = { key: String(count.ordinal), label, value: knownValue(count.value), format: { decimals: 0 } };
    partReadings.push(reading);
    const amount = formatScalar(count.value, 'integer');
    const detail = splitFill && count.ready > 0 ? `${amount} (${formatScalar(count.ready, 'integer')} ready)` : amount;
    tooltipItems.push({ label, ...(splitFill ? { swatch: color } : {}), value: detail });
    if (splitFill && count.value > 0) segments.push({ ...reading, share: count.value / row.total, color });
  }
  return {
    kind: 'data',
    key,
    subject: key,
    label: pres.label,
    media: pres.media,
    ...(pres.color ? { color: pres.color } : {}),
    value: { value, format: { decimals: 0 }, label: 'Nuclear stockpile' },
    numeric: row.total,
    ...(segments.length ? { segments } : {}),
    ...(partReadings.length ? { partReadings } : {}),
    tooltip: {
      title: barsLabelText(pres.label),
      hour: readView.hour,
      items: tooltipItems,
    },
  };
}

export const nuclearArsenalRaceProjection: BarsProjection<NuclearArsenalRaceOptions> = registerFamilyProjection({
  kind: 'nuclear-arsenal',
  family: 'bars',
  singleValueBars: options => options.split ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: NuclearArsenalRaceOptions): BarsModel {
    const hour = readView.hour;
    const baseModel = {
      form: 'ordered' as const,
      hour,
      animateOrder: true,
      showAxis: true,
      scaleUnit: 'Nuclear bombs',
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

    const reply = validateNuclearArsenal(panel);
    if (reply.missing !== null) {
      return { ...baseModel, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings };
    }

    const types = typeInfoMap(reply.types, options.surface);
    const rows = reply.countries.map(row => makeRow(readView, row, types, options.split, options.surface));

    return {
      ...baseModel,
      rows,
      warnings: reply.warnings,
      ...(!rows.length ? { empty: options.group ? GROUP_EMPTY : 'No nuclear stockpile recorded at this date.', emptyKind: 'empty' as const } : {}),
    };
  },
}, 'race');

export const nuclearArsenalRaceDeclaration: BarsWidgetDeclaration<NuclearArsenalRaceOptions> = {
  family: 'bars',
  kind: 'nuclear-arsenal',
  label: 'Nuclear arsenal',
  subject: 'nuclear stockpiles',
  emptyCampaign: 'Load a campaign to see nuclear stockpiles.',
  icon: 'target',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: ctx => nuclearArsenalSources(ctx, 'race'),
  projection: nuclearArsenalRaceProjection,
  projectionOptions: (ctx, extra) => ({ ...seriesRaceSelection(ctx, extra), split: ctx.options.split !== false }),
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
