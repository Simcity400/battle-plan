import { liveKey } from '../../charts/live.ts';
import { authoredHex, measureColour, neutralColour } from '../../charts/palette.ts';
import { airMode, airParams, airPercent, airScore, validateAirSuperiority } from '../../dashboard/airSuperiority.ts';
import { ADD_REGION_LABEL, mostActiveHeading, noRegionsBody, PICKED_REGIONS_HEADING } from '../../families/regions/regionRows.ts';
import { defaultCountryKey, selectedCountryKeys } from '../../dashboard/countrySelection.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import type { BarsModel, BarsProjection, BarsSegment } from '../../families/bars/model.ts';
import { regionLabel } from '../../families/heat/scale.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { buildShareRowsModel, type ShareRowEntry } from '../../families/bars/shareRows.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notApplicable, notRecorded, zeroValue } from '../../families/value.ts';
import { formatScalar } from '../../families/valueFormat.ts';
import type { AirRegion, AirSide, AirSuperiorityReply } from '../../generated/dashboard';
import { identityNameOr } from '../../families/table/identityCell.ts';

export type AirRegionOrderOption = 'contested' | 'active' | 'name';

export interface AirSuperiorityProjectionOptions {
  readonly mode: 'sides' | 'viewpoint';
  readonly filteredSides: boolean;
  readonly order: AirRegionOrderOption;
  readonly enemyColor: string;
  readonly mutedColor: string;
}

const AIR_REASONS: Readonly<Record<string, string>> = {
  'side-not-present': 'Side not present at this hour.',
  'score-not-recorded': 'Scores were not recorded at this hour.',
  'scores-unavailable': 'Scores were not recorded at this hour.',
  'sides-not-recorded': 'Side membership was not recorded.',
  'identity-not-recorded': 'Countries unavailable.',
  'theatres-not-recorded': 'Air groups unavailable.',
  'outside-theatres': 'Outside selected air theatres',
};

const NOT_APPLICABLE = new Set(['no-air-object', 'outside-theatres']);

function resolveSideName(side: AirSide, readView: BoundedReadView): string {
  if (side.factions.length) {
    const names: string[] = [];
    let resolvedAll = true;
    for (const id of side.factions) {
      const identity = readView.who('faction', id);
      if (identity.name.status === 'ready') {
        names.push(identity.name.value);
      } else {
        resolvedAll = false;
        break;
      }
    }
    if (resolvedAll && names.length) return names.join(' / ');
    if (side.name) return side.name;
    return side.factions.map(id => `Faction ${id}`).join(' / ');
  }
  const tag = side.tag ?? side.leader;
  if (tag) {
    const identity = readView.who('country', tag);
    const name = identityNameOr(readView, 'country', tag, side.name ?? tag);
    if (identity.name.status === 'ready' && side.key.startsWith('w:') && side.coBelligerents > 0) {
      return `${name} and allies`;
    }
    return name;
  }
  return side.name ?? 'Coalition (name not recorded)';
}

function resolveSideColor(
  side: AirSide,
  readView: BoundedReadView,
  options: AirSuperiorityProjectionOptions,
): string {
  if (side.colourFaction) {
    const factionIdentity = readView.who('faction', side.colourFaction);
    if (factionIdentity.colour?.status === 'ready') return factionIdentity.colour.value;
  }
  if (side.color) return authoredHex(side.color);
  const tag = side.anchor?.tag ?? side.tag ?? side.leader;
  if (tag) {
    const countryIdentity = readView.who('country', tag);
    if (countryIdentity.colour?.status === 'ready') return countryIdentity.colour.value;
  }
  return options.mutedColor;
}

function wingsText(region: AirRegion, live: ReturnType<BoundedReadView['live']>): { readonly text: string; readonly count: number | null } {
  if (region.wings === 'none') return { text: '0 air wings', count: 0 };
  if (region.wings === 'unknown') return { text: 'Wings not recorded', count: null };
  const raw = live?.get(liveKey(String(region.id), 'wings'));
  if (raw === undefined) return { text: 'Wings not recorded', count: null };
  const count = Math.trunc(Number(raw));
  return { text: `${formatScalar(count, 'integer')} air ${count === 1 ? 'wing' : 'wings'}`, count };
}

