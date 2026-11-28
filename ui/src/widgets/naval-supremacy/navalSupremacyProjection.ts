import { extendLastRun, joinLanePieces, laneRun, missingCell, noRuns, LaneRunBuilder, solidCell, type LanePieces, type LaneRuns } from '../../families/heat/laneRuns.ts';
import { measureColour } from '../../charts/palette.ts';
import { countryFromKey } from '../../dashboard/countryKey.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import type { BarsModel, BarsProjection, BarsSegment } from '../../families/bars/model.ts';
import { buildShareRowsModel, type ShareRowEntry } from '../../families/bars/shareRows.ts';
import type { BoundedReadView, SourcePlan } from '../../families/contract.ts';
import { closedDayTiles, dayHistoryNames, dayTileCount, dayTileEntries } from '../../families/heat/dayTiles.ts';
import type { HeatLanesWidgetDeclaration } from '../../families/heat/lanesDeclaration.ts';
import { findRunAtHour, spanIndexAt, type HeatLaneCell, type HeatLaneReadout, type HeatLaneRow, type HeatLanesModel, type HeatLanesProjection } from '../../families/heat/lanesModel.ts';
import type { FamilyPlanContext } from '../../families/plan.ts';
import { contestedGrey, countryColourAt, regionLabel, type DatedColour } from '../../families/heat/scale.ts';
import { formatDateOnly, formatExactRational, formatShare, formatShareRatio } from '../../families/valueFormat.ts';
import { readRegionChoice, regionParams } from '../../families/regions/regionChoice.ts';
import { ADD_REGION_LABEL, mostActiveHeading, noRegionsBody, PICKED_REGIONS_HEADING } from '../../families/regions/regionRows.ts';
import { knownValue, notRecorded } from '../../families/value.ts';
import type { MissionShips, NavalSupremacyReply, SupremacyRegion, SupremacyRun } from '../../generated/dashboard';

export const SUPREMACY_TOP_DEFAULT = 7;

export interface SupremacyOptions {
  readonly viewer: string | null;
  readonly surface: string;
  readonly historyStart: number;
  readonly tiles: number;
}

interface Look {
  readonly friendly: (hour: number) => DatedColour;
  readonly enemy: string;
  readonly contested: string;
  readonly friendlyLabel: string;
  readonly enemyLabel: string;
  readonly viewer: string;
}

function lookOf(readView: BoundedReadView, reply: NavalSupremacyReply, options: SupremacyOptions): Look {
  const { surface, viewer } = options;
  const noColour = measureColour('bopRight', surface);
  const friendly = viewer
    ? (hour: number) => countryColourAt(readView, viewer, hour, surface, noColour)
    : () => ({ colour: noColour, through: Infinity });
  return {
    friendly,
    enemy: measureColour('short', surface),
    contested: contestedGrey(surface, [friendly(reply.lastHour).colour]),
    friendlyLabel: 'Friendly control',
    enemyLabel: 'Enemy control',
    viewer: reply.viewerName ?? 'Viewer',
  };
}

function shareRatio(text: string | null, scale: number): { readonly raw: bigint; readonly denom: bigint } | null {
  if (text === null || !Number.isSafeInteger(scale) || scale <= 0 || !/^-?\d+$/.test(text)) return null;
  const denom = BigInt(scale);
  const parsed = BigInt(text);
  const raw = parsed < 0n ? 0n : parsed > denom ? denom : parsed;
  return { raw, denom };
}

function shareOf(text: string | null, scale: number): number | null {
  if (text === null) return null;
  const value = Number(text) / scale;
  return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : null;
}

export function shipsText(own: MissionShips, enemy: MissionShips): string {
  if (own.kind === 'counted' && enemy.kind === 'counted') return `${own.ships} v ${enemy.ships} ships`;
  if (own.kind === 'counted') return `${own.ships} v n/a ships`;
  if (enemy.kind === 'counted') return `n/a v ${enemy.ships} ships`;
  return 'Ships not recorded';
}

