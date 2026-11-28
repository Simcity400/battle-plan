import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { createServer } from 'vite';
import { countryKey } from '../../dashboard/countryKey.ts';
import { barsHostAdapter } from '../../families/bars/hostAdapter.ts';
import { assertBarsModelValid, barsLabelText, deriveBarsAudit, deriveBarsMirror, deriveBarsTooltip, type BarsDataRow } from '../../families/bars/model.ts';
import { buildTooltipContent } from '../../families/host/tooltips.ts';
import { tooltipText } from '../../charts/tooltip.ts';
import { createBoundedReadView, displayedHour } from '../../families/boundedView.ts';
import { lazyActionKeys } from '../../families/host/adapter.ts';
import { recordInstalled, resolveHostInput } from '../../families/host/installed.ts';
import type { FamilyNavTuple } from '../../families/host/keyboard.ts';
import type { BranchToHourSourceRead, ExactHoverSourceRead, FamilyPlanContext, IdentityProvider, InstalledAction, InstalledPicture } from '../../families/plan.ts';
import { deriveTimeChartTooltip } from '../../families/time-chart/hitTest.ts';
import { parseTimeChartHit, timeChartHostAdapter } from '../../families/time-chart/hostAdapter.ts';
import { hoverTimeChartLayout, layoutTimeChart } from '../../families/time-chart/layout.ts';
import { timeChartLane } from '../../families/time-chart/laneView.ts';
import { LANE_THEME } from '../../families/laneRuntime.fixture.ts';
import { createLaneView } from '../../families/laneRuntime.ts';
import { Lane } from '../../draw/lane.ts';
import { buildStripKeyframe, readPlanExact } from '../../families/broker.ts';
import { stripContextAt } from '../../families/host/planSources.ts';
import type { DashboardClient } from '../../dashboard/client.ts';
import { gatedHour, openGate } from '../../test-support/gated.ts';
import type { CountryIdentitiesReply, OccupationReserveReply, ReserveHistoryReply } from '../../generated/dashboard';
import { FAMILY_VIEWS } from '../familyManifest.ts';
import { occupationReserveBreakdownDeclaration, occupationReserveBreakdownProjection } from './occupationReserveBreakdownProjection.ts';
import { occupationReserveRaceDeclaration, occupationReserveRaceProjection } from './occupationReserveRaceProjection.ts';

function fakeIdentity(names: Record<string, { name: string; color: string }>): IdentityProvider {
  return {
    resolve(kind, key, atHour) {
      const hit = kind === 'country' ? names[key] : undefined;
      return {
        kind,
        key,
        atHour,
        holder: { status: 'ready', value: key },
        tag: { status: 'ready', value: key },
        name: hit ? { status: 'ready', value: hit.name } : { status: 'absent', reason: 'Name not recorded' },
        colour: hit ? { status: 'ready', value: hit.color } : { status: 'absent', reason: 'Colour not recorded' },
        flagChoice: { status: 'absent', reason: 'Flag not recorded' },
        validInterval: [0, 1000],
        nextChangeAt: Infinity,
      };
    },
  };
}

