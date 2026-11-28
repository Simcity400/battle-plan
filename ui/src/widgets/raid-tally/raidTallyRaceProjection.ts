import { colour } from '../../charts/palette.ts';
import { countryFromKey, countryKey } from '../../dashboard/countryKey.ts';
import type { GatedHour } from '../../dashboard/regionTime';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { barsLabelText, countryRowPresentation, type BarsDataRow, type BarsModel, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import type { BoundedReadView, FamilyPlanContext, SourceCoverage, SourcePlan } from '../../families/contract.ts';
import { groupSelection } from '../../families/groupSelection.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { knownValue, notRecorded, partialValue, zeroValue } from '../../families/value.ts';
import { formatScalar } from '../../families/valueFormat.ts';
import type { GroupSelection, RaidTallyAtReply, RaidTallyAtRow, RaidTallyMetric } from '../../generated/dashboard';

const NOTE = 'Success includes critical success and excludes limited success.';

export interface RaidTallyRaceOptions {
  readonly metric: RaidTallyMetric;
  readonly group: boolean;
  readonly top: number | null;
  readonly all: boolean;
  readonly surface: string;
}

const isObj = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);

export function validateRaidTallyAt(value: unknown): RaidTallyAtReply {
  if (
    !isObj(value)
    || typeof value.branchId !== 'string'
    || !Number.isSafeInteger(value.hour)
    || !Number.isSafeInteger(value.firstHour)
    || !Number.isSafeInteger(value.lastHour)
    || (value.metric !== 'both' && value.metric !== 'launched' && value.metric !== 'succeeded')
    || !(value.available === null || typeof value.available === 'boolean')
    || !(value.unavailable === null || typeof value.unavailable === 'string')
    || !Array.isArray(value.rows)
    || !Number.isSafeInteger(value.unattributed)
    || !Number.isSafeInteger(value.unattributedLaunched)
    || !Number.isSafeInteger(value.unattributedSucceeded)
    || !(value.missing === null || typeof value.missing === 'string')
    || !(value.partial === undefined || value.partial === null || typeof value.partial === 'string')
    || !Array.isArray(value.warnings)
  ) {
    throw new Error('The raid tally reply is malformed.');
  }
  return value as unknown as RaidTallyAtReply;
}

function provenInterval(data: unknown): readonly [number, number] | null {
  const reply = data as Partial<RaidTallyAtReply> | null | undefined;
  return reply && typeof reply.firstHour === 'number' && typeof reply.lastHour === 'number' && reply.firstHour <= reply.lastHour
    ? [reply.firstHour, reply.lastHour]
    : null;
}

function metricOption(value: unknown): RaidTallyMetric {
  return value === 'launched' || value === 'succeeded' ? value : 'both';
}

function countText(value: number): string {
  return formatScalar(value, { decimals: 0 });
}

interface RaidTallyExactBase {
  readonly branchId: string;
  readonly top: number | null;
  readonly all: boolean;
  readonly group: GroupSelection | null;
  readonly tags: readonly string[];
  readonly metric: RaidTallyMetric;
}

function buildExactParams(base: Readonly<Record<string, unknown>>, hour: GatedHour): Readonly<Record<string, unknown>> | null {
  const cfg = base as unknown as RaidTallyExactBase;
  if (cfg.top != null || cfg.all) {
    return {
      branchId: cfg.branchId,
      hour,
      countries: [],
      ...(cfg.top != null ? { top: cfg.top } : {}),
      metric: cfg.metric,
    };
  }
  if (cfg.group) {
    return {
      branchId: cfg.branchId,
      hour,
      countries: [],
      group: cfg.group,
      metric: cfg.metric,
    };
  }
  if (!cfg.tags.length) return null;
  return {
    branchId: cfg.branchId,
    hour,
    countries: cfg.tags.map(countryFromKey),
    metric: cfg.metric,
  };
}