function superiorityState(friendly: number, total: number, scale: number, badBelow: number, goodAbove: number): string {
  if (friendly >= 9223372036854775807) return 'Air superiority';
  const denom = BigInt.asIntN(64, BigInt(Math.trunc(total)));
  if (!denom) return 'Air superiority';
  const ratio = BigInt.asIntN(64, BigInt(Math.trunc(friendly)) * BigInt(scale)) / denom;
  return ratio > BigInt(goodAbove) ? 'Air superiority' : ratio < BigInt(badBelow) ? 'Enemy superiority' : 'Contested';
}

function projectRegion(
  region: AirRegion,
  reply: AirSuperiorityReply,
  readView: BoundedReadView,
  options: AirSuperiorityProjectionOptions,
): ShareRowEntry {
  const live = readView.live('main');
  const named = regionLabel(readView, region.id, region.name);
  const name = named.label;
  const wings = wingsText(region, live);
  const cell = region.cell;
  const baseTooltip: TooltipListItem[] = [];

  if (cell.kind === 'reason') {
    const reasonText = cell.reason === 'no-air-object' ? 'No air data' : AIR_REASONS[cell.reason] ?? cell.reason;
    const missing = NOT_APPLICABLE.has(cell.reason) ? notApplicable(reasonText) : notRecorded(reasonText);
    return {
      key: String(region.id),
      subject: String(region.id),
      label: named.value,
      segments: [],
      countText: wings.text,
      missing,
      tooltip: {
        title: name,
        hour: readView.hour,
        missingReason: reasonText,
        items: [{ label: 'Air wings', value: wings.text }],
      },
    };
  }

  if (cell.kind === 'view') {
    const side = cell.side === null ? undefined : reply.sides[cell.side];
    const vpName = side
      ? resolveSideName(side, readView)
      : cell.leader
        ? (() => {
            return identityNameOr(readView, 'country', cell.leader, reply.viewpointCountryStyle?.name ?? cell.leader);
          })()
        : (reply.viewpointCountryStyle?.name ?? 'Viewpoint');
    const vpColor = side
      ? resolveSideColor(side, readView, options)
      : reply.viewpointCountryStyle?.color
        ? authoredHex(reply.viewpointCountryStyle.color)
        : cell.leader
          ? (() => {
              const id = readView.who('country', cell.leader);
              return id.colour?.status === 'ready' ? id.colour.value : options.mutedColor;
            })()
          : options.mutedColor;

    if (cell.live === null) {
      return {
        key: String(region.id),
        subject: String(region.id),
        label: named.value,
        segments: [],
        countText: wings.text,
        tooltip: {
          title: name,
          hour: readView.hour,
          items: [
            { label: 'State', value: 'No air activity' },
            { label: 'Air wings', value: wings.text },
          ],
        },
      };
    }

    const friendlyRaw = live?.get(liveKey(cell.live, 'friendly'));
    const enemyRaw = live?.get(liveKey(cell.live, 'enemy'));
    const totalRaw = live?.get(liveKey(String(region.id), 'total'));
    if (friendlyRaw === undefined || enemyRaw === undefined || totalRaw === undefined) {
      const reason = live?.missing(liveKey(cell.live, 'friendly')) ?? 'Scores were not recorded at this hour.';
      return {
        key: String(region.id),
        subject: String(region.id),
        label: named.value,
        segments: [],
        countText: wings.text,
        missing: notRecorded(reason),
        tooltip: {
          title: name,
          hour: readView.hour,
          missingReason: reason,
          items: [{ label: 'Air wings', value: wings.text }],
        },
      };
    }

    const friendly = Number(friendlyRaw);
    const enemy = Number(enemyRaw);
    const total = Number(totalRaw);
    if (total <= 0) {
      return {
        key: String(region.id),
        subject: String(region.id),
        label: named.value,
        segments: [],
        countText: wings.text,
        tooltip: {
          title: name,
          hour: readView.hour,
          items: [
            { label: 'State', value: 'No air activity' },
            { label: 'Air wings', value: wings.text },
          ],
        },
      };
    }

    const friendlyShare = friendly / total;
    const enemyShare = enemy / total;
    const segments: BarsSegment[] = [];
    if (friendly > 0) {
      segments.push({
        key: 'friendly',
        label: vpName,
        share: friendlyShare,
        value: knownValue(friendly),
        color: vpColor,
        inlineLabel: `${vpName} ${airPercent(friendly, total)}`,
      });
    }
    if (enemy > 0) {
      segments.push({
        key: 'enemy',
        label: 'Enemies',
        share: enemyShare,
        value: knownValue(enemy),
        color: options.enemyColor,
        inlineLabel: `Enemies ${airPercent(enemy, total)}`,
      });
    }
    return {
      key: String(region.id),
      subject: String(region.id),
      label: named.value,
      segments,
      countText: wings.text,
      tooltip: {
        title: name,
        hour: readView.hour,
        items: [
          { label: `${vpName} (friendly)`, swatch: vpColor, value: `${airPercent(friendly, total)} \u00b7 ${airScore(friendly, reply.scale)}` },
          { label: `Enemies of ${vpName}`, swatch: options.enemyColor, value: `${airPercent(enemy, total)} \u00b7 ${airScore(enemy, reply.scale)}` },
          { label: 'State', value: superiorityState(friendly, total, reply.scale, reply.badBelow, reply.goodAbove) },
          { label: 'Air wings', value: wings.text },
        ],
      },
    };
  }

  if (cell.unknown.length > 0) {
    const reason = 'One or more sides had unrecorded air scores at this hour.';
    return {
      key: String(region.id),
      subject: String(region.id),
      label: named.value,
      segments: [],
      countText: wings.text,
      missing: notRecorded(reason),
      tooltip: {
        title: name,
        hour: readView.hour,
        missingReason: reason,
        items: [
          ...cell.unknown.map(idx => ({ label: resolveSideName(reply.sides[idx]!, readView), missingReason: 'Score unknown' })),
          { label: 'Air wings', value: wings.text },
        ],
      },
    };
  }

  if (cell.parts.length === 0) {
    const isNoAir = options.mode === 'viewpoint' && cell.noAirObject.length > 0;
    return {
      key: String(region.id),
      subject: String(region.id),
      label: named.value,
      segments: [],
      countText: wings.text,
      ...(isNoAir ? { missing: notApplicable('No air data') } : {}),
      tooltip: {
        title: name,
        hour: readView.hour,
        items: [
          { label: 'State', value: isNoAir ? 'No air data' : cell.filtered ? 'No selected side has a score' : 'No air activity' },
          { label: 'Air wings', value: wings.text },
        ],
      },
    };
  }

  const totalRaw = live?.get(liveKey(String(region.id), 'total'));
  if (totalRaw === undefined) {
    const reason = live?.missing(liveKey(String(region.id), 'total')) ?? 'Scores were not recorded at this hour.';
    return {
      key: String(region.id),
      subject: String(region.id),
      label: named.value,
      segments: [],
      countText: wings.text,
      missing: notRecorded(reason),
      tooltip: {
        title: name,
        hour: readView.hour,
        missingReason: reason,
        items: [{ label: 'Air wings', value: wings.text }],
      },
    };
  }
  const total = Number(totalRaw);
  const segments: BarsSegment[] = [];
  for (const part of cell.parts) {
    const scoreRaw = live?.get(liveKey(part.live, 'friendly'));
    if (scoreRaw === undefined) {
      const reason = live?.missing(liveKey(part.live, 'friendly')) ?? 'Scores were not recorded at this hour.';
      return {
        key: String(region.id),
        subject: String(region.id),
        label: named.value,
        segments: [],
        countText: wings.text,
        missing: notRecorded(reason),
        tooltip: {
          title: name,
          hour: readView.hour,
          missingReason: reason,
          items: [{ label: 'Air wings', value: wings.text }],
        },
      };
    }
    const score = Number(scoreRaw);
    if (score <= 0) continue;
    const side = reply.sides[part.side]!;
    const sideName = resolveSideName(side, readView);
    const sideColor = resolveSideColor(side, readView, options);
    const share = total > 0 ? score / total : 0;
    segments.push({
      key: side.key,
      label: sideName,
      share,
      value: score === 0 ? zeroValue() : knownValue(score),
      color: sideColor,
      inlineLabel: `${sideName} ${airPercent(score, total)}`,
    });
    baseTooltip.push({
      label: sideName,
      swatch: sideColor,
      value: `${airPercent(score, total)} \u00b7 ${airScore(score, reply.scale)}`,
    });
  }

  if (cell.excluded > 0) {
    baseTooltip.push({ label: 'Excluded sides', value: String(cell.excluded) });
  }
  baseTooltip.push({ label: 'Air wings', value: wings.text });

  return {
    key: String(region.id),
    subject: String(region.id),
    label: named.value,
    segments,
    countText: wings.text,
    tooltip: {
      title: name,
      hour: readView.hour,
      items: baseTooltip,
    },
  };
}