test('occupation-reserve-manpower race preserves Rust ranking, builds source-country segments, and separates negative entries in tooltip', () => {
  const reply: OccupationReserveReply = {
    holders: [
      {
        country: { id: 2, tag: 'AAA' },
        slot: null,
        admitted: true,
        total: 500,
        entries: [
          { ordinal: 0, source: { id: 8, tag: 'BBB' }, amount: -50 },
          { ordinal: 1, source: { id: 9, tag: 'CCC' }, amount: 300 },
          { ordinal: 2, source: { id: 9, tag: 'CCC' }, amount: 200 },
        ],
      },
      {
        country: { id: 3, tag: 'DDD' },
        slot: null,
        admitted: true,
        total: 250,
        entries: [
          { ordinal: 0, source: { id: 10, tag: null }, amount: 250 },
        ],
      },
    ],
    readHour: 240,
    unnamed: 1,
    noneMissing: null,
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({
    hour: displayedHour(240),
    panels: new Map([['main', reply]]),
    identity: fakeIdentity({
      AAA: { name: 'Alpha', color: '#6b7280' },
      DDD: { name: 'Delta', color: '#e11d48' },
      BBB: { name: 'Beta', color: '#22c55e' },
      CCC: { name: 'Gamma', color: '#f59e0b' },
    }),
  });
  const model = occupationReserveRaceProjection.project(view, { surface: '#1e1e1e', top: 10, all: false, group: false });
  assertBarsModelValid(model, 240);
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(rows.map(r => [r.key, r.numeric]), [['AAA', 500], ['DDD', 250]]);
  assert.deepEqual(rows[0]!.segments?.map(s => [s.key, s.share]), [['CCC', 1]]);
  assert.ok(rows[0]!.tooltip?.items?.some(item => item.label === 'Negative entries'));
  assert.equal(deriveBarsMirror(model).rows[0]?.cells.find(cell => cell.column === 'segments')?.spoken, 'Beta \u221250, Gamma 300, Gamma 200');
  assert.equal(deriveBarsAudit(model).find(cell => cell.key === 'AAA:part:0')?.value, '["known",["int","-50"]]');
  assert.ok(model.warnings?.some(w => w.includes('1 holders')));

  const pendingSourceIdentity: IdentityProvider = {
    resolve(kind, key, atHour) {
      if (kind === 'country' && key === 'AAA') {
        return { kind, key, atHour, holder: { status: 'ready', value: key }, tag: { status: 'ready', value: key }, name: { status: 'ready', value: 'Alpha' }, colour: { status: 'ready', value: '#6b7280' }, flagChoice: { status: 'absent', reason: 'Flag not recorded' }, validInterval: [0, 1000], nextChangeAt: Infinity };
      }
      return { kind, key, atHour, holder: { status: 'ready', value: key }, tag: { status: 'ready', value: key }, name: { status: 'pending' }, colour: { status: 'absent', reason: 'Colour not recorded' }, flagChoice: { status: 'absent', reason: 'Flag not recorded' }, validInterval: [0, 1000], nextChangeAt: Infinity };
    },
  };
  const pendingSourceModel = occupationReserveRaceProjection.project(
    createBoundedReadView({ hour: displayedHour(240), panels: new Map([['main', reply]]), identity: pendingSourceIdentity }),
    { surface: '#1e1e1e', top: 10, all: false, group: false },
  );
  const pendingSourceTip = deriveBarsTooltip(pendingSourceModel, 'AAA')!;
  const spoken = tooltipText(buildTooltipContent(pendingSourceTip));
  assert.match(spoken, /Source country Loading: Name is loading/);
  assert.match(spoken, /Manpower 300/);
});

test('occupation-reserve-manpower surfaces missing refusal as unavailable and handles unpopulated default Top cleanly', () => {
  const unpopulated = createBoundedReadView({ hour: displayedHour(240), panels: new Map() });
  const pending = occupationReserveRaceProjection.project(unpopulated, { surface: '#1e1e1e', top: 10, all: false, group: false });
  assert.equal(pending.rows.length, 0);
  assert.equal(pending.empty, undefined);

  const missing: OccupationReserveReply = {
    holders: [],
    readHour: null,
    unnamed: 0,
    noneMissing: null,
    missing: 'Occupation reserve was not recorded at this hour',
    warnings: [],
  };
  const refused = occupationReserveRaceProjection.project(
    createBoundedReadView({ hour: displayedHour(240), panels: new Map([['main', missing]]) }),
    { surface: '#1e1e1e', top: 10, all: false, group: false },
  );
  assert.equal(refused.emptyKind, 'unavailable');
  assert.equal(refused.empty, missing.missing);
  const unproven = occupationReserveRaceProjection.project(
    createBoundedReadView({ hour: displayedHour(240), panels: new Map([['main', { ...missing, missing: null, unnamed: 2, noneMissing: '2 holders the recording names no tag for hold reserve manpower at this hour' }]]) }),
    { surface: '#1e1e1e', top: 10, all: false, group: false },
  );
  assert.deepEqual([unproven.emptyKind, unproven.empty], ['unavailable', '2 holders the recording names no tag for hold reserve manpower at this hour'], 'an empty reserve is claimed only with the evidence for it');
});

test('occupation-reserve-manpower breakdown asks history to the hour, asks hover by recorded slot, and rejects future or foreign-holder leaks', async () => {
  const entry = FAMILY_VIEWS.find(item => item.kind === 'occupation-reserve-manpower' && item.view === 'breakdown');
  assert.equal(entry?.family, 'time-chart');
  assert.equal(entry?.viewOption, 'show');
  assert.deepEqual(entry?.options?.map(field => field.key), ['holder', 'show', 'yMin', 'yMax']);
  const spec = await entry!.loadSpec();
  assert.equal(spec.projection, occupationReserveBreakdownProjection);

  const aux: CountryIdentitiesReply = {
    branchId: '1',
    known: [],
    unknown: [],
    countries: [
      { tag: 'AAA', identities: [{ countryId: 2, slot: 7, firstHour: 100, lastHour: 450 }], unresolved: [] },
    ],
    warnings: [],
  };
  const ctx = {
    track: { source: { id: 's1', revision: 'r1' }, axisOffset: 0 },
    branch: { id: '1', firstHour: 100, lastHour: 450 },
    options: { holder: 'AAA|2' },
    countries: { all: false, top: null, tags: ['AAA'], holders: null },
    aux,
  } as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const plan = occupationReserveBreakdownDeclaration.sources(ctx, { width: 600, height: 260, following: false, staticSpan: [100, 450] });
  const histRead = plan.entries.find(e => e.name === 'history')?.read as BranchToHourSourceRead;
  const hoverRead = plan.entries.find(e => e.name === 'hover')?.read as ExactHoverSourceRead;
  assert.deepEqual(
    histRead.buildSpanParams!(histRead.params!, { firstHour: 100, lastHour: 300 } as Parameters<NonNullable<BranchToHourSourceRead['buildSpanParams']>>[1]),
    { branchId: '1', holder: 2, hour: 300 },
  );
  assert.deepEqual(histRead.dynamicKeys, ['hour']);
  assert.deepEqual(hoverRead.params, { branchId: '1', countries: [7] }, 'exact hover asks by recorded slot rather than reused tag');

  const history: ReserveHistoryReply = {
    hour: 300,
    nextChange: 351,
    segments: [
      { firstHour: 100, lastHour: 199, admitted: true, entries: [{ source: 9, amount: 300 }, { source: 8, amount: 100 }], unknown: null },
      { firstHour: 200, lastHour: 249, admitted: false, entries: null, unknown: 'The reserve was not recorded at these hours' },
      { firstHour: 250, lastHour: 300, admitted: true, entries: [{ source: 9, amount: 400 }, { source: 8, amount: -50 }, { source: 12, amount: 50 }], unknown: null },
    ],
    sources: [
      { id: 8, tags: [{ tag: 'BBB', firstHour: 100, lastHour: 300 }] },
      { id: 9, tags: [{ tag: 'CCC', firstHour: 100, lastHour: 220 }, { tag: 'CCC2', firstHour: 221, lastHour: 300 }] },
      { id: 12, tags: [{ tag: null, firstHour: 250, lastHour: 300 }] },
    ],
    warnings: [],
  };

  const datedIdentity: IdentityProvider = {
    resolve(kind, key, atHour) {
      const name = key === 'AAA' ? 'Alpha'
        : key === 'BBB' ? 'Beta'
          : key === 'CCC' ? (atHour <= 220 ? 'Old Gamma' : 'Gamma')
            : key === 'CCC2' ? 'New Gamma'
              : null;
      const color = key === 'BBB' ? '#22c55e' : key === 'CCC' || key === 'CCC2' ? '#f59e0b' : '#6b7280';
      return {
        kind,
        key,
        atHour,
        holder: { status: 'ready', value: key },
        tag: { status: 'ready', value: key },
        name: name ? { status: 'ready', value: name } : { status: 'absent', reason: 'Name not recorded' },
        colour: { status: 'ready', value: color },
        flagChoice: { status: 'absent', reason: 'Flag not recorded' },
        validInterval: [0, 1000],
        nextChangeAt: Infinity,
      };
    },
  };

  const hoverReply: OccupationReserveReply = {
    holders: [{
      country: { id: 2, tag: 'AAA' },
      slot: null,
      admitted: true,
      total: 300,
      entries: [
        { ordinal: 0, source: { id: 9, tag: 'CCC' }, amount: 300 },
        { ordinal: 1, source: { id: 8, tag: 'BBB' }, amount: -25 },
      ],
    }],
    readHour: 150,
    unnamed: 0,
    noneMissing: null,
    missing: null,
    warnings: [],
  };

  const view = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map<string, unknown>([['history', history]]),
    hovers: new Map([['hover', { hour: 150, data: hoverReply }]]),
    identity: datedIdentity,
  });

  assert.equal(occupationReserveBreakdownProjection.nextChange?.(view, {
    holder: { tag: 'AAA', id: 2, slot: 7 },
    extent: [100, 450],
    gaps: [],
    surface: '#161616',
    yMin: 0,
    yMax: 1000,
  }), 351);

  const model = occupationReserveBreakdownProjection.project(view, {
    holder: { tag: 'AAA', id: 2, slot: 7 },
    extent: [100, 450],
    gaps: [],
    surface: '#161616',
    yMin: 0,
    yMax: 1000,
  });

  assert.equal(model.mode, 'stack');
  assert.equal(model.stack?.step, true);
  assert.deepEqual([model.yAxis.min, model.yAxis.max], [0, 1000]);
  assert.deepEqual(model.stack?.bands.map(b => b.id), ['9', '8', '12']);
  assert.equal(model.stack?.bands[0]!.name, 'New Gamma', 'band name reflects drawn hour identity');
  assert.equal(model.stack?.bands[2]!.name, 'Source #12 (tag not recorded)', 'unresolved source keeps recorded key and reason');
  assert.deepEqual(model.stack?.hours, [100, 200, 250]);
  assert.deepEqual(model.stack?.bands[0]!.values, [300, null, 400]);
  assert.deepEqual(model.stack?.bands[1]!.values, [100, null, 0]);
  assert.deepEqual(model.stack?.bands[2]!.values, [0, null, 50]);
  assert.ok(model.gaps?.some(g => g.firstHour === 200 && g.lastHour === 249));
  assert.equal(model.note, 'Negative entries: Beta −50');
  assert.deepEqual(model.auditRows?.map(r => [r.key, r.value]), [['__total__', '450'], ['9', '400'], ['12', '50']]);

  const layout = hoverTimeChartLayout(layoutTimeChart(model, 600, 260, 'sans-serif', t => t.length * 6), null);
  const tip = deriveTimeChartTooltip(model, layout, 'plot:150');
  assert.ok(tip?.payload?.items?.some(item => item.label === 'Old Gamma' && item.value === '300'), 'hover resolves source name at hovered hour');
  assert.ok(tip?.payload?.items?.some(item => item.label === 'Negative entries'));
  assert.ok(tip?.payload?.items?.some(item => item.label === 'Beta' && item.value === '−25'));

  const foreignHolderHover: OccupationReserveReply = {
    holders: [{ country: { id: 99, tag: 'AAA' }, slot: null, admitted: true, total: 999, entries: [{ ordinal: 0, source: { id: 8, tag: 'BBB' }, amount: 999 }] }],
    readHour: 220,
    unnamed: 0,
    noneMissing: null,
    missing: null,
    warnings: [],
  };
  const unreadHoverView = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map<string, unknown>([['history', history]]),
    hovers: new Map([['hover', { hour: 220, data: foreignHolderHover }]]),
    identity: datedIdentity,
  });
  const unreadHoverModel = occupationReserveBreakdownProjection.project(unreadHoverView, {
    holder: { tag: 'AAA', id: 2, slot: 7 },
    extent: [100, 450],
    gaps: [],
    surface: '#161616',
    yMin: null,
    yMax: null,
  });
  assert.equal(unreadHoverModel.hoverDetail?.items[0]?.missingReason, 'The reserve was not recorded at these hours', 'hover never falls back to a different country id and preserves unread coverage');

  const unreadHeadHistory: ReserveHistoryReply = {
    hour: 220,
    nextChange: 250,
    segments: [
      { firstHour: 100, lastHour: 199, admitted: true, entries: [], unknown: null },
      { firstHour: 200, lastHour: 220, admitted: false, entries: null, unknown: 'Occupation settings were not recorded at these hours' },
    ],
    sources: [],
    warnings: [],
  };
  const unreadHeadModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(220), panels: new Map<string, unknown>([['history', unreadHeadHistory]]), identity: datedIdentity }),
    { holder: { tag: 'AAA', id: 2, slot: 7 }, extent: [100, 450], gaps: [], surface: '#161616', yMin: null, yMax: null },
  );
  assert.equal(unreadHeadModel.empty?.kind, 'unavailable');
  assert.equal(unreadHeadModel.empty?.message, 'Occupation settings were not recorded at these hours');
  assert.deepEqual(unreadHeadModel.auditRows, []);

  const lane = createLaneView(timeChartLane, () => {});
  assert.ok(lane);
  const unchosen = occupationReserveBreakdownProjection.project(view, {
    holder: null,
    extent: [100, 450],
    gaps: [],
    surface: '#161616',
    yMin: null,
    yMax: null,
  });
  assert.equal(unchosen.empty?.message, 'Choose a holder: click its bar in the race.');
  assert.equal(LANE_THEME.surface.length > 0, true);
});