function shipItems(region: SupremacyRegion, look: Look) {
  const item = (label: string, ships: MissionShips) => ships.kind === 'counted'
    ? { label, value: `${ships.ships}` }
    : { label, missingReason: ships.reason };
  return [item(`Mission-registered ships, ${look.viewer} and friends`, region.ownShips), item(`Mission-registered ships, enemies of ${look.viewer}`, region.enemyShips)];
}

function emptyBars(hour: number): BarsModel {
  return buildShareRowsModel({ hour });
}

export function projectSupremacyShares(readView: BoundedReadView, options: SupremacyOptions): BarsModel {
  const hour: number = readView.hour;
  const refused = readView.refused('now');
  if (refused) return buildShareRowsModel({ hour, empty: refused, emptyKind: 'unavailable' });
  const reply = readView.panel<NavalSupremacyReply>('now');
  if (!reply || !options.surface || reply.lastHour > hour) return emptyBars(hour);
  const look = lookOf(readView, reply, options);
  const entry = (region: SupremacyRegion): ShareRowEntry => {
    const named = regionLabel(readView, region.id, null);
    const base = { key: region.id, subject: region.id, region: Number(region.id), label: named.value, countText: shipsText(region.ownShips, region.enemyShips) };
    const readout = region.readout;
    if (!readout || readout.kind === 'notRecorded') {
      const reason = readout?.reason ?? 'Naval region control was not recorded';
      return { ...base, segments: [], missing: notRecorded(reason), tooltip: { title: named.label, hour, missingReason: reason, items: shipItems(region, look) } };
    }
    if (idle(readout.friendly, readout.enemy)) {
      return { ...base, segments: [], tooltip: { title: named.label, hour: readout.as_of_hour, items: [{ label: 'No activity' }, ...shipItems(region, look)] } };
    }
    const share = shareOf(readout.share, reply.scale);
    if (share === null) {
      return { ...base, segments: [], missing: notRecorded('The supremacy share was not recorded'), tooltip: { title: named.label, hour: readout.as_of_hour, missingReason: 'The supremacy share was not recorded' } };
    }
    const ratio = shareRatio(readout.share, reply.scale);
    const enemyShare = ratio ? Number(ratio.denom - ratio.raw) / Number(ratio.denom) : 1 - share;
    const friendlyPct = ratio ? formatShareRatio(ratio.raw, ratio.denom) : formatShare(share);
    const enemyPct = ratio ? formatShareRatio(ratio.denom - ratio.raw, ratio.denom) : formatShare(enemyShare);
    const friendly = look.friendly(readout.as_of_hour).colour;
    const segments: BarsSegment[] = [
      { key: 'friendly', label: `${look.viewer} and friends`, share, value: knownValue(share), color: friendly, inlineLabel: `Friendly ${friendlyPct}` },
      { key: 'enemy', label: `Enemies of ${look.viewer}`, share: enemyShare, value: knownValue(enemyShare), color: look.enemy, inlineLabel: `Enemy ${enemyPct}` },
    ];
    const control = readout.control > 0 ? look.friendlyLabel : readout.control < 0 ? look.enemyLabel : 'Contested';
    return {
      ...base,
      segments,
      tooltip: {
        title: named.label,
        hour: readout.as_of_hour,
        items: [
          { label: 'Control', value: control },
          { label: `${look.viewer} and friends`, value: friendlyPct, swatch: friendly },
          { label: `Enemies of ${look.viewer}`, value: enemyPct, swatch: look.enemy },
          ...shipItems(region, look),
        ],
      },
    };
  };
  const picked = reply.regions.filter(region => region.pinned).map(entry);
  const automatic = reply.regions.filter(region => !region.pinned).map(entry);
  return buildShareRowsModel({
    hour,
    picked,
    automatic,
    pickedHeading: PICKED_REGIONS_HEADING,
    automaticHeading: mostActiveHeading(look.viewer),
    addRegionLabel: ADD_REGION_LABEL,
    ...(reply.empty ? noRegionsBody(reply.empty, { one: 'active sea region', many: 'active sea regions', none: `No naval activity for ${look.viewer} at this date.` }) : {}),
    active: reply.activeUnlisted,
    warnings: reply.regions.length && reply.rankMissing ? [...reply.warnings, reply.rankMissing] : reply.warnings,
  });
}

