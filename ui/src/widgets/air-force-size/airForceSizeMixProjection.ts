import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { projectForceAtMix, type ForceAtMixOptions } from '../../families/bars/forceAtMix.ts';
import { forceAtSources, validateForceAtReply } from '../../families/bars/forceAtRace.ts';
import type { BarsModel, BarsProjection } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { buildShareRowsModel } from '../../families/bars/shareRows.ts';
import { seriesRaceSelection, seriesRaceUnavailable, seriesRaceWarnings } from '../../families/bars/seriesRace.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import type { AirForceMeasureOption, AirForceOwnershipOption } from './airForceSizeRaceProjection.ts';

export interface AirForceSizeMixOptions extends ForceAtMixOptions {
  readonly measure: AirForceMeasureOption;
  readonly ownership: AirForceOwnershipOption;
}

const airMeasure = (value: unknown): AirForceMeasureOption => (value === 'wings' ? 'wings' : 'planes');
const airOwnership = (value: unknown): AirForceOwnershipOption => (value === 'effective' ? 'effective' : 'roster');

export const airForceSizeMixProjection: BarsProjection<AirForceSizeMixOptions> = registerFamilyProjection({
  kind: 'air-force-size',
  family: 'bars',
  singleValueBars: () => 'share',
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: AirForceSizeMixOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    const valueLabel = options.measure === 'wings' ? 'Number of wings' : 'Number of planes';
    const emptyMessage = options.measure === 'wings' ? 'No air wings recorded at this date.' : 'No planes in wings recorded at this date.';
    const note = options.measure === 'planes' ? 'Planes in wings; stockpiled planes are not included.' : undefined;
    return projectForceAtMix(readView, validateForceAtReply(panel), options, {
      decimals: 0,
      valueFormat: { decimals: 0 },
      valueLabel,
      unit: options.measure === 'wings' ? 'Wings' : 'Planes',
      partsLabel: 'Aircraft types',
      emptyMessage,
      ...(note ? { note } : {}),
    });
  },
}, 'mix');

function airForceSizeMixSources(ctx: FamilyPlanContext): SourcePlan {
  const measure = airMeasure(ctx.options.measure);
  const ownership = airOwnership(ctx.options.ownership);
  return forceAtSources(ctx, `air-force-size:mix:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${measure}:${ownership}`, 'air-force-at', {
    measure,
    ownership,
  });
}

export const airForceSizeMixDeclaration: BarsWidgetDeclaration<AirForceSizeMixOptions> = {
  family: 'bars',
  kind: 'air-force-size',
  label: 'Air force composition',
  subject: 'air forces',
  emptyCampaign: 'Load a campaign to see its air forces.',
  icon: 'layers',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: airForceSizeMixSources,
  projection: airForceSizeMixProjection,
  projectionOptions: (ctx, extra) => {
    return {
      ...seriesRaceSelection(ctx, extra),
      measure: airMeasure(ctx.options.measure),
      ownership: airOwnership(ctx.options.ownership),
    };
  },
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
