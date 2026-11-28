import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { type ForceAtRaceBaseOptions, forceAtSources, projectForceAtReply, validateForceAtReply } from '../../families/bars/forceAtRace.ts';
import type { BarsModel, BarsProjection } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import {
  projectSeriesRace,
  seriesRaceSelection,
  seriesRaceSources,
  seriesRaceUnavailable,
  seriesRaceWarnings,
  type SeriesRaceBaseOptions,
} from '../../families/bars/seriesRace.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';

export type AirForceMeasureOption = 'planes' | 'wings';
export type AirForceOwnershipOption = 'roster' | 'effective';

export interface AirForceSizeRaceOptions extends SeriesRaceBaseOptions, ForceAtRaceBaseOptions {
  readonly measure: AirForceMeasureOption;
  readonly ownership: AirForceOwnershipOption;
}

const airMeasure = (value: unknown): AirForceMeasureOption => (value === 'wings' ? 'wings' : 'planes');
const airOwnership = (value: unknown): AirForceOwnershipOption => (value === 'effective' ? 'effective' : 'roster');

function totalsFamily(measure: AirForceMeasureOption, ownership: AirForceOwnershipOption) {
  return { kind: 'air-force-totals', measure: `${ownership}${measure === 'wings' ? 'Wings' : 'Planes'}` };
}

export const airForceSizeRaceProjection: BarsProjection<AirForceSizeRaceOptions> = registerFamilyProjection({
  kind: 'air-force-size',
  family: 'bars',
  singleValueBars: options => options.split ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: AirForceSizeRaceOptions): BarsModel {
    const scaleUnit = options.measure === 'wings' ? 'Air wings' : 'Planes';
    const valueLabel = options.measure === 'wings' ? 'Number of wings' : 'Number of planes';
    const emptyMessage = options.measure === 'wings' ? 'No air wings recorded at this date.' : 'No planes in wings recorded at this date.';
    const note = options.measure === 'planes' ? 'Planes in wings; stockpiled planes are not included.' : undefined;

    if (!options.split) {
      return projectSeriesRace(readView, options, {
        valueFormat: { decimals: 0 },
        scaleFormat: { decimals: 0 },
        scaleUnit,
        valueLabel,
        emptyMessage,
        ...(note ? { note } : {}),
      });
    }

    const hour = readView.hour;

    const refused = readView.refused('main');
    if (refused) {
      return { form: 'ordered', hour, animateOrder: true, showAxis: true, scaleUnit, scaleFormat: { decimals: 0 }, rows: [], empty: refused, emptyKind: 'unavailable' };
    }

    const panel = readView.panel('main');
    if (panel === undefined) {
      return { form: 'ordered', hour, animateOrder: true, showAxis: true, scaleUnit, scaleFormat: { decimals: 0 }, rows: [] };
    }

    return projectForceAtReply(readView, validateForceAtReply(panel), options, {
      decimals: 0,
      valueFormat: { decimals: 0 },
      scaleFormat: { decimals: 0 },
      scaleUnit,
      valueLabel,
      partsLabel: 'Aircraft types',
      emptyMessage,
      ...(note ? { note } : {}),
    });
  },
}, 'race');

function airForceSizeRaceSources(ctx: FamilyPlanContext): SourcePlan {
  const measure = airMeasure(ctx.options.measure);
  const ownership = airOwnership(ctx.options.ownership);
  const split = ctx.options.split === true;
  if (!split) {
    return seriesRaceSources(ctx, { kind: 'air-force-size', family: totalsFamily(measure, ownership) });
  }
  return forceAtSources(ctx, `air-force-size:race:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${measure}:${ownership}:split`, 'air-force-at', {
    measure,
    ownership,
  });
}

export const airForceSizeRaceDeclaration: BarsWidgetDeclaration<AirForceSizeRaceOptions> = {
  family: 'bars',
  kind: 'air-force-size',
  label: 'Air force size',
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
  sources: airForceSizeRaceSources,
  projection: airForceSizeRaceProjection,
  projectionOptions: (ctx, extra) => {
    return {
      ...seriesRaceSelection(ctx, extra),
      measure: airMeasure(ctx.options.measure),
      ownership: airOwnership(ctx.options.ownership),
      split: ctx.options.split === true,
    };
  },
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