function idle(friendly: string | null, enemy: string | null): boolean {
  return friendly !== null && enemy !== null && Number(friendly) <= 0 && Number(enemy) <= 0;
}

const noActivity: HeatLaneCell = { kind: 'clear', label: 'No activity' };

function runCell(run: SupremacyRun, look: Look, friendly: string, old?: HeatLaneCell): HeatLaneCell {
  if (run.control === null) return missingCell(run.missing ?? 'Naval region control was not recorded', old);
  if (idle(run.friendly, run.enemy)) return noActivity;
  return solidCell(run.control > 0 ? friendly : run.control < 0 ? look.enemy : look.contested, old);
}

function emptyLanes(hour: number, extra: Partial<HeatLanesModel>): HeatLanesModel {
  return { hour, firstHour: hour, lastHour: hour, dayHours: 24, playhead: null, rows: [], ...extra };
}

function unknownRunsOf(reply: NavalSupremacyReply, through: number): LaneRuns {
  return reply.unknown.map(span => {
    const firstHour = Math.max(reply.firstHour, reply.firstDayHour + span.firstDay * reply.dayHours);
    const lastHour = Math.min(through, reply.firstDayHour + (span.lastDay + 1) * reply.dayHours - 1);
    return { firstHour, lastHour, readHour: lastHour, cell: { kind: 'missing', reason: span.reason } };
  });
}

function regionRuns(reply: NavalSupremacyReply, region: SupremacyRegion, look: Look, through: number, unknown: LaneRuns, previous: LaneRuns): LaneRuns {
  const runs = new LaneRunBuilder(previous);
  const addUnknown = (run: LaneRuns[number]) => {
    const old = runs.next;
    if (old?.firstHour === run.firstHour && old.cell.kind === 'missing' && run.cell.kind === 'missing' && old.cell.reason === run.cell.reason) {
      runs.add(laneRun(run.firstHour, run.lastHour, run.readHour, old.cell, old));
    } else runs.add(run);
  };
  let nextUnknown = 0;
  for (const run of region.runs) {
    for (let day = run.firstDay; day <= run.lastDay;) {
      const firstHour = Math.max(reply.firstHour, reply.firstDayHour + day * reply.dayHours);
      if (firstHour > through) break;
      while (nextUnknown < unknown.length && unknown[nextUnknown]!.firstHour < firstHour) addUnknown(unknown[nextUnknown++]!);
      const friendly = look.friendly(firstHour);
      const until = Math.max(day, Math.min(run.lastDay, Math.floor((friendly.through - reply.firstDayHour) / reply.dayHours)));
      const lastHour = Math.min(through, reply.firstDayHour + (until + 1) * reply.dayHours - 1);
      const old = runs.next;
      runs.add(laneRun(firstHour, lastHour, lastHour, runCell(run, look, friendly.colour, old?.cell), old));
      day = until + 1;
    }
  }
  while (nextUnknown < unknown.length) addUnknown(unknown[nextUnknown++]!);
  return runs.finish();
}

const indexes = new WeakMap<NavalSupremacyReply, ReadonlyMap<string, SupremacyRegion>>();
interface TileLanes {
  readonly through: number;
  readonly sig: string;
  unknown?: LaneRuns;
  readonly rows: Map<string, LaneRuns>;
}

const heldTiles = new WeakMap<NavalSupremacyReply, TileLanes>();
const windows = new WeakMap<SupremacyOptions, Map<string, LanePieces>>();

