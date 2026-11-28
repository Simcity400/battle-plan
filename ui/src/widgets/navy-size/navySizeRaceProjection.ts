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

export type NavyMeasureOption = 'ships' | 'strength';
export type NavyOwnershipOption = 'roster' | 'logical';
export type NavyGroupingOption = 'class' | 'type';

export interface NavySizeRaceOptions extends SeriesRaceBaseOptions, ForceAtRaceBaseOptions {
  readonly measure: NavyMeasureOption;
  readonly ownership: NavyOwnershipOption;
  readonly grouping: NavyGroupingOption;
}

const navyMeasure = (value: unknown): NavyMeasureOption => (value === 'strength' ? 'strength' : 'ships');
const navyOwnership = (value: unknown): NavyOwnershipOption => (value === 'logical' ? 'logical' : 'roster');
const navyGrouping = (value: unknown): NavyGroupingOption => (value === 'type' ? 'type' : 'class');

export const navySizeRaceProjection: BarsProjection<NavySizeRaceOptions> = registerFamilyProjection({
  kind: 'navy-size',
  family: 'bars',
  singleValueBars: options => options.split ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: NavySizeRaceOptions): BarsModel {
    const strength = options.measure === 'strength';
    const scaleUnit = strength ? 'Hull HP' : 'Ships';
    const valueLabel = strength ? 'Current hull strength (HP)' : 'Number of ships';
    const emptyMessage = strength ? 'No hull strength recorded at this date.' : 'No ships recorded at this date.';
    const valueFormat = strength ? { decimals: 0, maxDecimals: 2 } : { decimals: 0 };
    const scaleFormat = strength ? { decimals: 0, maxDecimals: 2 } : { decimals: 0 };

    if (!options.split && !strength) {
      return projectSeriesRace(readView, options, {
        valueFormat,
        scaleFormat,
        scaleUnit,
        valueLabel,
        emptyMessage,
      });
    }

    const hour = readView.hour;

    const refused = readView.refused('main');
    if (refused) {
      return { form: 'ordered', hour, animateOrder: true, showAxis: true, scaleUnit, scaleFormat, rows: [], empty: refused, emptyKind: 'unavailable' };
    }

    const panel = readView.panel('main');
    if (panel === undefined) {
      return { form: 'ordered', hour, animateOrder: true, showAxis: true, scaleUnit, scaleFormat, rows: [] };
    }

    return projectForceAtReply(readView, validateForceAtReply(panel), options, {
      decimals: strength ? 5 : 0,
      valueFormat,
      scaleFormat,
      scaleUnit,
      valueLabel,
      partsLabel: options.grouping === 'type' ? 'Roles' : 'Ship classes',
      emptyMessage,
      ...(strength ? { referenceLabel: 'Maximum hull strength (HP)' } : {}),
    });
  },
}, 'race');

function navySizeRaceSources(ctx: FamilyPlanContext): SourcePlan {
  const measure = navyMeasure(ctx.options.measure);
  const ownership = navyOwnership(ctx.options.ownership);
  const grouping = navyGrouping(ctx.options.grouping);
  const split = ctx.options.split === true;
  if (!split && measure === 'ships') {
    return seriesRaceSources(ctx, {
      kind: 'navy-size',
      family: { kind: 'navy-totals', measure: `${ownership}Ships` },
    });
  }
  return forceAtSources(ctx, `navy-size:race:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${measure}:${ownership}:${grouping}:${split ? 1 : 0}`, 'navy-at', {
    ownership,
    grouping,
    measure,
  });
}

export const navySizeRaceDeclaration: BarsWidgetDeclaration<NavySizeRaceOptions> = {
  family: 'bars',
  kind: 'navy-size',
  label: 'Navy size',
  subject: 'navies',
  emptyCampaign: 'Load a campaign to see its navies.',
  icon: 'flows',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: navySizeRaceSources,
  projection: navySizeRaceProjection,
  projectionOptions: (ctx, extra) => {
    return {
      ...seriesRaceSelection(ctx, extra),
      measure: navyMeasure(ctx.options.measure),
      ownership: navyOwnership(ctx.options.ownership),
      grouping: navyGrouping(ctx.options.grouping),
      split: ctx.options.split === true,
    };
  },
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