export const raidTallyRaceProjection: BarsProjection<RaidTallyRaceOptions> = registerFamilyProjection({
  kind: 'raid-tally',
  family: 'bars',
  singleValueBars: options => options.metric === 'both' ? 'ordered-segments' : false,
  project(readView: BoundedReadView, options: RaidTallyRaceOptions): BarsModel {
    const hour = readView.hour;
    const ranked = options.top != null || options.all;
    const unit = options.metric === 'succeeded' ? 'Succeeded raids' : options.metric === 'launched' ? 'Launched raids' : 'Raids';
    const baseModel = {
      form: 'ordered' as const,
      hour,
      animateOrder: true,
      showAxis: true,
      scaleUnit: unit,
      scaleFormat: { decimals: 0 },
      ...(options.metric === 'both' ? { caption: 'Solid: succeeded · hatched: other launched' } : {}),
    };

    const refused = readView.refused('main');
    if (refused) {
      return { ...baseModel, rows: [], empty: refused, emptyKind: 'unavailable' };
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return { ...baseModel, rows: [] };
    }
    const reply = validateRaidTallyAt(panel);
    const warnings = [
      ...reply.warnings,
      ...(reply.unattributed > 0 ? [`${formatScalar(reply.unattributed, { decimals: 0 })} raid(s) up to this date have an unrecorded raider.`] : []),
    ];
    if (reply.available === false) {
      return {
        ...baseModel,
        rows: [],
        empty: 'No raid types available.',
        ...(reply.unavailable ? { emptyDetail: reply.unavailable } : {}),
        emptyKind: 'unavailable',
        warnings,
      };
    }
    if (reply.missing !== null) {
      return { ...baseModel, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings };
    }

    const rows: BarsDataRow[] = [];
    for (const item of reply.rows as readonly RaidTallyAtRow[]) {
      const key = countryKey(item.country);
      const pres = countryRowPresentation(readView, key);
      const subjectTag = item.tag;
      if (item.missing !== null) {
        rows.push({
          kind: 'data',
          key,
          subject: subjectTag,
          label: pres.label,
          media: pres.media,
          ...(pres.color ? { color: pres.color } : {}),
          value: { value: notRecorded(item.missing), format: { decimals: 0 } },
          numeric: null,
          tooltip: {
            title: barsLabelText(pres.label),
            hour,
            missingReason: item.missing,
            items: [{ label: unit, missingReason: item.missing }],
            note: NOTE,
          },
        });
        continue;
      }

      const primary = options.metric === 'succeeded' ? item.succeeded : item.launched;
      const partialReason = reply.partial ?? null;
      const isUnattributedOnly = partialReason !== null && reply.unattributed > 0 && partialReason.includes('unrecorded raider');
      const gapPartial = partialReason !== null && !isUnattributedOnly ? partialReason : null;
      const launchedPartial = gapPartial
        ?? (reply.unattributedLaunched > 0
          ? `${formatScalar(reply.unattributedLaunched, { decimals: 0 })} raid(s) up to this date have an unrecorded raider`
          : null);
      const succeededPartial = gapPartial
        ?? (reply.unattributedSucceeded > 0
          ? `${formatScalar(reply.unattributedSucceeded, { decimals: 0 })} raid(s) up to this date have an unrecorded raider`
          : null);
      const primaryPartial = options.metric === 'succeeded' ? succeededPartial : launchedPartial;
      const allUnattributedMatched = gapPartial === null
        && options.metric === 'both'
        && reply.unattributed === reply.unattributedLaunched
        && reply.unattributed === reply.unattributedSucceeded;
      const otherPartial = allUnattributedMatched ? null : (launchedPartial ?? succeededPartial);
      const value = primaryPartial
        ? partialValue(primary, primaryPartial)
        : primary === 0 ? zeroValue() : knownValue(primary);
      const barFill = pres.color ?? colour(options.surface, 0);
      let segments: BarsSegment[] | undefined;
      if (options.metric === 'both' && item.launched > 0) {
        const succeeded = Math.min(item.launched, item.succeeded);
        const other = item.launched - succeeded;
        segments = [];
        if (succeeded > 0) {
          segments.push({ key: 'succeeded', label: 'Succeeded', share: succeeded / item.launched, value: succeededPartial ? partialValue(succeeded, succeededPartial) : knownValue(succeeded), format: { decimals: 0 }, color: barFill });
        }
        if (other > 0) {
          segments.push({ key: 'other', label: 'Other launched', share: other / item.launched, value: otherPartial ? partialValue(other, otherPartial) : knownValue(other), format: { decimals: 0 }, color: barFill, hatched: true });
        }
      }
      const tooltipItems: TooltipListItem[] = [
        { label: 'Launches', value: countText(item.launched), ...(launchedPartial ? { partialReason: launchedPartial } : {}) },
        { label: 'Successes', value: countText(item.succeeded), ...(succeededPartial ? { partialReason: succeededPartial } : {}) },
      ];
      rows.push({
        kind: 'data',
        key,
        subject: subjectTag,
        label: pres.label,
        media: pres.media,
        ...(pres.color ? { color: pres.color } : {}),
        value: { value, format: { decimals: 0 }, label: unit },
        numeric: primary,
        ...(segments?.length ? { segments } : {}),
        readout: {
          primary: options.metric === 'both'
            ? `${countText(item.launched)} \u00b7 ${countText(item.succeeded)}`
            : countText(primary),
          ...(partialReason ? { secondary: '(partial)' } : {}),
        },
        tooltip: {
          title: barsLabelText(pres.label),
          hour,
          items: tooltipItems,
          note: NOTE,
        },
      });
    }

    if (!rows.length && reply.unattributed > 0) {
      return {
        ...baseModel,
        rows: [],
        empty: `${formatScalar(reply.unattributed, { decimals: 0 })} raid(s) recorded up to this date with an unrecorded raider.`,
        emptyKind: 'unavailable',
        warnings,
      };
    }

    if (!rows.length && reply.partial) {
      return {
        ...baseModel,
        rows: [],
        empty: reply.partial,
        emptyKind: 'unavailable',
        warnings,
      };
    }

    const empty = !rows.length
      ? (options.group
        ? 'No country of this group at this date.'
        : ranked
          ? `No raids ${options.metric === 'both' ? 'launched or succeeded' : options.metric} yet.`
          : 'No raid counts recorded at this time.')
      : undefined;

    return {
      ...baseModel,
      rows,
      ...(empty ? { empty, emptyKind: 'empty' as const } : {}),
      warnings,
    };
  },
}, 'race');

