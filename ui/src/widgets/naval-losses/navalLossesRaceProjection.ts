import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { projectForceAtReply, validateNavalLossesAtReply, type ForceAtRaceBaseOptions } from '../../families/bars/forceAtRace.ts';
import type { BarsModel, BarsProjection } from '../../families/bars/model.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings } from '../../families/bars/seriesRace.ts';
import type { BoundedReadView } from '../../families/contract.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { NavalAttribution, NavalKind, NavalPeriod } from '../../generated/dashboard';
import { NAVAL_NOTE, navalLossesAtSources } from './navalLossesAt.ts';
import { navalAttribution, navalKind, navalMeasure, navalPeriod } from './navalLossesData.ts';

export interface NavalLossesRaceOptions extends ForceAtRaceBaseOptions {
  readonly kind: NavalKind;
  readonly attribution: NavalAttribution;
  readonly period: NavalPeriod;
}

export const navalLossesRaceProjection: BarsProjection<NavalLossesRaceOptions> = registerFamilyProjection({
  kind: 'naval-losses',
  family: 'bars',
  singleValueBars: options => options.split ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: NavalLossesRaceOptions): BarsModel {
    const scaleUnit = options.kind === 'ships' ? 'Ships' : options.kind === 'convoys' ? 'Convoys' : 'Ships & convoys';
    const baseModel = { form: 'ordered' as const, hour: readView.hour, animateOrder: true, showAxis: true, scaleUnit, scaleFormat: { decimals: 0 } };
    const refused = readView.refused('main');
    if (refused) return { ...baseModel, rows: [], empty: refused, emptyKind: 'unavailable' };
    const panel = readView.panel('main');
    if (panel === undefined) return { ...baseModel, rows: [] };
    return projectForceAtReply(readView, validateNavalLossesAtReply(panel), options, {
      decimals: 0,
      valueFormat: { decimals: 0 },
      scaleFormat: { decimals: 0 },
      scaleUnit,
      valueLabel: navalMeasure({ kind: options.kind, attribution: options.attribution }),
      partsLabel: 'Ships and convoys',
      emptyMessage: options.period === 'month' ? 'No known naval losses this month.' : 'No known naval losses yet.',
      note: NAVAL_NOTE,
    });
  },
}, 'race');

export const navalLossesRaceDeclaration: BarsWidgetDeclaration<NavalLossesRaceOptions> = {
  family: 'bars',
  kind: 'naval-losses',
  label: 'Naval losses',
  subject: 'naval losses',
  emptyCampaign: 'Load a campaign to see its naval losses.',
  icon: 'shield',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: ctx => navalLossesAtSources(ctx, 'race'),
  projection: navalLossesRaceProjection,
  projectionOptions: (ctx, extra) => ({
    ...seriesRaceSelection(ctx, extra),
    kind: navalKind(ctx.options),
    attribution: navalAttribution(ctx.options),
    period: navalPeriod(ctx.options),
    split: ctx.options.split === true,
  }),
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