test('occupation-reserve-manpower breakdown follows the holder at the displayed hour and never backfills a missing source tag', async () => {
  const aux: CountryIdentitiesReply = {
    branchId: '1',
    known: [],
    unknown: [],
    countries: [
      {
        tag: 'AAA',
        identities: [
          { countryId: 10, slot: 7, firstHour: 0, lastHour: 4 },
          { countryId: 11, slot: 8, firstHour: 5, lastHour: 10 },
        ],
        unresolved: [],
      },
      {
        tag: 'BBB',
        identities: [
          { countryId: 12, slot: 7, firstHour: 5, lastHour: 10 },
        ],
        unresolved: [],
      },
    ],
    warnings: [],
  };
  const makeCtx = (tags: readonly string[], planHour: number) => ({
    track: { source: { id: 's1', revision: 'r1' }, axisOffset: 0 },
    branch: { id: '1', firstHour: 0, lastHour: 12 },
    options: {},
    countries: { all: false, top: null, tags, holders: null },
    aux,
    planHour,
  }) as unknown as FamilyPlanContext<CountryIdentitiesReply>;

  const planAt3 = occupationReserveBreakdownDeclaration.sources(makeCtx(['AAA'], 3), { width: 600, height: 260, following: true, staticSpan: null });
  const histAt3 = planAt3.entries.find(e => e.name === 'history')?.read as BranchToHourSourceRead;
  const hoverAt3 = planAt3.entries.find(e => e.name === 'hover')?.read as ExactHoverSourceRead;
  assert.deepEqual(histAt3.params, { branchId: '1', holder: 10 });
  assert.deepEqual(hoverAt3.params, { branchId: '1', countries: [7] });
  assert.deepEqual(planAt3.valid, [0, 4]);

  const planAt8 = occupationReserveBreakdownDeclaration.sources(makeCtx(['AAA'], 8), { width: 600, height: 260, following: true, staticSpan: null });
  const histAt8 = planAt8.entries.find(e => e.name === 'history')?.read as BranchToHourSourceRead;
  const hoverAt8 = planAt8.entries.find(e => e.name === 'hover')?.read as ExactHoverSourceRead;
  assert.deepEqual(histAt8.params, { branchId: '1', holder: 11 });
  assert.deepEqual(hoverAt8.params, { branchId: '1', countries: [8] });
  assert.deepEqual(planAt8.valid, [5, 10]);
  assert.deepEqual(
    occupationReserveBreakdownDeclaration.projectionOptions(makeCtx(['AAA'], 8), { extent: [0, 12], surface: '#161616' }).holder,
    { tag: 'AAA', id: 11, slot: 8 },
  );

  const slotPlanAt8 = occupationReserveBreakdownDeclaration.sources(makeCtx([countryKey(7)], 8), { width: 600, height: 260, following: true, staticSpan: null });
  const slotHistAt8 = slotPlanAt8.entries.find(e => e.name === 'history')?.read as BranchToHourSourceRead;
  const slotHoverAt8 = slotPlanAt8.entries.find(e => e.name === 'hover')?.read as ExactHoverSourceRead;
  assert.deepEqual(slotHistAt8.params, { branchId: '1', holder: 12 });
  assert.deepEqual(slotHoverAt8.params, { branchId: '1', countries: [7] });
  assert.deepEqual(slotPlanAt8.valid, [5, 10]);

  const absentPlanAt12 = occupationReserveBreakdownDeclaration.sources(makeCtx(['AAA'], 12), { width: 600, height: 260, following: true, staticSpan: null });
  assert.equal(absentPlanAt12.entries.length, 0);
  assert.equal(
    occupationReserveBreakdownDeclaration.projectionOptions(makeCtx(['AAA'], 12), { extent: [0, 12], surface: '#161616' }).holder,
    null,
  );
  const declaration = occupationReserveBreakdownDeclaration;
  const before = makeCtx(['AAA'], 3), after = makeCtx(['AAA'], 12);
  const at = stripContextAt(before, planAt3.valid, 12, before.countryTag, before.countryName);
  const extra = { extent: [0, 12] as const, surface: '#161616' };
  const readOptions = { following: true, timeMode: 'follow' as const, branch: after.branch, window: null, staticSpan: null, gate: openGate(0, 12) };
  const plan = declaration.sources(at, { following: true, staticSpan: null });
  const keyframe = buildStripKeyframe(plan, gatedHour(12), readOptions, declaration.projectionOptions(at, extra));
  assert.equal(keyframe.asks.length, 0);
  const view = createLaneView(timeChartLane, () => {});
  const lane = new Lane({
    lane: 0, lanes: 1, views: () => view, crop: async () => assert.fail('scrub must not crop'), present: () => assert.fail('scrub must not present'),
    release() {}, fail: assert.fail, later() {},
  });
  lane.mount(1, [], 'time-chart', { family: 'time-chart', kind: declaration.kind, view: 'breakdown', options: declaration.projectionOptions(before, extra) });
  lane.strip(1, 1, { kind: 'scenes', page: 2, fine: false, frames: [[12, keyframe.scene!]] });
  lane.scrub(2, { page: 2, frame: 1, hour: 12, density: 1, gap: null });
  const actual = lane.scrubScenes?.widgets.get(1);
  assert.equal(actual?.mode, 'picture');
  const client = { product: () => assert.fail('an absent holder builds no request') } as unknown as DashboardClient;
  const seek = await readPlanExact(absentPlanAt12, 12, { ...readOptions, client });
  assert.ok(seek);
  assert.deepEqual(actual.picture.model, declaration.projection.project(seek, declaration.projectionOptions(after, extra)));

  const explicitGapCtx = { ...makeCtx(['AAA'], 8), options: { holder: 'AAA|10' } } as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const explicitGapPlan = occupationReserveBreakdownDeclaration.sources(explicitGapCtx, { width: 600, height: 260, following: true, staticSpan: null });
  assert.equal(explicitGapPlan.entries.length, 0, 'explicit holder in an identity gap never falls back to its first slot while another country occupies it');
  assert.deepEqual(explicitGapPlan.valid, [5, 12]);
  assert.equal(
    occupationReserveBreakdownDeclaration.projectionOptions(explicitGapCtx, { extent: [0, 12], surface: '#161616' }).holder,
    null,
  );

  const unresolvedAux: CountryIdentitiesReply = {
    branchId: '1',
    known: [],
    unknown: [],
    countries: [
      { tag: 'AAA', identities: [{ countryId: 10, slot: 7, firstHour: 0, lastHour: 4 }], unresolved: [{ firstHour: 5, lastHour: 8 }] },
    ],
    warnings: [],
  };
  const makeUnresolvedCtx = (planHour: number) => ({
    ...makeCtx(['AAA'], planHour),
    options: { holder: 'AAA|10' },
    aux: unresolvedAux,
  }) as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const unresAt6 = occupationReserveBreakdownDeclaration.sources(makeUnresolvedCtx(6), { width: 600, height: 260, following: true, staticSpan: null });
  assert.deepEqual(unresAt6.valid, [5, 8], 'unresolved identity span ends before proven absence');
  assert.equal(
    occupationReserveBreakdownDeclaration.projectionOptions(makeUnresolvedCtx(6), { extent: [0, 12], surface: '#161616' }).holderReason,
    'Country identity unresolved at this hour',
  );
  const absentAt9 = occupationReserveBreakdownDeclaration.sources(makeUnresolvedCtx(9), { width: 600, height: 260, following: true, staticSpan: null });
  assert.deepEqual(absentAt9.valid, [9, 12]);
  assert.equal(
    occupationReserveBreakdownDeclaration.projectionOptions(makeUnresolvedCtx(9), { extent: [0, 12], surface: '#161616' }).holderReason,
    'not a country at this hour',
  );

  const history: ReserveHistoryReply = {
    hour: 8,
    nextChange: 15,
    segments: [
      { firstHour: 0, lastHour: 4, admitted: true, entries: [{ source: 9, amount: 200 }, { source: 8, amount: 50 }], unknown: null },
      { firstHour: 5, lastHour: 8, admitted: true, entries: [{ source: 9, amount: 250 }, { source: 8, amount: 75 }], unknown: null },
    ],
    sources: [
      { id: 9, tags: [{ tag: 'OLD', firstHour: 0, lastHour: 4 }, { tag: null, firstHour: 5, lastHour: 8 }] },
      { id: 8, tags: [{ tag: 'BBB', firstHour: 0, lastHour: 8 }] },
    ],
    warnings: [],
  };
  const identity = fakeIdentity({
    AAA: { name: 'Alpha', color: '#6b7280' },
    BBB: { name: 'Beta', color: '#3b82f6' },
    OLD: { name: 'Old Name', color: '#22c55e' },
  });
  const model = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(8), panels: new Map<string, unknown>([['history', history]]), identity }),
    { holder: { tag: 'AAA', id: 11, slot: 8 }, extent: [0, 14], gaps: [], surface: '#161616', yMin: null, yMax: null },
  );
  const band = model.stack?.bands[0];
  assert.equal(band?.key, '9');
  assert.equal(band?.name, 'Source #9 (tag not recorded)');
  assert.equal(band?.flag, undefined);
  assert.equal(band?.endLabel?.name, 'Source #9 (tag not recorded)');

  const reusedModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(12), panels: new Map<string, unknown>([['history', history]]), identity }),
    { holder: { tag: 'AAA', id: 11, slot: 8 }, extent: [0, 14], gaps: [], surface: '#161616', yMin: null, yMax: null },
  );
  assert.equal(reusedModel.gaps, undefined, 'reused reply within proven interval adds no trailing gap');
  assert.equal(reusedModel.stack?.bands[0]?.name, 'Source #9 (tag not recorded)', 'explicit null tag at reply hour stays null across proven interval');
  assert.equal(reusedModel.stack?.bands[1]?.name, 'Beta', 'known tag at reply hour persists across proven interval');
  assert.deepEqual(reusedModel.auditRows?.map(r => [r.key, r.value]), [['__total__', '325'], ['9', '250'], ['8', '75']]);
});