function tileLanes(reply: NavalSupremacyReply, through: number, look: Look, surface: string) {
  let regions = indexes.get(reply);
  if (!regions) indexes.set(reply, regions = new Map(reply.regions.map(region => [region.id, region])));
  const sig = `${surface}|${look.contested}|${datedSignature(look, reply.firstHour, through)}`;
  let tile = heldTiles.get(reply);
  const old = tile;
  if (!tile || tile.through !== through || tile.sig !== sig) {
    heldTiles.set(reply, tile = { sig, through, rows: new Map() });
  }
  const extend = old && old.sig === sig && old.through < through
    && Math.floor((old.through - reply.firstDayHour) / reply.dayHours) === Math.floor((through - reply.firstDayHour) / reply.dayHours);
  return { reply, regions, tile, extend: extend ? old : undefined };
}

function datedSignature(look: Look, first: number, last: number): string {
  const out: string[] = [];
  for (let at = first; at <= last;) {
    const dated = look.friendly(at);
    out.push(`${at}:${dated.colour}`);
    if (dated.through >= last) break;
    at = dated.through + 1;
  }
  return out.join(',');
}

function inspectDay(reply: NavalSupremacyReply, region: SupremacyRegion | undefined, title: string, at: number, look: Look, painted: HeatLaneCell | undefined, readoutDay: boolean) {
  const dayStart = (day: number) => reply.firstDayHour + day * reply.dayHours;
  const day = Math.floor((at - reply.firstDayHour) / reply.dayHours);
  const readout = region?.readout;
  const dated = readoutDay && readout?.kind === 'control' ? { hour: readout.as_of_hour } : { subtitle: formatDateOnly(dayStart(day)) };
  const unknown = reply.unknown[spanIndexAt(reply.unknown, day, span => span.firstDay)];
  if (unknown && day <= unknown.lastDay) return { title, ...dated, missingReason: unknown.reason };
  const run = region?.runs[spanIndexAt(region.runs, day, item => item.firstDay)];
  if (!run || day > run.lastDay) return { title, ...dated, missingReason: 'Naval region control was not recorded' };
  if (run.control === null) return { title, ...dated, missingReason: run.missing ?? 'Naval region control was not recorded' };
  if (idle(run.friendly, run.enemy)) return { title, ...dated, items: [{ label: 'No activity' }] };
  const share = shareOf(run.share, reply.scale);
  const ratio = shareRatio(run.share, reply.scale);
  const sideRaw = ratio ? (run.control < 0 ? ratio.denom - ratio.raw : ratio.raw) : null;
  const sideShare = share === null ? null : (run.control < 0 ? 1 - share : share);
  const shareText = ratio && sideRaw !== null ? formatShareRatio(sideRaw, ratio.denom) : sideShare === null ? null : formatShare(sideShare);
  const label = run.control > 0 ? look.friendlyLabel : run.control < 0 ? look.enemyLabel : 'Contested';
  const swatch = run.control === 0 ? look.contested : run.control < 0 ? look.enemy : painted?.kind === 'solid' ? painted.color : look.contested;
  return { title, ...dated, items: [{ label, swatch, ...(shareText === null ? {} : { value: `${shareText} of the score` }) }] };
}

