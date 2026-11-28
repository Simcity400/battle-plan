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
import type { ArmyAttribution, ArmyMeasure } from '../../generated/dashboard';
import { armyMeasure, armyMetadataParams, measureName, validateArmyMetadata, type ArmyMetadataReply } from './armySizeMeta.ts';

export interface ArmySizeRaceOptions extends SeriesRaceBaseOptions, ForceAtRaceBaseOptions {
  readonly measure: ArmyMeasure;
  readonly countFor: ArmyAttribution;
}

const armyCountFor = (value: unknown): ArmyAttribution => (value === 'host' ? 'host' : 'owner');

function seriesMeasure(measure: ArmyMeasure, countFor: ArmyAttribution): string {
  const prefix = countFor === 'host' ? 'host' : 'owner';
  switch (measure) {
    case 'manpower': return `${prefix}Manpower`;
    case 'divisions': return `${prefix}Divisions`;
    case 'strength': return `${prefix}Strength`;
    case 'battalions': return `${prefix}Battalions`;
  }
}

export const armySizeRaceProjection: BarsProjection<ArmySizeRaceOptions> = registerFamilyProjection({
  kind: 'army-size',
  family: 'bars',
  singleValueBars: options => options.split && options.measure === 'battalions' ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: ArmySizeRaceOptions): BarsModel {
    const isStrength = options.measure === 'strength';
    const scaleUnit = options.measure === 'manpower'
      ? 'Manpower'
      : options.measure === 'divisions'
        ? 'Divisions'
        : isStrength
          ? 'Strength (HP)'
          : 'Battalions';
    const valueLabel = measureName(options.measure);
    const valueFormat = isStrength ? { decimals: 0, maxDecimals: 2 } : { decimals: 0 };
    const scaleFormat = isStrength ? { decimals: 0, maxDecimals: 2 } : { decimals: 0 };
    const emptyMessage = 'No known army sizes at this date.';

    if (!options.split || options.measure !== 'battalions') {
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
      decimals: 0,
      valueFormat: { decimals: 0 },
      scaleFormat: { decimals: 0 },
      scaleUnit,
      valueLabel,
      partsLabel: 'Battalions by unit group',
      emptyMessage,
    });
  },
}, 'race');

function armySizeRaceSources(ctx: FamilyPlanContext<ArmyMetadataReply>): SourcePlan {
  const measure = armyMeasure(ctx.options.measure, ctx.aux?.measures);
  const countFor = armyCountFor(ctx.options.countFor);
  const byType = measure === 'battalions' && ctx.options.byType === true;
  if (!byType) {
    return seriesRaceSources(ctx, {
      kind: 'army-size',
      family: { kind: 'army-totals', measure: seriesMeasure(measure, countFor) },
    });
  }
  return forceAtSources(ctx, `army-size:race:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${countFor}:byType`, 'army-at', {
    countFor,
  });
}

export const armySizeRaceDeclaration: BarsWidgetDeclaration<ArmySizeRaceOptions, ArmyMetadataReply> = {
  family: 'bars',
  kind: 'army-size',
  label: 'Army size',
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
  aux: {
    product: 'army-size',
    label: 'the army types',
    params: track => ({ ...armyMetadataParams(track.branch.id) }),
    validate: validateArmyMetadata,
  },
  sources: armySizeRaceSources,
  projection: armySizeRaceProjection,
  projectionOptions: (ctx, extra) => {
    const measure = armyMeasure(ctx.options.measure, ctx.aux?.measures);
    return {
      ...seriesRaceSelection(ctx, extra),
      measure,
      countFor: armyCountFor(ctx.options.countFor),
      split: measure === 'battalions' && ctx.options.byType === true,
    };
  },
  unavailable: ctx => ctx.aux?.available === false ? ctx.aux.missing[0]?.reason ?? 'Army totals are not recorded.' : seriesRaceUnavailable(ctx),
  warnings: (ctx, followRefused) => [
    ...(ctx.aux?.warnings ?? []),
    ...seriesRaceWarnings(followRefused),
  ],
  jumpToDate: true,
  keyByBranch: true,
};
