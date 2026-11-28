import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { projectForceAtMix, type ForceAtMixOptions } from '../../families/bars/forceAtMix.ts';
import { validateNavalLossesAtReply } from '../../families/bars/forceAtRace.ts';
import type { BarsModel, BarsProjection } from '../../families/bars/model.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings } from '../../families/bars/seriesRace.ts';
import { buildShareRowsModel } from '../../families/bars/shareRows.ts';
import type { BoundedReadView } from '../../families/contract.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { NavalAttribution, NavalKind, NavalPeriod } from '../../generated/dashboard';
import { NAVAL_NOTE, navalLossesAtSources } from './navalLossesAt.ts';
import { navalAttribution, navalKind, navalMeasure, navalPeriod } from './navalLossesData.ts';

export interface NavalLossesMixOptions extends ForceAtMixOptions {
  readonly kind: NavalKind;
  readonly attribution: NavalAttribution;
  readonly period: NavalPeriod;
}

export const navalLossesMixProjection: BarsProjection<NavalLossesMixOptions> = registerFamilyProjection({
  kind: 'naval-losses',
  family: 'bars',
  singleValueBars: () => 'share',
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: NavalLossesMixOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    const label = navalMeasure({ kind: options.kind, attribution: options.attribution });
    const emptyMessage = options.period === 'month' ? 'No known naval losses this month.' : 'No known naval losses yet.';
    return projectForceAtMix(readView, validateNavalLossesAtReply(panel), options, {
      decimals: 0,
      valueFormat: { decimals: 0 },
      valueLabel: label,
      unit: options.kind === 'ships' ? 'Ships' : options.kind === 'convoys' ? 'Convoys' : 'Ships & convoys',
      partsLabel: 'Ships and convoys',
      emptyMessage,
      note: NAVAL_NOTE,
    });
  },
}, 'mix');

export const navalLossesMixDeclaration: BarsWidgetDeclaration<NavalLossesMixOptions> = {
  family: 'bars',
  kind: 'naval-losses',
  label: 'Naval losses mix',
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
  sources: ctx => navalLossesAtSources(ctx, 'mix'),
  projection: navalLossesMixProjection,
  projectionOptions: (ctx, extra) => ({
    ...seriesRaceSelection(ctx, extra),
    kind: navalKind(ctx.options),
    attribution: navalAttribution(ctx.options),
    period: navalPeriod(ctx.options),
  }),
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