function raidTallyRaceSources(ctx: FamilyPlanContext): SourcePlan {
  const metric = metricOption(ctx.options.metric);
  const top = ctx.countries.top;
  const all = ctx.countries.all;
  const group = top == null && !all ? groupSelection(ctx.widget.countries) : null;
  const emptySelection = top == null && !all && !group && !ctx.countries.tags.length;
  const coverage: SourceCoverage = top != null
    ? { mode: 'population', form: 'top', limit: top }
    : all || group
      ? { mode: 'population', form: 'all' }
      : { mode: 'keyed', keys: ctx.countries.tags };
  const base: RaidTallyExactBase = {
    branchId: ctx.branch.id,
    top,
    all,
    group,
    tags: group ? [] : ctx.countries.tags,
    metric,
  };
  return {
    owner: `raid-tally:race:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${metric}`,
    entries: emptySelection ? [] : [{
      name: 'main',
      source: ctx.track.source,
      branch: ctx.branch.id,
      offset: ctx.track.axisOffset,
      read: {
        shape: 'point',
        channel: 'followed',
        coverage,
        product: 'raid-tally-at',
        exactProduct: 'raid-tally-at',
        followQuery: null,
        params: base as unknown as Readonly<Record<string, unknown>>,
        buildExactParams,
        dynamicKeys: ['hour'],
        provenInterval,
        validate: validateRaidTallyAt,
      },
    }],
    allowEmptyFollow: emptySelection,
  };
}

export const raidTallyRaceDeclaration: BarsWidgetDeclaration<RaidTallyRaceOptions> = {
  family: 'bars',
  kind: 'raid-tally',
  label: 'Raid tally',
  subject: 'raids',
  emptyCampaign: 'Load a campaign to see its raid tally.',
  icon: 'target',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
  },
  sources: raidTallyRaceSources,
  projection: raidTallyRaceProjection,
  projectionOptions: (ctx, extra) => {
    const top = ctx.countries.top;
    const all = ctx.countries.all;
    return {
      surface: extra.surface,
      metric: metricOption(ctx.options.metric),
      group: top == null && !all && groupSelection(ctx.widget.countries) !== null,
      top,
      all,
    };
  },
  unavailable: ctx => ctx.countries.reason,
  warnings: (_ctx, followRefused) => (followRefused ? [followRefused] : []),
  jumpToDate: true,
  keyByBranch: true,
};
