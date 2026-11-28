import { rankReading } from '../../charts/race/measureRaceData.ts';
import { colour } from '../../charts/palette.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import type { BarsModel, BarsPartReading, BarsProjection, BarsSegment } from '../../families/bars/model.ts';
import {
  projectSeriesRace,
  seriesRaceSelection,
  seriesRaceSources,
  seriesRaceUnavailable,
  seriesRaceWarnings,
  type SeriesRaceBaseOptions,
  type SeriesRaceParts,
} from '../../families/bars/seriesRace.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notRecorded } from '../../families/value.ts';
import { formatExactScalar } from '../../families/valueFormat.ts';
import type { SeriesRankReply, SeriesRankRow } from '../../generated/dashboard';
import { SCORE_LABELS, scorePart, type ScorePart } from './gameScoreData.ts';

const SERIES = 'country-game-scores';
const SPLIT_PARTS = ['army', 'navy', 'air'] as const;

export interface GameScoreRaceOptions extends SeriesRaceBaseOptions {
  readonly by: ScorePart;
  readonly split: boolean;
}

const splits = (options: Pick<GameScoreRaceOptions, 'by' | 'split'>) => options.split && options.by === 'total';

function splitParts(surface: string, reply: SeriesRankReply, row: SeriesRankRow): SeriesRaceParts {
  const items: TooltipListItem[] = [];
  const partReadings: BarsPartReading[] = [];
  const known: { readonly part: typeof SPLIT_PARTS[number]; readonly color: string; readonly amount: number }[] = [];
  SPLIT_PARTS.forEach((part, at) => {
    const color = colour(surface, at);
    const reading = rankReading(reply, row, part);
    const decimals = reply.measures.find(measure => measure.name === part)?.decimals ?? 0;
    const label = SCORE_LABELS[part];
    if (!reading?.run) {
      const missingReason = reading?.missing ?? 'Not recorded at this hour';
      partReadings.push({ key: part, label, value: notRecorded(missingReason) });
      items.push({ label, swatch: color, missingReason });
      return;
    }
    const raw = BigInt(reading.run.valueRaw);
    const format = { decimals };
    partReadings.push({ key: part, label, value: knownValue(raw), format });
    items.push({ label, swatch: color, value: formatExactScalar(raw, format) });
    known.push({ part, color, amount: Math.max(0, Number(raw) / 10 ** decimals) });
  });
  const sum = known.reduce((total, part) => total + part.amount, 0);
  if (known.length < SPLIT_PARTS.length) return { items, partReadings, segmentsIncomplete: true };
  if (sum <= 0) return { items, partReadings };
  const segments: BarsSegment[] = known.filter(part => part.amount > 0).map(part => ({
    key: part.part,
    label: SCORE_LABELS[part.part],
    share: part.amount / sum,
    value: knownValue(part.amount),
    format: { decimals: 0 },
    color: part.color,
  }));
  return { segments, items, partReadings };
}

export const gameScoreRaceProjection: BarsProjection<GameScoreRaceOptions> = registerFamilyProjection({
  kind: 'game-score',
  family: 'bars',
  singleValueBars: options => splits(options) ? 'ordered-segments' : true,
  nextChange: () => Infinity,
  project(readView: BoundedReadView, options: GameScoreRaceOptions): BarsModel {
    return projectSeriesRace(readView, options, {
      valueFormat: { decimals: 0 },
      scaleFormat: { decimals: 0 },
      scaleUnit: SCORE_LABELS[options.by],
      valueLabel: SCORE_LABELS[options.by],
      emptyMessage: 'No country’s scores recorded at this time.',
      ...(splits(options) ? { parts: (reply: SeriesRankReply, row: SeriesRankRow) => splitParts(options.surface, reply, row) } : {}),
    });
  },
}, 'race');

function gameScoreRaceSources(ctx: FamilyPlanContext): SourcePlan {
  const by = scorePart(ctx.options.topMeasure);
  const split = ctx.options.split !== false;
  return seriesRaceSources(ctx, {
    kind: 'game-score',
    family: { kind: SERIES, measure: by },
    ...(splits({ by, split }) ? { measures: SPLIT_PARTS } : {}),
  });
}

export const gameScoreRaceDeclaration: BarsWidgetDeclaration<GameScoreRaceOptions> = {
  family: 'bars',
  kind: 'game-score',
  label: 'Game score',
  subject: 'game scores',
  emptyCampaign: 'Load a campaign to see its game scores.',
  icon: 'grid',
  countries: {
    mode: 'multi',
    content: 'hour',
    allowSlots: true,
    requireGroupsWhen: widget => widget.countries.kind === 'group' && widget.countries.group.type !== 'top',
  },
  sources: gameScoreRaceSources,
  projection: gameScoreRaceProjection,
  projectionOptions: (ctx, extra) => ({
    ...seriesRaceSelection(ctx, extra),
    by: scorePart(ctx.options.topMeasure),
    split: ctx.options.split !== false,
  }),
  unavailable: seriesRaceUnavailable,
  warnings: (_ctx, followRefused) => seriesRaceWarnings(followRefused),
  jumpToDate: true,
  keyByBranch: true,
};
