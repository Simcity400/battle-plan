import { colour } from '../../charts/palette.ts';
import { countryFromKey, countryKey } from '../../dashboard/countryKey.ts';
import { contentSet } from '../../dashboard/options.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import { barsLabelText, countryRowPresentation, type BarsLegendItem, type BarsModel, type BarsProjection, type BarsSegment } from '../../families/bars/model.ts';
import { buildShareRowsModel, type ShareRowEntry } from '../../families/bars/shareRows.ts';
import type { BoundedReadView, FamilyPlanContext, SourceCoverage, SourcePlan } from '../../families/contract.ts';
import { groupSelection } from '../../families/groupSelection.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { knownValue, notRecorded } from '../../families/value.ts';
import { formatScalar, formatShare } from '../../families/valueFormat.ts';
import type { FactoryMixReply, FactoryMixRow } from '../../generated/dashboard';
import { COUNT_MODES, countMode, mixPartPresentation, type CountMode } from './factoriesData.ts';
import type { FactoriesRowMeta } from './factoriesProjection.ts';

export interface FactoriesMixOptions {
  readonly mode: CountMode;
  readonly modeLabel: string;
  readonly group: boolean;
  readonly top: number | null;
  readonly surface: string;
}

const isObj = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);

export function validateFactoryMixReply(value: unknown): FactoryMixReply {
  if (
    !isObj(value)
    || !Number.isSafeInteger(value.hour)
    || typeof value.mode !== 'string'
    || !Array.isArray(value.parts)
    || !Array.isArray(value.rows)
    || !(value.missing === null || typeof value.missing === 'string')
    || !Array.isArray(value.warnings)
  ) {
    throw new Error('The factory mix reply is malformed.');
  }
  const partsOk = value.parts.every(
    part => isObj(part)
      && typeof part.key === 'string'
      && (part.name === null || typeof part.name === 'string')
      && typeof part.damaged === 'boolean'
      && typeof part.group === 'string',
  );
  const rowsOk = value.rows.every(
    row => isObj(row)
      && (typeof row.country === 'string' || Number.isSafeInteger(row.country))
      && typeof row.tag === 'string'
      && (row.total === null || Number.isSafeInteger(row.total))
      && Array.isArray(row.values)
      && row.values.every(v => v === null || Number.isSafeInteger(v))
      && Array.isArray(row.shares)
      && row.shares.every(s => s === null || (typeof s === 'number' && Number.isFinite(s)))
      && (row.missing === null || typeof row.missing === 'string')
      && (row.partsMissing === null || typeof row.partsMissing === 'string'),
  );
  if (!partsOk || !rowsOk) throw new Error('The factory mix reply is malformed.');
  return value as unknown as FactoryMixReply;
}

function formatWhole(value: number): string {
  return formatScalar(value, { decimals: 0 });
}