test('occupation-reserve-manpower breakdown keeps unread source bands null and withholds partial subtotals as totals', () => {
  const history: ReserveHistoryReply = {
    hour: 200,
    nextChange: null,
    segments: [
      { firstHour: 100, lastHour: 149, admitted: true, entries: [{ source: 9, amount: 300 }, { source: 8, amount: 100 }], unknown: null },
      { firstHour: 150, lastHour: 179, admitted: false, entries: [{ source: 9, amount: 250 }], unknown: null },
      { firstHour: 180, lastHour: 200, admitted: true, entries: [{ source: 9, amount: 250 }], unknown: null },
    ],
    sources: [
      { id: 9, tags: [{ tag: 'CCC', firstHour: 100, lastHour: 200 }] },
      { id: 8, tags: [{ tag: 'BBB', firstHour: 100, lastHour: 200 }] },
    ],
    warnings: [],
  };
  const identity = fakeIdentity({
    AAA: { name: 'Alpha', color: '#6b7280' },
    BBB: { name: 'Beta', color: '#22c55e' },
    CCC: { name: 'Gamma', color: '#f59e0b' },
  });
  const opts = { holder: { tag: 'AAA', id: 2, slot: 7 }, extent: [100, 200] as const, gaps: [], surface: '#161616', yMin: null, yMax: null };

  const partialHistory: ReserveHistoryReply = { ...history, hour: 170, segments: history.segments.slice(0, 2) };
  const partialModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(170), panels: new Map<string, unknown>([['history', partialHistory]]), identity }),
    opts,
  );
  assert.deepEqual(partialModel.stack?.hours, [100, 150]);
  assert.deepEqual(partialModel.stack?.bands[0]?.values, [300, 250]);
  assert.deepEqual(partialModel.stack?.bands[1]?.values, [100, null]);
  assert.ok(partialModel.stack?.bands[1]?.missingReason?.[1]);
  assert.deepEqual(partialModel.stack?.total?.values, [400, null]);
  const totalMissing = (partialModel.stack?.total as { readonly missingReason?: readonly (string | null)[] } | undefined)?.missingReason?.[1];
  assert.ok(totalMissing && !/at least|≥|\+$/.test(totalMissing), `missing total carries a plain reason without lower-bound wording: ${String(totalMissing)}`);

  const totalAudit = partialModel.auditRows?.find(row => row.key === '__total__')?.value ?? '';
  assert.ok(totalAudit.length > 0 && totalAudit !== '250' && !/at least|≥|\+$/.test(totalAudit));
  assert.equal(partialModel.auditRows?.find(row => row.key === '9')?.value, '250');
  assert.equal(partialModel.auditRows?.find(row => row.key === '8')?.value, partialModel.stack?.bands[1]?.missingReason?.[1]);

  const layout = hoverTimeChartLayout(layoutTimeChart(partialModel, 600, 260, 'sans-serif', t => t.length * 6), null);
  const stackTip = deriveTimeChartTooltip(partialModel, layout, 'plot:160');
  const tipItems = stackTip?.payload?.items ?? [];
  assert.equal(tipItems[0]?.value, undefined);
  assert.ok(tipItems[0]?.missingReason && !/at least|≥|\+$/.test(tipItems[0].missingReason));
  assert.ok(tipItems.some(item => item.label === 'Gamma' && item.value === '250'));
  assert.ok(tipItems.some(item => item.label === 'Beta' && Boolean(item.missingReason) && item.value === undefined));

  const unadmittedHover: OccupationReserveReply = {
    holders: [{ country: { id: 2, tag: 'AAA' }, slot: 7, admitted: false, total: 250, entries: [{ ordinal: 0, source: { id: 9, tag: 'CCC' }, amount: 250 }] }],
    readHour: 160,
    unnamed: 0,
    noneMissing: 'Which countries hold an occupation reserve is not known at this hour: not recorded',
    missing: null,
    warnings: [],
  };
  const exactHoverModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({
      hour: displayedHour(170),
      panels: new Map<string, unknown>([['history', partialHistory]]),
      hovers: new Map([['hover', { hour: 160, data: unadmittedHover }]]),
      identity,
    }),
    opts,
  );
  assert.equal(exactHoverModel.hoverDetail?.items[0]?.value, undefined);
  assert.ok(exactHoverModel.hoverDetail?.items[0]?.missingReason);
  assert.ok(exactHoverModel.hoverDetail?.items.some(item => item.label === 'Beta' && Boolean(item.missingReason)));

  const fullModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(200), panels: new Map<string, unknown>([['history', history]]), identity }),
    opts,
  );
  assert.deepEqual(fullModel.stack?.bands[1]?.values, [100, null, 0]);
  assert.deepEqual(fullModel.stack?.total?.values, [400, null, 250]);
  assert.deepEqual(fullModel.auditRows?.map(r => [r.key, r.value]), [['__total__', '250'], ['9', '250']]);

  const omittedHolderHover: OccupationReserveReply = {
    holders: [],
    readHour: 120,
    unnamed: 0,
    noneMissing: null,
    missing: null,
    warnings: [],
  };
  const omittedHoverModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({
      hour: displayedHour(200),
      panels: new Map<string, unknown>([['history', history]]),
      hovers: new Map([['hover', { hour: 120, data: omittedHolderHover }]]),
      identity,
    }),
    opts,
  );
  assert.equal(omittedHoverModel.hoverDetail?.items[0]?.value, '400', 'omitted holder in exact hover uses hovered segment parts and total instead of false zero');
  assert.ok(omittedHoverModel.hoverDetail?.items.some(item => item.label === 'Gamma' && item.value === '300'));
  assert.ok(omittedHoverModel.hoverDetail?.items.some(item => item.label === 'Beta' && item.value === '100'));

  const witnessedZeroHistory: ReserveHistoryReply = {
    hour: 120,
    nextChange: null,
    segments: [{ firstHour: 100, lastHour: 120, admitted: false, entries: [{ source: 9, amount: 0 }], unknown: null }],
    sources: [{ id: 9, tags: [{ tag: 'CCC', firstHour: 100, lastHour: 120 }] }],
    warnings: [],
  };
  const witnessedZeroModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(120), panels: new Map<string, unknown>([['history', witnessedZeroHistory]]), identity }),
    opts,
  );
  assert.equal(witnessedZeroModel.empty?.kind, 'empty');
  assert.deepEqual(witnessedZeroModel.auditRows?.map(r => [r.key, r.value]), [['__total__', '0']]);

  const negativeOnlyUnreadHistory: ReserveHistoryReply = {
    hour: 170,
    nextChange: null,
    segments: [
      { firstHour: 100, lastHour: 149, admitted: true, entries: [{ source: 9, amount: 250 }, { source: 8, amount: -40 }], unknown: null },
      { firstHour: 150, lastHour: 170, admitted: false, entries: [{ source: 9, amount: 250 }], unknown: null },
    ],
    sources: [
      { id: 9, tags: [{ tag: 'CCC', firstHour: 100, lastHour: 170 }] },
      { id: 8, tags: [{ tag: 'BBB', firstHour: 100, lastHour: 170 }] },
    ],
    warnings: [],
  };
  const negUnreadModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(170), panels: new Map<string, unknown>([['history', negativeOnlyUnreadHistory]]), identity }),
    opts,
  );
  const negBand = negUnreadModel.stack?.bands.find(b => b.id === '8');
  assert.deepEqual(negBand?.values, [null, null], 'negative-only source keeps plotted area empty');
  assert.ok(negBand?.missingReason?.[1], 'negative-only unread source carries missing reason into stack hover');
  assert.equal(negUnreadModel.auditRows?.find(row => row.key === '8')?.value, negBand?.missingReason?.[1], 'audit lists unread negative-only source');
  const negUnreadLayout = hoverTimeChartLayout(layoutTimeChart(negUnreadModel, 600, 260, 'sans-serif', t => t.length * 6), null);
  const negUnreadTip = deriveTimeChartTooltip(negUnreadModel, negUnreadLayout, 'plot:160');
  assert.ok(negUnreadTip?.payload?.items?.some(item => item.label === 'Beta' && Boolean(item.missingReason) && item.value === undefined));

  const allUnreadWithNegHistory: ReserveHistoryReply = {
    hour: 170,
    nextChange: null,
    segments: [
      { firstHour: 100, lastHour: 149, admitted: true, entries: [{ source: 9, amount: 250 }, { source: 8, amount: -40 }], unknown: null },
      { firstHour: 150, lastHour: 170, admitted: false, entries: [], unknown: null },
    ],
    sources: [
      { id: 9, tags: [{ tag: 'CCC', firstHour: 100, lastHour: 170 }] },
      { id: 8, tags: [{ tag: 'BBB', firstHour: 100, lastHour: 170 }] },
    ],
    warnings: [],
  };
  const allUnreadModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(170), panels: new Map<string, unknown>([['history', allUnreadWithNegHistory]]), identity }),
    opts,
  );
  const allUnreadNegBand = allUnreadModel.stack?.bands.find(b => b.id === '8');
  assert.deepEqual(allUnreadNegBand?.values, [null, null]);
  assert.ok(allUnreadNegBand?.missingReason?.[1], 'negative-only source keeps missing reason even when every positive band is unread');
  assert.equal(allUnreadModel.auditRows?.find(row => row.key === '8')?.value, allUnreadNegBand?.missingReason?.[1]);

  const noPositiveUnreadHistory: ReserveHistoryReply = {
    hour: 170,
    nextChange: null,
    segments: [
      { firstHour: 100, lastHour: 149, admitted: true, entries: [{ source: 8, amount: -40 }], unknown: null },
      { firstHour: 150, lastHour: 170, admitted: false, entries: [], unknown: null },
    ],
    sources: [{ id: 8, tags: [{ tag: 'BBB', firstHour: 100, lastHour: 170 }] }],
    warnings: [],
  };
  const noPosUnreadModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(170), panels: new Map<string, unknown>([['history', noPositiveUnreadHistory]]), identity }),
    opts,
  );
  assert.equal(noPosUnreadModel.empty?.kind, 'unavailable');
  assert.equal(noPosUnreadModel.auditRows?.find(row => row.key === '__total__')?.value, noPosUnreadModel.empty?.message);
  assert.equal(noPosUnreadModel.auditRows?.find(row => row.key === '8')?.value, noPosUnreadModel.empty?.message);
});

