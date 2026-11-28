import { colour } from '../../charts/palette.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { GROUP_EMPTY } from '../../families/bars/forceAtRace.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings, type SeriesRaceBaseOptions } from '../../families/bars/seriesRace.ts';
import { barsLabelText, type BarsLegendItem, type BarsModel, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { buildShareRowsModel, type ShareRowEntry } from '../../families/bars/shareRows.ts';
import type { BoundedReadView } from '../../families/contract.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notRecorded } from '../../families/value.ts';
import { formatScalar, formatShare } from '../../families/valueFormat.ts';
import type { NuclearArsenalReply, NukeType } from '../../generated/dashboard';
import { nuclearArsenalSources, nukeRowIdentity, typeName, validateNuclearArsenal } from './nuclearArsenalData.ts';

export type NuclearArsenalMixOptions = SeriesRaceBaseOptions;

function sharePct(share: number): string {
  return formatShare(share);
}

export const nuclearArsenalMixProjection: BarsProjection<NuclearArsenalMixOptions> = registerFamilyProjection({
  kind: 'nuclear-arsenal',
  family: 'bars',
  singleValueBars: () => 'share',
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: NuclearArsenalMixOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    const reply: NuclearArsenalReply = validateNuclearArsenal(panel);
    if (reply.missing !== null) {
      return buildShareRowsModel({ hour, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings });
    }

    const typeByOrdinal = new Map<number, { readonly type: NukeType; readonly color: string; readonly label: string }>();
    for (const type of reply.types) typeByOrdinal.set(type.ordinal, { type, color: colour(options.surface, type.ordinal), label: typeName(type) });

    const usedOrdinals = new Set<number>();
    const rows: ShareRowEntry[] = [];
    for (const row of reply.countries) {
      const { key, pres } = nukeRowIdentity(readView, row);
      const countText = formatScalar(row.total, { decimals: 0 });
      const tooltipItems: TooltipListItem[] = [{ label: 'Nuclear stockpile', value: countText }];
      if (row.total <= 0) {
        rows.push({
          key,
          subject: key,
          label: pres.label,
          media: pres.media,
          segments: [],
          countText,
          tooltip: { title: barsLabelText(pres.label), hour, items: tooltipItems },
        });
        continue;
      }

      const positive = row.counts.filter(c => c.value > 0);
      const partsSum = positive.reduce((sum, c) => sum + c.value, 0);
      if (partsSum !== row.total) {
        const reason = 'Weapon type breakdown does not reconcile with the recorded total at this hour';
        tooltipItems.push({ label: 'Weapon types', missingReason: reason });
        rows.push({
          key,
          subject: key,
          label: pres.label,
          media: pres.media,
          segments: [],
          countText,
          missing: notRecorded(reason),
          tooltip: { title: barsLabelText(pres.label), hour, missingReason: reason, items: tooltipItems },
        });
        continue;
      }

      const segments: BarsSegment[] = [];
      for (const count of positive) {
        const info = typeByOrdinal.get(count.ordinal);
        const label = info?.label ?? `Unnamed nuclear type ${count.ordinal}`;
        const color = info?.color ?? colour(options.surface, count.ordinal);
        const share = count.value / row.total;
        usedOrdinals.add(count.ordinal);
        segments.push({
          key: String(count.ordinal),
          label,
          share,
          value: knownValue(count.value),
          format: { decimals: 0 },
          color,
          inlineLabel: `${label} ${sharePct(share)}`,
        });
        tooltipItems.push({
          label,
          swatch: color,
          value: `${formatScalar(count.value, { decimals: 0 })} (${sharePct(share)})`,
        });
      }

      rows.push({
        key,
        subject: key,
        label: pres.label,
        media: pres.media,
        segments,
        countText,
        tooltip: { title: barsLabelText(pres.label), hour, items: tooltipItems },
      });
    }

    const legend: BarsLegendItem[] = [];
    for (const type of reply.types) {
      if (!usedOrdinals.has(type.ordinal)) continue;
      const info = typeByOrdinal.get(type.ordinal)!;
      legend.push({ key: String(type.ordinal), label: info.label, color: info.color });
    }

    return buildShareRowsModel({
      hour,
      rows,
      legend,
      unit: 'Warheads',
      warnings: reply.warnings,
      ...(!rows.length ? { empty: options.group ? GROUP_EMPTY : 'No nuclear stockpile recorded at this date.', emptyKind: 'empty' as const } : {}),
    });
  },
}, 'mix');

export const nuclearArsenalMixDeclaration: BarsWidgetDeclaration<NuclearArsenalMixOptions> = {
  family: 'bars',
  kind: 'nuclear-arsenal',
  label: 'Nuclear arsenal composition',
  subject: 'nuclear arsenals',
  emptyCampaign: 'Load a campaign to see its nuclear arsenal.',
  icon: 'target',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: ctx => nuclearArsenalSources(ctx, 'mix'),
  projection: nuclearArsenalMixProjection,
  projectionOptions: seriesRaceSelection,
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
