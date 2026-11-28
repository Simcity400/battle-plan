import { rankSource } from '../seriesRank.ts';
import { validateRankReply } from '../../charts/race/measureRaceData.ts';
import { countryKey } from '../../dashboard/countryKey.ts';
import type { SeriesRankReply, SeriesRankRow } from '../../generated/dashboard';
import type { BoundedReadView, FamilyPlanContext, SourceCoverage, SourcePlan } from '../contract.ts';
import { groupSelection } from '../groupSelection.ts';
import type { DeclaredProjectionExtra } from '../host/DeclaredFamilyWidget.tsx';
import type { TooltipListItem } from '../host/tooltips.ts';
import { knownValue, notRecorded, valueFromRawBigInt, zeroValue, type Value } from '../value.ts';
import { formatValueState, type ValueFormat } from '../valueFormat.ts';
import { barsLabelText, countryRowPresentation, type BarsDataRow, type CountryRowPresentation, type BarsModel } from './model.ts';

export interface SeriesRaceBaseOptions {
  readonly top: number | null;
  readonly all?: boolean;
  readonly group: boolean;
  readonly surface: string;
}

export interface SeriesRaceConfig {
  readonly kind: string;
  readonly family: { readonly kind: string; readonly measure: string };
  readonly measures?: readonly string[];
}

export interface SeriesRaceParts extends Pick<BarsDataRow, 'segments' | 'segmentsIncomplete'> {
  readonly items: readonly TooltipListItem[];
}

export interface SeriesRaceProjectConfig {
  readonly valueFormat: ValueFormat;
  readonly scaleFormat: ValueFormat;
  readonly scaleUnit: string;
  readonly valueLabel: string;
  readonly emptyMessage: string;
  readonly note?: string;
  readonly parts?: (reply: SeriesRankReply, row: SeriesRankRow) => SeriesRaceParts;
}

export function seriesRaceSelection(ctx: FamilyPlanContext, extra: DeclaredProjectionExtra): SeriesRaceBaseOptions {
  return {
    surface: extra.surface,
    top: ctx.countries.top,
    all: ctx.countries.all,
    group: !ctx.countries.all && ctx.countries.top == null && groupSelection(ctx.widget.countries) !== null,
  };
}

export function seriesRaceSources(ctx: FamilyPlanContext, config: SeriesRaceConfig): SourcePlan {
  const top = ctx.countries.top;
  const group = top == null ? groupSelection(ctx.widget.countries) : null;
  const coverage: SourceCoverage = ctx.countries.top != null
    ? { mode: 'population', form: 'top', limit: ctx.countries.top }
    : ctx.countries.all || group
      ? { mode: 'population', form: 'all' }
      : { mode: 'keyed', keys: ctx.countries.tags };
  return {
    owner: `${config.kind}:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${config.family.kind}:${config.family.measure}`,
    entries: rankSource(ctx, 'main', config.family, coverage, ctx.countries.tags, top, group, config.measures),
  };
}

export function seriesRaceUnavailable(ctx: FamilyPlanContext): string | null {
  if (!ctx.countries.all && ctx.countries.top == null && groupSelection(ctx.widget.countries) === null && !ctx.countries.tags.length) {
    return 'No countries available for this selection.';
  }
  return null;
}

export function seriesRaceWarnings(followRefused: string | null): readonly string[] {
  return followRefused ? [`Values are not followed here: ${followRefused}.`] : [];
}

function scaleValue(raw: Value<bigint | number>, decimals: number): { readonly value: Value<bigint | number>; readonly numeric: number | null } {
  if (raw.kind === 'notRecorded' || raw.kind === 'notApplicable' || raw.kind === 'pending') return { value: raw, numeric: null };
  if (raw.kind === 'zero') return { value: zeroValue(), numeric: 0 };
  const num = Number(raw.value) / (decimals > 0 ? 10 ** decimals : 1);
  if (decimals === 0) return { value: raw.value === 0 || raw.value === 0n ? zeroValue() : knownValue(raw.value), numeric: num };
  return { value: num === 0 ? zeroValue() : knownValue(num), numeric: num };
}

export function rankRowKey(row: SeriesRankRow): string {
  return countryKey(row.slot ?? row.country);
}

function makeRow(readView: BoundedReadView, reply: SeriesRankReply, rankRow: SeriesRankRow, decimals: number, config: SeriesRaceProjectConfig): BarsDataRow {
  const key = rankRowKey(rankRow);
  const named = countryRowPresentation(readView, key);
  const pres: CountryRowPresentation = rankRow.unnamed ? { label: notRecorded(rankRow.unnamed), media: named.media } : named;
  const state = rankRow.readings[0]!;
  const { value, numeric } = scaleValue(
    valueFromRawBigInt(state.run?.valueRaw ?? null, { kind: 'notRecorded', reason: state.missing ?? 'Not recorded at this hour' }),
    decimals,
  );
  const formatted = formatValueState(value, config.valueFormat);
  const item: TooltipListItem = formatted.kind === 'notRecorded'
    ? { label: config.valueLabel, missingReason: formatted.reason }
    : formatted.kind === 'notApplicable'
      ? { label: config.valueLabel, value: '—' }
      : formatted.kind === 'pending'
        ? { label: config.valueLabel, value: formatted.label }
        : { label: config.valueLabel, value: formatted.exact ?? formatted.text };
  const parts = numeric !== null && config.parts ? config.parts(reply, rankRow) : null;
  const identity = readView.who('country', key);
  const tag = rankRow.unnamed ? null : identity.tag?.status === 'ready' ? identity.tag.value : typeof rankRow.country === 'string' ? rankRow.country : null;
  return {
    kind: 'data',
    key,
    subject: key,
    label: pres.label,
    media: pres.media,
    ...(pres.color ? { color: pres.color } : {}),
    value: { value, format: config.valueFormat, label: config.valueLabel },
    numeric,
    ...(parts?.segments ? { segments: parts.segments } : {}),
    ...(parts?.segmentsIncomplete ? { segmentsIncomplete: true } : {}),
    tooltip: {
      title: barsLabelText(pres.label),
      hour: readView.hour,
      items: [...(rankRow.unnamed ? [{ label: 'Country', missingReason: rankRow.unnamed }] : []), item, ...(parts?.items ?? [])],
      ...(config.note ? { note: config.note } : {}),
    },
    meta: { tag: tag ?? key, key },
  };
}

export function projectSeriesRace(readView: BoundedReadView, options: SeriesRaceBaseOptions, config: SeriesRaceProjectConfig): BarsModel {
  const baseModel = {
    form: 'ordered' as const,
    hour: readView.hour,
    animateOrder: true,
    showAxis: true,
    scaleUnit: config.scaleUnit,
    scaleFormat: config.scaleFormat,
  };
  const refused = readView.refused('main');
  if (refused) return { ...baseModel, rows: [], empty: refused, emptyKind: 'unavailable' };
  const panel = readView.panel('main');
  if (panel === undefined) return { ...baseModel, rows: [] };
  const reply = validateRankReply(panel);
  if (!reply.rows.length && reply.refused) return { ...baseModel, rows: [], empty: reply.refused, emptyKind: 'unavailable' };
  const decimals = reply.measures[0]!.decimals;
  const rows: BarsDataRow[] = [];
  for (const rankRow of reply.rows) {
    rows.push(makeRow(readView, reply, rankRow, decimals, config));
  }
  if (rows.length) return { ...baseModel, rows };
  return { ...baseModel, rows, empty: options.group ? 'No country of this group at this date.' : config.emptyMessage, emptyKind: 'empty' };
}