export const airSuperiorityRegionProjection: BarsProjection<AirSuperiorityProjectionOptions> = registerFamilyProjection({
  kind: 'air-superiority-region',
  family: 'bars',
  singleValueBars: () => 'share',
  project(readView: BoundedReadView, options: AirSuperiorityProjectionOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    const reply = validateAirSuperiority(panel);
    if (reply.missing !== null) {
      return buildShareRowsModel({ hour, rows: [], empty: reply.missing, emptyKind: 'unavailable', warnings: reply.warnings });
    }

    const picked: ShareRowEntry[] = [];
    const automatic: ShareRowEntry[] = [];
    for (const region of reply.regions) {
      (region.pinned ? picked : automatic).push({ ...projectRegion(region, reply, readView, options), region: region.id });
    }

    const subject = options.mode === 'viewpoint' ? reply.viewpointCountryStyle?.name ?? null : null;
    const empty = reply.empty
      ? noRegionsBody(reply.empty, { one: 'active region', many: 'active regions', none: `No air activity${subject ? ` for ${subject}` : ''} at this date.` })
      : null;
    return buildShareRowsModel({
      hour,
      picked,
      automatic,
      pickedHeading: PICKED_REGIONS_HEADING,
      automaticHeading: airAutomaticHeading(subject, options.order),
      addRegionLabel: ADD_REGION_LABEL,
      ...(empty ?? {}),
      active: reply.activeUnlisted,
      warnings: reply.warnings,
    });
  },
});

