import { liveKey } from '../../charts/live.ts';
import { reservedGrey } from '../../charts/palette.ts';
import { countryFromKey } from '../../dashboard/countryKey.ts';
import { selectedCountryKeys } from '../../dashboard/countrySelection.ts';
import type { BarsWidgetDeclaration } from '../../families/bars/declaration.ts';
import type { BarsModel, BarsProjection, BarsSegment } from '../../families/bars/model.ts';
import { registerFamilyProjection } from '../../families/registry.ts';
import { buildShareRowsModel, type ShareRowEntry } from '../../families/bars/shareRows.ts';
import type { BoundedReadView, FamilyPlanContext, SourcePlan } from '../../families/contract.ts';
import { regionLabel } from '../../families/heat/scale.ts';
import type { TooltipListItem } from '../../families/host/tooltips.ts';
import { knownValue, notRecorded, zeroValue } from '../../families/value.ts';
import { readRegionChoice, regionParams } from '../../families/regions/regionChoice.ts';
import { ADD_REGION_LABEL, mostActiveHeading, noRegionsBody, PICKED_REGIONS_HEADING, type RegionNouns } from '../../families/regions/regionRows.ts';
import { formatShare } from '../../families/valueFormat.ts';
import type { MinedRegion } from '../../generated/dashboard';
import { minesText, validateMinefields, type RegionRow } from './minefieldsData.ts';
import { identityName } from '../../families/table/identityCell.ts';

export type MinefieldsOrderOption = 'mines' | 'name';

const MINED_REGIONS: RegionNouns = { one: 'mined region', many: 'mined regions', none: 'No minefields at this date.' };

export interface MinefieldsProjectionOptions {
  readonly order: MinefieldsOrderOption;
  readonly surface: string;
}

function readBigInt(live: ReturnType<BoundedReadView['live']>, key: string): bigint | null {
  if (!live) return null;
  const raw = live.raw?.(key);
  if (raw !== undefined) return raw;
  const value = live.get(key);
  if (typeof value === 'bigint') return value;
  if (typeof value === 'number') return BigInt(Math.trunc(value));
  return null;
}

function minesCountText(total: bigint, scale: number): string {
  return `${minesText(total, scale)} ${total === BigInt(scale) ? 'mine' : 'mines'}`;
}

function projectMineRegion(
  region: MinedRegion,
  scale: number,
  readView: BoundedReadView,
  surface: string,
): ShareRowEntry {
  const id = String(region.region);
  const named = regionLabel(readView, region.region, null);
  const name = named.label;
  const live = readView.live('main');
  const meta: RegionRow = {
    region: id,
    name,
    owners: region.owners.map(owner => ({ tag: owner.tag })),
    unknownOwners: region.unknownOwners,
  };

  if (region.owners.length === 0) {
    if (region.unknownOwners > 0 || region.missing !== null) {
      const reason = region.missing ?? `${region.unknownOwners} ${region.unknownOwners === 1 ? 'country' : 'countries'} had unrecorded minefields here.`;
      return {
        key: id,
        subject: id,
        label: named.value,
        segments: [],
        countText: 'Not recorded',
        missing: notRecorded(reason),
        tooltip: {
          title: name,
          hour: readView.hour,
          missingReason: reason,
        },
        meta,
      };
    }
    return {
      key: id,
      subject: id,
      label: named.value,
      segments: [],
      countText: '0 mines',
      tooltip: {
        title: name,
        hour: readView.hour,
        items: [{ label: 'Mines', value: '0' }],
      },
      meta,
    };
  }

  const totalKey = liveKey(id, 'total');
  const total = readBigInt(live, totalKey);
  if (total === null) {
    const reason = live?.missing(totalKey) ?? 'Not recorded at this hour';
    const items: TooltipListItem[] = region.owners.map(owner => {
      const ownerName = identityName(readView, 'country', owner.tag);
      const ownerKey = liveKey(id, owner.tag);
      const amount = readBigInt(live, ownerKey);
      return amount === null
        ? { label: ownerName, missingReason: live?.missing(ownerKey) ?? 'Not recorded at this hour' }
        : { label: ownerName, value: minesCountText(amount, scale) };
    });
    if (region.unknownOwners > 0) {
      items.push({ label: 'Not recorded', value: `${region.unknownOwners} ${region.unknownOwners === 1 ? 'country' : 'countries'}` });
    }
    return {
      key: id,
      subject: id,
      label: named.value,
      segments: [],
      countText: 'Not recorded',
      missing: notRecorded(reason),
      tooltip: {
        title: name,
        hour: readView.hour,
        missingReason: reason,
        items,
      },
      meta,
    };
  }

  const segments: BarsSegment[] = [];
  let segmentsIncomplete = region.unknownOwners > 0 || region.missing !== null;
  const items: TooltipListItem[] = [{ label: 'Mines', value: minesCountText(total, scale) }];
  const unrecordedColour = reservedGrey(surface);
  for (const owner of region.owners) {
    const identity = readView.who('country', owner.tag);
    const ownerName = identityName(readView, 'country', owner.tag);
    const ownerColor = identity.colour?.status === 'ready' ? identity.colour.value : unrecordedColour;
    const ownerKey = liveKey(id, owner.tag);
    const amount = readBigInt(live, ownerKey);
    if (amount === null) {
      segmentsIncomplete = true;
      items.push({ label: ownerName, swatch: ownerColor, missingReason: live?.missing(ownerKey) ?? 'Not recorded at this hour' });
      continue;
    }
    const share = total > 0n && amount > 0n ? Number(amount * 10000n / total) / 10000 : 0;
    const pct = formatShare(share);
    if (amount > 0n) {
      segments.push({
        key: owner.tag,
        label: ownerName,
        share,
        value: knownValue(amount),
        color: ownerColor,
        inlineLabel: `${ownerName} ${pct}`,
      });
    } else {
      segments.push({
        key: owner.tag,
        label: ownerName,
        share: 0,
        value: zeroValue(),
        color: ownerColor,
      });
    }
    items.push({ label: ownerName, swatch: ownerColor, value: `${minesCountText(amount, scale)} (${pct})` });
  }

  if (region.unknownOwners > 0) {
    items.push({ label: 'Not recorded', value: `${region.unknownOwners} ${region.unknownOwners === 1 ? 'country' : 'countries'}` });
  }

  return {
    key: id,
    subject: id,
    label: named.value,
    segments,
    segmentsIncomplete,
    countText: minesCountText(total, scale),
    tooltip: {
      title: name,
      hour: readView.hour,
      items,
    },
    meta,
  };
}

