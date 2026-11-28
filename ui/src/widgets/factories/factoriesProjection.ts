import { rankSource } from '../../families/seriesRank.ts';
import { colour } from '../../charts/palette.ts';
import { validateRankReply } from '../../charts/race/measureRaceData.ts';
import { countryKey } from '../../dashboard/countryKey.ts';
import { contentSet } from '../../dashboard/options.ts';
import { barsLabelText, countryRowPresentation, formatBarsValueField, type BarsDataRow, type BarsModel, type BarsPartReading, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import type { BoundedReadView, FamilyPlanContext, SourceCoverage, SourcePlan } from '../../families/contract.ts';
import { groupSelection } from '../../families/groupSelection.ts';
import { holderTagAt, nextHoldersChange, type HolderRun } from '../../families/holders.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notRecorded, valueFromRawBigInt, zeroValue, type Value } from '../../families/value.ts';
import { formatScalar } from '../../families/valueFormat.ts';
import type { FactoryGroupsReply, SeriesRankReply, SeriesState } from '../../generated/dashboard';
import { COUNT_MODES, chosenGroups, countMode, damagedGroupLabel, damagedMeasure, factoryGroupPresentation, factoryMeasure, validateFactoryGroups, type CountMode } from './factoriesData.ts';

export interface FactoryPartSpec {
  readonly id: string;
  readonly label: string;
  readonly source: string;
  readonly damagedSource?: string;
  readonly color: string;
}

export interface FactoriesProjectionOptions {
  readonly mode: CountMode;
  readonly modeLabel: string;
  readonly split: boolean;
  readonly parts: readonly FactoryPartSpec[];
  readonly damagedSource?: string;
  readonly holders?: Readonly<Record<string, readonly HolderRun[]>> | null;
  readonly group: boolean;
  readonly top: number | null;
  readonly surface: string;
}

export interface FactoriesRowMeta {
  readonly tag: string;
  readonly key: string;
}

function partSpecs(mode: CountMode, types: readonly string[], groups: FactoryGroupsReply | undefined, surface: string): FactoryPartSpec[] {
  return chosenGroups(types).map(group => {
    const presentation = factoryGroupPresentation(group.value, groups);
    return {
      id: presentation.key,
      label: presentation.label,
      source: `${mode}${group.bit}`,
      ...(mode === 'built' ? { damagedSource: `damaged${group.bit}` } : {}),
      color: colour(surface, presentation.colorIndex),
    };
  });
}

const PART_NOT_READ = 'Not recorded at this hour';

type PanelIndex = (source: string) => ReadonlyMap<string, SeriesState> | null;

function panelIndex(reply: SeriesRankReply): PanelIndex {
  const held = new Map<string, ReadonlyMap<string, SeriesState> | null>();
  return measure => {
    let index = held.get(measure);
    if (index === undefined) {
      const at = reply.measures.findIndex(asked => asked.name === measure);
      index = at < 0 ? null : new Map(reply.rows.map(row => [countryKey(row.country), row.readings[at]!]));
      held.set(measure, index);
    }
    return index;
  };
}

function partValue(index: PanelIndex, source: string, key: string): Value<bigint | number> | null {
  const states = index(source);
  if (states === null) return null;
  const state = states.get(key);
  if (!state) return notRecorded(PART_NOT_READ);
  return valueFromRawBigInt(state.run?.valueRaw ?? null, { kind: 'notRecorded', reason: state.missing ?? PART_NOT_READ });
}

function reasonOf(value: Value<bigint | number>): string {
  return value.kind === 'notRecorded' || value.kind === 'notApplicable' ? value.reason : PART_NOT_READ;
}

function numericOf(value: Value<bigint | number>): number | null {
  if (value.kind === 'known' || value.kind === 'partial') return Number(value.value);
  if (value.kind === 'zero') return 0;
  return null;
}

function scalarOf(raw: bigint | number): Value<bigint | number> {
  return raw === 0 || raw === 0n ? zeroValue() : knownValue(raw);
}

function countText(amount: number): string {
  return formatScalar(amount, 'integer');
}

function segment(key: string, label: string, amount: number, total: number, color: string, hatched = false): BarsSegment {
  return { key, label, share: amount / total, value: scalarOf(amount), format: { decimals: 0 }, color, ...(hatched ? { hatched } : {}) };
}

function buildPartSegments(
  index: PanelIndex,
  wireName: string,
  total: number | null,
  options: FactoriesProjectionOptions,
  baseColor: string | undefined,
): Pick<BarsDataRow, 'segments' | 'segmentsIncomplete' | 'partReadings'> & { readonly tooltipItems: TooltipListItem[] } {
  if (!options.split && options.mode !== 'built') return { tooltipItems: [] };

  const tooltipItems: TooltipListItem[] = [];
  if (!options.split) {
    const requestedDamage = options.damagedSource ? partValue(index, options.damagedSource, wireName) : null;
    const damaged = requestedDamage ?? notRecorded(PART_NOT_READ);
    const partReadings = [{ key: 'damaged', label: 'Damaged', value: damaged }];
    if (total === null) return { tooltipItems, partReadings };
    const amount = numericOf(damaged);
    if (amount === null) {
      tooltipItems.push({ label: 'Damaged', missingReason: reasonOf(damaged) });
      return { tooltipItems, partReadings, segmentsIncomplete: true };
    }
    if (total <= 0) return { tooltipItems, partReadings };
    const hurt = Math.min(total, Math.max(0, amount));
    tooltipItems.push({ label: 'Damaged', value: countText(hurt) });
    if (hurt <= 0) return { tooltipItems, partReadings };
    const fill = baseColor ?? colour(options.surface, 0);
    const segments: BarsSegment[] = [];
    if (total > hurt) segments.push(segment('healthy', 'Undamaged', total - hurt, total, fill));
    segments.push(segment('damaged', 'Damaged', hurt, total, fill, true));
    return { segments, tooltipItems, partReadings };
  }

  const segments: BarsSegment[] = [];
  const partReadings: BarsPartReading[] = [];
  let known = 0;
  let unknownReason: string | null = null;
  let damageUnknown = false;
  let measureAbsent = false;
  for (const spec of options.parts) {
    const requestedPart = partValue(index, spec.source, wireName);
    measureAbsent ||= requestedPart === null;
    const value = requestedPart ?? notRecorded(PART_NOT_READ);
    partReadings.push({ key: spec.id, label: spec.label, value });
    const damagedLabel = damagedGroupLabel({ label: spec.label });
    const damagedValue = spec.damagedSource === undefined ? null : partValue(index, spec.damagedSource, wireName) ?? notRecorded(PART_NOT_READ);
    if (damagedValue !== null) partReadings.push({ key: `damaged:${spec.id}`, label: damagedLabel, value: damagedValue });
    if (total === null) continue;
    const amount = numericOf(value);
    if (amount === null) {
      unknownReason ??= reasonOf(value);
      if (total > 0) tooltipItems.push({ label: spec.label, swatch: spec.color, missingReason: reasonOf(value) });
      continue;
    }
    const damaged = damagedValue === null ? null : numericOf(damagedValue);
    const damageMissing = damagedValue !== null && damaged === null;
    if (total > 0 || damageMissing) {
      tooltipItems.push({ label: spec.label, swatch: spec.color, value: countText(amount) });
    }
    if (damageMissing) {
      damageUnknown = true;
      tooltipItems.push({ label: damagedLabel, swatch: spec.color, missingReason: reasonOf(damagedValue) });
    }
    if (total <= 0) continue;
    const whole = Math.max(0, amount);
    if (whole <= 0) continue;
    known += whole;
    const hurt = damaged === null ? 0 : Math.min(whole, Math.max(0, damaged));
    if (whole > hurt) segments.push(segment(spec.id, spec.label, whole - hurt, total, spec.color));
    if (hurt > 0) segments.push(segment(`damaged:${spec.id}`, damagedLabel, hurt, total, spec.color, true));
  }
  if (unknownReason !== null && total !== null && total > known) {
    segments.push({ key: 'unrecorded', label: 'Not recorded', share: (total - known) / total, value: notRecorded(unknownReason), color: options.surface, hatched: true });
  }
  return {
    ...(!measureAbsent && segments.length ? { segments } : {}),
    ...(measureAbsent || unknownReason !== null || damageUnknown ? { segmentsIncomplete: true } : {}),
    tooltipItems,
    partReadings,
  };
}

function makeRow(
  readView: BoundedReadView,
  index: PanelIndex,
  key: string,
  displayTag: string,
  wireName: string,
  value: Value<bigint | number>,
  options: FactoriesProjectionOptions,
): BarsDataRow {
  const pres = countryRowPresentation(readView, displayTag);
  const numeric = numericOf(value);
  const { tooltipItems, ...parts } = buildPartSegments(index, wireName, numeric, options, pres.color);
  const totalItem: TooltipListItem = value.kind === 'notRecorded'
    ? { label: options.modeLabel, missingReason: value.reason }
    : value.kind === 'notApplicable'
      ? { label: options.modeLabel, value: '\u2014' }
      : { label: options.modeLabel, value: formatBarsValueField({ value, format: { decimals: 0 } }).text };
  const meta: FactoriesRowMeta = { tag: displayTag, key };
  return {
    kind: 'data',
    key,
    subject: displayTag,
    label: pres.label,
    media: pres.media,
    ...(pres.color ? { color: pres.color } : {}),
    value: {
      value,
      format: { decimals: 0 },
      label: options.modeLabel,
    },
    numeric,
    ...parts,
    tooltip: {
      title: barsLabelText(pres.label),
      hour: readView.hour,
      items: [totalItem, ...tooltipItems],
    },
    meta,
  };
}

export const factoriesProjection: BarsProjection<FactoriesProjectionOptions> = registerFamilyProjection({
  kind: 'factories',
  family: 'bars',
  singleValueBars: options => options.split || options.mode === 'built' ? 'ordered-segments' : true,
  nextChange(readView, options) {
    const hour = readView.hour;
    return nextHoldersChange(options.holders, hour);
  },
  project(readView: BoundedReadView, options: FactoriesProjectionOptions): BarsModel {
    const hour = readView.hour;
    const baseModel = {
      form: 'ordered' as const,
      hour,
      animateOrder: true,
      showAxis: true,
      scaleUnit: 'Factories',
      scaleFormat: { decimals: 0 },
    };

    const panel = readView.panel('main');
    if (panel !== undefined) {
      const reply = validateRankReply(panel);
      if (!reply.rows.length && reply.refused) return { ...baseModel, rows: [], empty: reply.refused, emptyKind: 'unavailable' };
      const index = panelIndex(reply);
      const rows: BarsDataRow[] = [];
      for (const row of reply.rows) {
        const state = row.readings[0]!;
        const key = countryKey(row.country);
        const displayTag = options.holders ? (holderTagAt(options.holders[key], hour) ?? key) : key;
        const value = valueFromRawBigInt(
          state.run?.valueRaw ?? null,
          { kind: 'notRecorded', reason: state.missing ?? 'Not recorded at this hour' },
        );
        rows.push(makeRow(readView, index, key, displayTag, key, value, options));
      }
      return {
        ...baseModel,
        rows,
        ...(!rows.length ? { empty: options.group ? 'No country of this group at this date.' : 'No factories recorded at this date.', emptyKind: 'empty' as const } : {}),
      };
    }
    const refused = readView.refused('main');
    if (refused) return { ...baseModel, rows: [], empty: refused, emptyKind: 'unavailable' };
    return { ...baseModel, rows: [] };
  },
}, 'race');

function factoriesSources(ctx: FamilyPlanContext<FactoryGroupsReply>): SourcePlan {
  const mode = countMode(ctx.options.count);
  const types = contentSet(ctx.options.types);
  const measureName = factoryMeasure(mode, types);
  const top = ctx.countries.top;
  const split = ctx.options.split === true;
  const owner = `factories:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${measureName}:${split ? 1 : 0}`;
  const group = top != null ? null : groupSelection(ctx.widget.countries);
  const coverage: SourceCoverage = top != null
    ? { mode: 'population', form: 'top', limit: top }
    : group ? { mode: 'population', form: 'all' } : { mode: 'keyed', keys: ctx.countries.tags };
  const measures = split
    ? chosenGroups(types).flatMap(group => [`${mode}${group.bit}`, ...(mode === 'built' ? [`damaged${group.bit}`] : [])])
    : mode === 'built' ? [damagedMeasure(types)] : [];
  return {
    owner,
    entries: rankSource(ctx, 'main', { kind: 'country-factories', measure: measureName }, coverage, ctx.countries.tags, top, group, measures),
  };
}

export const factoriesDeclaration: BarsWidgetDeclaration<FactoriesProjectionOptions, FactoryGroupsReply> = {
  family: 'bars',
  kind: 'factories',
  label: 'Factories',
  subject: 'factories',
  emptyCampaign: 'Load a campaign to see factories.',
  icon: 'factory',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
  },
  aux: {
    product: 'factory-groups',
    label: 'the factory groups',
    params: track => ({ branchId: track.branch.id }),
    validate: validateFactoryGroups,
  },
  sources: factoriesSources,
  projection: factoriesProjection,
  projectionOptions: (ctx, extra) => {
    const mode = countMode(ctx.options.count);
    const types = contentSet(ctx.options.types);
    const modeTitle = COUNT_MODES.find(item => item.value === mode)?.label.toLowerCase() ?? 'available';
    const top = ctx.countries.top;
    return {
      mode,
      modeLabel: `Factories, ${modeTitle}`,
      split: ctx.options.split === true,
      parts: partSpecs(mode, types, ctx.aux, extra.surface),
      ...(mode === 'built' ? { damagedSource: damagedMeasure(types) } : {}),
      holders: top == null ? ctx.countries.holders : null,
      group: top == null && groupSelection(ctx.widget.countries) !== null,
      top,
      surface: extra.surface,
    };
  },
  unavailable: ctx => {
    if (ctx.countries.top == null && ctx.following && !ctx.countries.tags.length && !groupSelection(ctx.widget.countries)) {
      return 'No countries available for this selection.';
    }
    return null;
  },
  warnings: (ctx, followRefused) => {
    const list: string[] = [];
    if (ctx.aux?.missing) list.push(ctx.aux.missing);
    if (ctx.aux?.warnings.length) list.push(...ctx.aux.warnings);
    if (followRefused) list.push(`Values are not followed here: ${followRefused}.`);
    return list;
  },
  jumpToDate: true,
  keyByBranch: true,
};