export function projectSupremacyTimeline(readView: BoundedReadView, options: SupremacyOptions): HeatLanesModel {
  const hour: number = readView.hour;
  const previous = windows.get(options);
  windows.delete(options);
  const refused = refusedOf(readView, options.tiles);
  if (refused) return emptyLanes(hour, { refused });
  const now = readView.panel<NavalSupremacyReply>('now');
  if (!now || !options.surface || now.lastHour > hour) return emptyLanes(hour, {});
  const tiles = closedDayTiles<NavalSupremacyReply>(readView, options.tiles, options.historyStart, now.firstHour);
  if (!tiles) return emptyLanes(hour, { waiting: true });
  const look = lookOf(readView, now, options);
  const through = Math.min(hour, now.validThrough);
  const pieces = [...tiles, now];
  const lanes = pieces.map(reply => tileLanes(reply, reply === now ? through : reply.lastHour, look, options.surface));
  const current = new Map<string, LanePieces>();
  windows.set(options, current);
  const lastDay = Math.floor((through - now.firstDayHour) / now.dayHours);
  const rows: HeatLaneRow[] = now.regions.map(region => {
    const named = regionLabel(readView, region.id, null);
    const old = previous?.get(region.id);
    const parts = lanes.map(({ reply, regions, tile, extend }, i) => {
      const source = regions.get(region.id);
      if (!source) return tile.unknown ??= unknownRunsOf(reply, tile.through);
      let runs = tile.rows.get(region.id);
      if (runs) return runs;
      const prior = extend?.rows.get(region.id);
      const last = prior?.at(-1);
      if (prior && last && extend && last.lastHour === extend.through) {
        runs = extendLastRun(prior, tile.through, tile.through);
      } else {
        const unknown = tile.unknown ??= unknownRunsOf(reply, tile.through);
        runs = regionRuns(reply, source, look, tile.through, unknown, old?.pieces[i] ?? noRuns);
      }
      tile.rows.set(region.id, runs);
      return runs;
    });
    const joined = joinLanePieces(parts, old);
    current.set(region.id, joined);
    const runs = joined.runs;
    const readout = region.readout;
    let shown: HeatLaneReadout | undefined;
    if (readout?.kind === 'notRecorded') shown = { kind: 'missing', reason: readout.reason };
    else if (readout && idle(readout.friendly, readout.enemy)) shown = { kind: 'clear', label: 'No activity' };
    else if (readout) {
      const share = shareOf(readout.share, now.scale);
      const ratio = shareRatio(readout.share, now.scale);
      const color = readout.control > 0 ? look.friendly(readout.as_of_hour).colour : readout.control < 0 ? look.enemy : look.contested;
      const primary = ratio
        ? formatShareRatio(readout.control > 0 ? ratio.raw : ratio.denom - ratio.raw, ratio.denom)
        : share === null
          ? ''
          : formatShare(readout.control > 0 ? share : 1 - share);
      shown = readout.control === 0
        ? { kind: 'value', form: 'value', primary: '', secondary: 'Contested', swatch: color, audit: 'Contested' }
        : { kind: 'value', form: 'value', primary,
          ...(ratio ? { exact: formatExactRational((readout.control > 0 ? ratio.raw : ratio.denom - ratio.raw) * 100n, ratio.denom, { percent: true }) } : {}),
          secondary: readout.control > 0 ? look.friendlyLabel : look.enemyLabel, swatch: color };
    }
    return { id: region.id, label: named.label, labelValue: named.value, ...(named.nameMissing ? { nameMissing: named.nameMissing } : {}), pinned: region.pinned, region: Number(region.id), runs, ...(shown ? { readout: shown } : {}) };
  });
  const gaps = new Map<string, { firstHour: number; lastHour: number; reason: string }>();
  for (const reply of pieces) for (const gap of reply.gaps) gaps.set(`${gap.firstHour}:${gap.lastHour}`, { firstHour: gap.firstHour, lastHour: gap.lastHour, reason: 'The recording holds no hour here' });
  return {
    hour: through,
    firstHour: tiles[0]?.firstHour ?? now.firstHour,
    lastHour: through,
    dayHours: now.dayHours,
    playhead: through,
    gaps: [...gaps.values()],
    rows,
    regions: { pickedHeading: PICKED_REGIONS_HEADING, automaticHeading: mostActiveHeading(look.viewer), addLabel: ADD_REGION_LABEL, active: now.activeUnlisted },
    readoutWidth: 204,
    inspect: (rowIndex, at) => {
      const row = rows[rowIndex];
      if (!row) return null;
      const reply = pieces[spanIndexAt(pieces, at, item => item.firstHour)];
      if (!reply || at > (reply === now ? through : reply.lastHour)) return null;
      const region = reply === now ? now.regions[rowIndex] : reply.regions.find(item => item.id === row.id);
      const readoutDay = reply === now && Math.floor((at - now.firstDayHour) / now.dayHours) === lastDay;
      return inspectDay(reply, region, row.label, at, look, findRunAtHour(row.runs, at, through)?.cell, readoutDay);
    },
    ...(now.empty ? noRegionsBody(now.empty, { one: 'active sea region', many: 'active sea regions', none: `No naval activity for ${look.viewer} at this date.` }) : {}),
    warnings: [...new Set(pieces.flatMap(reply => reply.warnings).concat(rows.length && now.rankMissing ? [now.rankMissing] : []))],
  };
}