export const minefieldsProjection: BarsProjection<MinefieldsProjectionOptions> = registerFamilyProjection({
  kind: 'minefields',
  family: 'bars',
  singleValueBars: () => 'share',
  project(readView: BoundedReadView, options: MinefieldsProjectionOptions): BarsModel {
    const hour = readView.hour;
    const refused = readView.refused('main');
    if (refused) {
      return buildShareRowsModel({ hour, rows: [], empty: refused, emptyKind: 'unavailable' });
    }
    const panel = readView.panel('main');
    if (panel === undefined) {
      return buildShareRowsModel({ hour, rows: [] });
    }
    const reply = validateMinefields(panel);
    const picked: ShareRowEntry[] = [];
    const automatic: ShareRowEntry[] = [];
    for (const region of reply.regions) {
      (region.pinned ? picked : automatic).push({ ...projectMineRegion(region, reply.scale, readView, options.surface), region: region.region });
    }
    const empty = reply.empty ? noRegionsBody(reply.empty, MINED_REGIONS) : null;
    return buildShareRowsModel({
      hour,
      picked,
      automatic,
      pickedHeading: PICKED_REGIONS_HEADING,
      automaticHeading: options.order === 'name' ? 'Mined regions' : mostActiveHeading(null),
      addRegionLabel: ADD_REGION_LABEL,
      ...(empty ?? {}),
      active: reply.activeUnlisted,
      warnings: reply.warnings,
    });
  },
});

function minefieldsSources(ctx: FamilyPlanContext): SourcePlan {
  const tags = selectedCountryKeys(ctx.widget.countries, ctx.track);
  const chosen = regionParams(readRegionChoice(ctx.options.regions));
  const order: MinefieldsOrderOption = ctx.options.order === 'name' ? 'name' : 'mines';
  const params = {
    branchId: ctx.branch.id,
    ...(tags.length ? { countries: tags.map(countryFromKey) } : {}),
    ...chosen,
    order,
  };
  const owner = `minefields:${ctx.track.source.id}:${ctx.track.source.revision}:${ctx.branch.id}:${order}:${JSON.stringify(tags)}:${JSON.stringify(chosen)}`;
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
        product: 'minefields',
        followQuery: { panel: 'minefields', ...params },
        params,
        validate: validateMinefields,
      },
    }],
  };
}

export const minefieldsDeclaration: BarsWidgetDeclaration<MinefieldsProjectionOptions> = {
  family: 'bars',
  kind: 'minefields',
  label: 'Minefields by sea region',
  subject: 'minefields',
  emptyCampaign: 'Load a campaign to see minefields.',
  icon: 'anchor',
  countries: { mode: 'none' },
  sources: minefieldsSources,
  projection: minefieldsProjection,
  projectionOptions: (ctx, extra) => ({
    order: ctx.options.order === 'name' ? 'name' : 'mines',
    surface: extra.surface,
  }),
  jumpToDate: true,
  keyByBranch: true,
};