test('occupation-reserve-manpower breakdown rejects malformed identities aux and keeps pending inputs in loading state', () => {
  const validate = occupationReserveBreakdownDeclaration.aux!.validate!;
  assert.throws(
    () => validate({
      branchId: '1',
      countries: [{ tag: 'AAA', identities: [{ slot: 1, countryId: 101, firstHour: 100, lastHour: 200 }] }],
      unknown: [],
      warnings: [],
    }),
    /malformed/i,
    'a country entry missing unresolved spans is rejected at the aux boundary rather than crashing render',
  );
  assert.throws(
    () => validate({
      branchId: '1',
      countries: [{ tag: 'AAA', identities: [], unresolved: [] }],
      warnings: [],
    }),
    /malformed/i,
    'an identities reply missing unknown spans is rejected at the aux boundary',
  );
  assert.throws(
    () => validate({
      branchId: '1',
      known: [],
      countries: [{ tag: 'AAA', identities: [{ slot: 1, countryId: 101, firstHour: 100, lastHour: 200 }], unresolved: [] }],
      unknown: [{ firstHour: 151, lastHour: 149, reason: 'reversed' }],
      warnings: [],
    }),
    /malformed/i,
    'reversed hour spans in unknown coverage are rejected',
  );
  assert.throws(
    () => validate({
      branchId: '1',
      known: [],
      countries: [{ tag: 'AAA', identities: [{ slot: -1, countryId: 101, firstHour: 100, lastHour: 200 }], unresolved: [] }],
      unknown: [],
      warnings: [],
    }),
    /malformed/i,
    'negative slot outside u32 range is rejected',
  );
  assert.throws(
    () => validate({
      branchId: '1',
      known: [],
      countries: [{ tag: 'AAA', identities: [{ slot: 1, countryId: -1, firstHour: 100, lastHour: 200 }], unresolved: [] }],
      unknown: [],
      warnings: [],
    }),
    /malformed/i,
    'negative countryId outside u32 range is rejected',
  );
  assert.throws(
    () => validate({
      branchId: '1',
      known: [],
      countries: [{ tag: 'AAA', identities: [{ slot: 1, countryId: 0x1_0000_0000, firstHour: 100, lastHour: 200 }], unresolved: [] }],
      unknown: [],
      warnings: [],
    }),
    /malformed/i,
    'countryId above u32 max is rejected',
  );
  assert.doesNotThrow(
    () => validate({
      branchId: '1',
      known: [],
      countries: [{ tag: 'AAA', identities: [{ slot: 0, countryId: 0, firstHour: 100, lastHour: 100 }], unresolved: [] }],
      unknown: [],
      warnings: [],
    }),
    'u32 lower bound zero and single-hour spans are accepted',
  );

  const validAux = validate({
    branchId: '1',
    known: [],
    countries: [{ tag: 'AAA', identities: [{ slot: 1, countryId: 101, firstHour: 100, lastHour: 200 }], unresolved: [] }],
    unknown: [],
    warnings: [],
  });
  const baseCtx = {
    track: { id: 'main', source: { id: 's1', revision: 'r1' }, axisOffset: 0 },
    branch: { id: '1', firstHour: 100, lastHour: 200, gaps: [] },
    options: {},
    planHour: 150,
  };
  const allCtx = {
    ...baseCtx,
    countries: { all: true, top: null, tags: [], holders: null },
    aux: validAux,
  } as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const allPlan = occupationReserveBreakdownDeclaration.sources(allCtx, { following: true, staticSpan: null });
  assert.equal(allPlan.entries.length, 0);
  const allOpts = occupationReserveBreakdownDeclaration.projectionOptions(allCtx, { extent: [100, 200], surface: '#1e1e1e' });
  assert.equal(allOpts.holder, null);
  assert.equal(allOpts.resolvingHolder, false);
  const allModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(150), panels: new Map() }),
    allOpts,
  );
  assert.equal(allModel.empty?.kind, 'empty');
  assert.match(allModel.empty?.message ?? '', /Choose a holder/);

  const pendingSingleCtx = {
    ...baseCtx,
    countries: { all: false, top: null, tags: ['AAA'], holders: null },
    aux: undefined,
  } as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const pendingSingleOpts = occupationReserveBreakdownDeclaration.projectionOptions(pendingSingleCtx, { extent: [100, 200], surface: '#1e1e1e' });
  assert.equal(pendingSingleOpts.holder, null);
  assert.equal(pendingSingleOpts.resolvingHolder, true);
  const pendingSingleModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(150), panels: new Map() }),
    pendingSingleOpts,
  );
  assert.equal(pendingSingleModel.empty, null, 'pending single-country identities stay loading rather than prompting to choose a holder');

  const pendingExplicitCtx = {
    ...baseCtx,
    options: { holder: 'AAA|101' },
    countries: { all: true, top: null, tags: [], holders: null },
    aux: undefined,
  } as unknown as FamilyPlanContext<CountryIdentitiesReply>;
  const pendingExplicitOpts = occupationReserveBreakdownDeclaration.projectionOptions(pendingExplicitCtx, { extent: [100, 200], surface: '#1e1e1e' });
  const pendingExplicitModel = occupationReserveBreakdownProjection.project(
    createBoundedReadView({ hour: displayedHour(150), panels: new Map() }),
    pendingExplicitOpts,
  );
  assert.equal(pendingExplicitModel.empty, undefined, 'pending history for an explicit holder stays loading');
});