function refusedOf(readView: BoundedReadView, tiles: number): string | undefined {
  return readView.refused('now') ?? dayHistoryNames(tiles).map(name => readView.refused(name)).find(Boolean);
}

function supremacySources(ctx: FamilyPlanContext, shares: boolean): SourcePlan {
  if (!ctx.countryTag) return { owner: `naval-supremacy:${ctx.branch.id}:none`, entries: [], unavailable: 'Choose a country in the widget’s header.' };
  const top = typeof ctx.options.rowCount === 'number' ? ctx.options.rowCount : SUPREMACY_TOP_DEFAULT;
  const regions: Record<string, unknown> = regionParams(readRegionChoice(ctx.options.regions));
  const base = { branchId: ctx.branch.id, country: countryFromKey(ctx.countryTag) };
  const params = { ...base, top, ...regions };
  const { pinnedRegions: _pinned, hiddenRegions: _hidden, ...rankMissing } = regions;
  const entries = dayTileEntries(ctx, {
    product: 'naval-supremacy',
    span: (params, first, last) => ({ ...params, firstHour: first, lastHour: last }),
    now: {
      params,
      coverage: { mode: 'population', form: 'top', limit: top },
    },
    tile: {
      panel: 'naval-supremacy',
      params: { ...base, ...rankMissing },
      coverage: { mode: 'population', form: 'all' },
    },
  });
  return {
    owner: `naval-supremacy:${shares ? 'shares' : 'timeline'}:${JSON.stringify(params)}`,
    entries: shares ? entries.filter(entry => entry.name === 'now') : entries,
  };
}

const COMMON = {
  kind: 'naval-supremacy',
  label: 'Naval supremacy',
  subject: 'naval supremacy',
  emptyCampaign: 'Load a campaign to see naval supremacy.',
  icon: 'anchor' as const,
  countries: { mode: 'single' } as const,
  unavailable: (ctx: FamilyPlanContext) => (ctx.countryTag ? null : 'Choose a country in the widget’s header.'),
  jumpToDate: true,
};

export const supremacySharesProjection: BarsProjection<SupremacyOptions> = {
  kind: 'naval-supremacy', family: 'bars', singleValueBars: () => 'share', project: projectSupremacyShares,
};
export const supremacyTimelineProjection: HeatLanesProjection<SupremacyOptions> = { kind: 'naval-supremacy', family: 'heat-lanes', project: projectSupremacyTimeline };

export const supremacySharesDeclaration: BarsWidgetDeclaration<SupremacyOptions> = {
  ...COMMON,
  family: 'bars',
  sources: ctx => supremacySources(ctx, true),
  projection: supremacySharesProjection,
  projectionOptions: (ctx, extra) => ({ viewer: ctx.countryTag, surface: extra?.surface ?? '', historyStart: ctx.branch.firstHour, tiles: 0 }),
};

export const supremacyTimelineDeclaration: HeatLanesWidgetDeclaration<SupremacyOptions> = {
  ...COMMON,
  family: 'heat-lanes',
  sources: ctx => supremacySources(ctx, false),
  projection: supremacyTimelineProjection,
  projectionOptions: (ctx, extra) => ({ viewer: ctx.countryTag, surface: extra.surface, historyStart: ctx.staticSpan?.[0] ?? ctx.branch.firstHour, tiles: dayTileCount(ctx.branch) }),
};