export const factoriesMixProjection: BarsProjection<FactoriesMixOptions> = registerFamilyProjection({
  kind: 'factories',
  family: 'bars',
  singleValueBars: () => 'share',
  project(readView: BoundedReadView, options: FactoriesMixOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], unit: 'Factories', empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [], unit: 'Factories' });
    }
    const reply = validateFactoryMixReply(panel);
    if (reply.missing !== null) {
      return buildShareRowsModel({ hour, rows: [], unit: 'Factories', empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings });
    }

    const partInfo = reply.parts.map(part => {
      const presentation = mixPartPresentation(part);
      return {
        part,
        label: presentation.label,
        color: colour(options.surface, presentation.colorIndex),
      };
    });
    const usedParts = new Set<number>();
    const rows: ShareRowEntry[] = [];

    for (const item of reply.rows as readonly FactoryMixRow[]) {
      const key = countryKey(item.country);
      const pres = countryRowPresentation(readView, key);
      const subjectTag = item.tag;
      const meta: FactoriesRowMeta = { tag: subjectTag, key };

      const tooltipItems: TooltipListItem[] = [];
      if (item.total === null) {
        const reason = item.missing ?? 'Not recorded at this hour';
        tooltipItems.push({ label: options.modeLabel, missingReason: reason });
        for (let i = 0; i < partInfo.length; i++) {
          const raw = item.values[i] ?? null;
          if (raw === null) continue;
          const info = partInfo[i]!;
          tooltipItems.push({ label: info.label, swatch: info.color, value: formatWhole(raw) });
        }
        rows.push({
          key,
          subject: subjectTag,
          label: pres.label,
          media: pres.media,
          segments: [],
          missing: notRecorded(reason),
          format: { decimals: 0 },
          tooltip: {
            title: barsLabelText(pres.label),
            hour,
            missingReason: reason,
            items: tooltipItems,
          },
          meta,
        });
        continue;
      }

      const countText = formatWhole(item.total);
      tooltipItems.push({ label: options.modeLabel, value: countText });

      if (item.total === 0) {
        rows.push({
          key,
          subject: subjectTag,
          label: pres.label,
          media: pres.media,
          segments: [],
          segmentsIncomplete: item.partsMissing !== null || item.values.length !== partInfo.length || item.values.some(raw => raw === null),
          countText,
          tooltip: { title: barsLabelText(pres.label), hour, items: tooltipItems },
          meta,
        });
        continue;
      }

      const segments: BarsSegment[] = [];
      let anyMissingShare = false;
      for (let i = 0; i < partInfo.length; i++) {
        const info = partInfo[i]!;
        const raw = item.values[i] ?? null;
        const share = item.shares[i] ?? null;
        if (share !== null && raw !== null && raw > 0) {
          usedParts.add(i);
          const pctText = formatShare(share);
          segments.push({
            key: info.part.key,
            label: info.label,
            share,
            value: knownValue(raw),
            format: { decimals: 0 },
            color: info.color,
            ...(info.part.damaged ? { hatched: true } : {}),
            inlineLabel: `${info.label} ${pctText}`,
          });
          tooltipItems.push({
            label: info.label,
            swatch: info.color,
            value: `${formatWhole(raw)} (${pctText})`,
          });
        } else if (raw !== null) {
          tooltipItems.push({
            label: info.label,
            swatch: info.color,
            value: formatWhole(raw),
          });
          if (share === null) anyMissingShare = true;
        } else {
          anyMissingShare = true;
        }
      }

      const partReason = item.partsMissing ?? (anyMissingShare ? 'Factory breakdown not recorded at this hour' : null);
      if (partReason !== null) {
        tooltipItems.push({ label: 'Unrecorded share', missingReason: partReason });
        rows.push({
          key,
          subject: subjectTag,
          label: pres.label,
          media: pres.media,
          segments,
          countText,
          missing: notRecorded(partReason),
          format: { decimals: 0 },
          tooltip: { title: barsLabelText(pres.label), hour, missingReason: partReason, items: tooltipItems },
          meta,
        });
        continue;
      }

      rows.push({
        key,
        subject: subjectTag,
        label: pres.label,
        media: pres.media,
        segments,
        countText,
        tooltip: { title: barsLabelText(pres.label), hour, items: tooltipItems },
        meta,
      });
    }

    const legend: BarsLegendItem[] = [];
    for (let i = 0; i < partInfo.length; i++) {
      if (!usedParts.has(i)) continue;
      const info = partInfo[i]!;
      legend.push({
        key: info.part.key,
        label: info.label,
        color: info.color,
        ...(info.part.damaged ? { hatched: true } : {}),
      });
    }

    const emptyMessage = options.group ? 'No country of this group at this date.' : 'No factories recorded at this date.';
    return buildShareRowsModel({
      hour,
      rows,
      legend,
      unit: 'Factories',
      warnings: reply.warnings,
      ...(!rows.length ? { empty: emptyMessage, emptyKind: 'empty' as const } : {}),
    });
  },
}, 'mix');

function factoriesMixSources(ctx: FamilyPlanContext): SourcePlan {
  const mode = countMode(ctx.options.count);
  const types = contentSet(ctx.options.types);
  const top = ctx.countries.top;
  const all = ctx.countries.all;
  const group = top == null && !all ? groupSelection(ctx.widget.countries) : null;
  const countries = top == null && !all && !group ? ctx.countries.tags.map(countryFromKey) : [];
  const coverage: SourceCoverage = top != null
    ? { mode: 'population', form: 'top', limit: top }
    : all || group
      ? { mode: 'population', form: 'all' }
      : { mode: 'keyed', keys: ctx.countries.tags };
  const params = {
    branchId: ctx.branch.id,
    countries,
    ...(group ? { group } : {}),
    ...(top != null ? { top } : {}),
    mode,
    types,
  };
  const emptyExplicit = top == null && !all && !group && !countries.length;
  return {
    owner: `factories:mix:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${mode}:${types.join(',')}`,
    entries: emptyExplicit ? [] : [{
      name: 'main',
      source: ctx.track.source,
      branch: ctx.branch.id,
      offset: ctx.track.axisOffset,
      read: {
        shape: 'point',
        channel: 'panel',
        coverage,
        product: 'factory-mix',
        followQuery: { panel: 'factory-mix', ...params },
        params,
        validate: validateFactoryMixReply,
      },
    }],
    allowEmptyFollow: emptyExplicit,
  };
}

export const factoriesMixDeclaration: BarsWidgetDeclaration<FactoriesMixOptions> = {
  family: 'bars',
  kind: 'factories',
  label: 'Factory composition',
  subject: 'factories',
  emptyCampaign: 'Load a campaign to see factories.',
  icon: 'factory',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    everyOnTop: true,
  },
  sources: factoriesMixSources,
  projection: factoriesMixProjection,
  projectionOptions: (ctx, extra) => {
    const mode = countMode(ctx.options.count);
    const modeTitle = COUNT_MODES.find(item => item.value === mode)?.label.toLowerCase() ?? 'available';
    const top = ctx.countries.top;
    const all = ctx.countries.all;
    return {
      mode,
      modeLabel: `Factories, ${modeTitle}`,
      group: top == null && !all && groupSelection(ctx.widget.countries) !== null,
      top,
      surface: extra.surface,
    };
  },
  unavailable: ctx => ctx.countries.reason,
  warnings: (_ctx, followRefused) => (followRefused ? [followRefused] : []),
  jumpToDate: true,
  keyByBranch: true,
};
