import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { projectForceAtMix, type ForceAtMixOptions } from '../../families/bars/forceAtMix.ts';
import { forceAtSources, validateForceAtReply } from '../../families/bars/forceAtRace.ts';
import type { BarsModel, BarsProjection } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { buildShareRowsModel } from '../../families/bars/shareRows.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings } from '../../families/bars/seriesRace.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import type { ArmyAttribution } from '../../generated/dashboard';

export interface ArmySizeMixOptions extends ForceAtMixOptions {
  readonly countFor: ArmyAttribution;
}

const armyCountFor = (value: unknown): ArmyAttribution => (value === 'host' ? 'host' : 'owner');

export const armySizeMixProjection: BarsProjection<ArmySizeMixOptions> = registerFamilyProjection({
  kind: 'army-size',
  family: 'bars',
  singleValueBars: () => 'share',
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: ArmySizeMixOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    return projectForceAtMix(readView, validateForceAtReply(panel), options, {
      decimals: 0,
      valueFormat: { decimals: 0 },
      valueLabel: 'Battalions',
      unit: 'Battalions',
      partsLabel: 'Battalions by unit group',
      emptyMessage: 'No battalion mix recorded at this date.',
    });
  },
}, 'mix');

function armySizeMixSources(ctx: FamilyPlanContext): SourcePlan {
  const countFor = armyCountFor(ctx.options.countFor);
  return forceAtSources(ctx, `army-size:mix:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${countFor}`, 'army-at', {
    countFor,
  });
}

export const armySizeMixDeclaration: BarsWidgetDeclaration<ArmySizeMixOptions> = {
  family: 'bars',
  kind: 'army-size',
  label: 'Army composition',
  subject: 'armies',
  emptyCampaign: 'Load a campaign to see its army sizes.',
  icon: 'shield',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: armySizeMixSources,
  projection: armySizeMixProjection,
  projectionOptions: (ctx, extra) => {
    return {
      ...seriesRaceSelection(ctx, extra),
      countFor: armyCountFor(ctx.options.countFor),
    };
  },
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