function airAutomaticHeading(subject: string | null, order: AirRegionOrderOption): string {
  if (order === 'active') return mostActiveHeading(subject);
  const lead = order === 'contested' ? 'Most contested' : 'Active';
  return subject ? `${lead} for ${subject}` : order === 'contested' ? lead : 'Active regions';
}

export interface AirSuperiorityAux {
  readonly enemyColor: string;
  readonly mutedColor: string;
}

function airSuperioritySources(ctx: FamilyPlanContext<AirSuperiorityAux>): SourcePlan {
  const campaign = ctx.track.source.metadata.campaignKey ?? '';
  const currentCountry = selectedCountryKeys(ctx.widget.countries, ctx.track)[0] ?? defaultCountryKey(ctx.track);
  const mode = airMode(ctx.options, campaign, currentCountry);
  const order: AirRegionOrderOption = ctx.options.order === 'name' ? 'name' : ctx.options.order === 'active' ? 'active' : 'contested';
  const owner = `air-superiority-region:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${order}:${JSON.stringify(mode)}`;
  if (mode.mode === 'viewpoint' && !mode.viewpoint) {
    return {
      owner,
      entries: [],
      unavailable: 'Choose a dashboard country or a viewpoint in the widget settings.',
    };
  }
  const params = { branchId: ctx.branch.id, ...airParams(mode), order };
  return {
    owner,
    entries: [{
      name: 'main',
      source: ctx.track.source,
      branch: ctx.branch.id,
      offset: ctx.track.axisOffset,
      read: {
        shape: 'point',
        channel: 'panel',
        coverage: { mode: 'population', form: 'all' },
        product: 'air-superiority',
        followQuery: { panel: 'air-superiority', ...params },
        params,
        validate: validateAirSuperiority,
      },
    }],
  };
}

export const airSuperiorityRegionDeclaration: BarsWidgetDeclaration<AirSuperiorityProjectionOptions, AirSuperiorityAux> = {
  family: 'bars',
  kind: 'air-superiority-region',
  label: 'Air superiority by region',
  subject: 'air superiority',
  emptyCampaign: 'Load a campaign to see its air superiority.',
  icon: 'plane',
  countries: { mode: 'none' },
  sources: airSuperioritySources,
  projection: airSuperiorityRegionProjection,
  projectionOptions: (ctx, extra) => {
    const campaign = ctx.track.source.metadata.campaignKey ?? '';
    const currentCountry = selectedCountryKeys(ctx.widget.countries, ctx.track)[0] ?? defaultCountryKey(ctx.track);
    const mode = airMode(ctx.options, campaign, currentCountry);
    const order: AirRegionOrderOption = ctx.options.order === 'name' ? 'name' : ctx.options.order === 'active' ? 'active' : 'contested';
    return {
      mode: mode.mode,
      filteredSides: mode.sides.length > 0,
      order,
      enemyColor: ctx.aux?.enemyColor ?? measureColour('short', extra.surface),
      mutedColor: ctx.aux?.mutedColor ?? neutralColour('fallback', extra.surface),
    };
  },
  jumpToDate: true,
  keyByBranch: true,
};