test('occupation-reserve-manpower drawer ignores chart-coordinate and untyped actions and opens only for a typed country row', async () => {
  let capturedActivate: ((action: InstalledAction) => void) | undefined;
  const opened: unknown[] = [];
  const harness = {
    captureActivate(fn: ((action: InstalledAction) => void) | undefined) { capturedActivate = fn; },
    setDetail(next: unknown) { opened.push(next); },
  };
  const host = globalThis as typeof globalThis & { __ormDetailHarness?: typeof harness };
  host.__ormDetailHarness = harness;

  const vite = await createServer({
    configFile: false,
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, watch: null, hmr: false, ws: false },
    plugins: [{
      name: 'orm-detail-action-test',
      enforce: 'pre',
      transform(code, id) {
        const norm = id.replaceAll('\\', '/');
        if (norm.endsWith('/widgets/occupation-reserve-manpower/family.tsx')) {
          return code
            .replace("from 'react'", "from 'orm-test-react'")
            .replace("from '../../dashboard/regionGate'", "from 'orm-test-gate'")
            .replace("from '../../families/host/index.ts'", "from 'orm-test-detailHost'")
            .replace("from '../../families/host/registry.tsx'", "from 'orm-test-familyWidget'")
            .replace("import './occupationReserve.css';", '');
        }
      },
      resolveId(id) {
        if (id.startsWith('orm-test-')) return `\0${id}`;
      },
      load(id) {
        if (id === '\0orm-test-react') {
          return `import { useCallback, useRef } from 'react';
            export { useCallback, useRef };
            export const useState = init => [init, next => globalThis.__ormDetailHarness.setDetail(next)];`;
        }
        if (id === '\0orm-test-gate') {
          return `export const useGate = () => ({ recorded: hour => hour });`;
        }
        if (id === '\0orm-test-detailHost') {
          return `export const DetailWindowHost = () => null;`;
        }
        if (id === '\0orm-test-familyWidget') {
          return `export const FamilyWidget = props => {
            globalThis.__ormDetailHarness.captureActivate(props.onActivate);
            return null;
          };`;
        }
      },
    }],
  });

  try {
    const { default: renderer } = await vite.ssrLoadModule('/ui/src/widgets/occupation-reserve-manpower/family.tsx') as typeof import('./family.tsx');
    renderToStaticMarkup(createElement(renderer.render, {
      widget: { id: 'w1', kind: 'occupation-reserve-manpower', trackIds: ['t1'], countries: { kind: 'all' }, time: { mode: 'follow' } } as never,
      runtime: { tracks: [], clock: {}, client: {} } as never,
      active: true,
      options: {},
      mapOptions: {} as never,
      headerSlot: null,
      onDiagnostics: () => {},
      manifestDeclaration: occupationReserveRaceDeclaration,
    }));
    assert.ok(capturedActivate, 'race FamilyWidget receives onActivate');

    const basePicture = (owner: string, nav: readonly FamilyNavTuple[]): InstalledPicture => ({
      id: 1,
      owner,
      generation: { seek: 1, reset: 0, binding: 1 },
      factRevisions: {},
      validInterval: [0, 1000],
      through: 1000,
      hour: displayedHour(240),
      viewport: { width: 400, height: 200, dpr: 1, scrollLeft: 0, scrollTop: 0 },
      optionsRevision: '1',
      themeRevision: '1',
      actionKeys: lazyActionKeys(nav, displayedHour(240)),
      accessibleDescription: null,
    });

    const chartPicture = basePicture('orm:breakdown', []);
    const chartRecord = recordInstalled(chartPicture, { meta: { transform: { axisFrom: 0, axisTo: 1000 } } }, null);
    const plotHit = parseTimeChartHit('plot:200')!;
    const plotEffect = resolveHostInput(
      timeChartHostAdapter,
      'activate',
      { key: plotHit.key, hit: plotHit, picture: chartPicture },
      { current: chartRecord, held: false, expired: false },
      { expansion: {}, takesToggles: false, regions: false, menu: false },
      () => null,
    );
    assert.equal(plotEffect?.kind, 'activate');
    if (plotEffect?.kind !== 'activate') return;

    const reply: OccupationReserveReply = {
      holders: [{
        country: { id: 2, tag: 'AAA' },
        slot: 7,
        admitted: true,
        total: 500,
        entries: [{ ordinal: 0, source: { id: 9, tag: 'CCC' }, amount: 500 }],
      }],
      readHour: 240,
      unnamed: 0,
      noneMissing: null,
      missing: null,
      warnings: [],
    };
    const projected = occupationReserveRaceProjection.project(
      createBoundedReadView({
        hour: displayedHour(240),
        panels: new Map([['main', reply]]),
        identity: fakeIdentity({ AAA: { name: 'Alpha', color: '#6b7280' }, CCC: { name: 'Gamma', color: '#f59e0b' } }),
      }),
      { surface: '#1e1e1e', top: 10, all: false, group: false },
    );
    const row = projected.rows.find((item): item is BarsDataRow => item.kind === 'data')!;
    const rowNav: FamilyNavTuple[] = [[row.key, barsLabelText(row.label), 0, -1, '', row.meta]];
    const barsPicture = basePicture('orm:race', rowNav);
    const barsRecord = recordInstalled(barsPicture, { nav: rowNav }, null);
    const rowHit = barsHostAdapter.parseHit!(`row:${row.key}`)!;
    const rowEffect = resolveHostInput(
      barsHostAdapter,
      'activate',
      { key: rowHit.key, hit: rowHit, picture: barsPicture },
      { current: barsRecord, held: false, expired: false },
      { expansion: {}, takesToggles: false, regions: false, menu: false },
      () => null,
    );
    assert.equal(rowEffect?.kind, 'activate');
    if (rowEffect?.kind !== 'activate') return;

    capturedActivate!(plotEffect.action);
    capturedActivate!({ ...plotEffect.action, meta: row.meta });
    capturedActivate!({ ...rowEffect.action, meta: undefined });
    assert.deepEqual(opened, [], 'chart-coordinate and untyped row actions must not open the country drawer');

    capturedActivate!(rowEffect.action);
    assert.deepEqual(opened, [{ tag: 'AAA', key: countryKey(7), hour: 240 }], 'typed country row opens the country drawer');
  } finally {
    delete host.__ormDetailHarness;
    await vite.close();
  }
});
