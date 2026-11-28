import assert from 'node:assert/strict';
import test from 'node:test';
import { liveKey } from '../../charts/live.ts';
import { valuesOf } from '../../test-support/liveValues.ts';
import { MEASURES } from '../../widgets/army-size/armySizeMeta.ts';
import type { EquipmentProducedRaceOptions } from '../../widgets/equipment-produced/equipmentProducedRaceProjection.ts';
import { COUNT_MODES } from '../../widgets/factories/factoriesData.ts';
import { SCORE_PARTS } from '../../widgets/game-score/gameScoreData.ts';
import { BARS_VIEWS } from '../../widgets/manifest/bars.ts';
import { createBoundedReadView, displayedHour } from '../boundedView.ts';
import { normalizeView } from '../manifest.ts';
import type { FactionTotalsRaceOptions } from './factionTotalsRace.ts';
import type { BarsPlaybackForm } from '../contract.ts';
import { isSingleValueOrdered, supportsBarsPlayback, type BarsProjection } from './model.ts';

const hour = displayedHour(100);
const settings = { top: null, group: false, surface: '#1e1e1e' };
const booleans = [false, true];
const counts = [0, 40];
const factions: FactionTotalsRaceOptions = {
  ...settings, kind: 'territory', measure: 'controlledStates', measureLabel: 'States',
  scaleUnit: 'States', aggregated: false, groupLabel: 'Faction',
};

function rank(value: number, ...measures: string[]) {
  return {
    branchId: '1', hour, from: hour, until: hour, refused: null,
    measures: measures.map(name => ({ name, decimals: 0 })),
    rows: [{
      country: 'GER',
      readings: measures.map(() => ({
        country: 'GER', hour, drop: null, missing: null,
        run: { valueRaw: String(value), firstHour: hour, lastHour: hour, slot: 1, countryId: 1, startsSegment: false, segmentFirstHour: 0, firstExact: true, lastExact: true },
      })),
    }],
  };
}

function forces(value: number) {
  return {
    firstHour: hour, lastHour: hour, missing: null, warnings: [],
    parts: [{ key: 'p', name: 'Part', unresolved: false }], ordinals: [0],
    rows: [{ country: 'GER', slot: null, tag: 'GER', total: value, referenceTotal: 60, missing: null, values: [value], partsMissing: null }],
  };
}

function factionReply(value: number, options: FactionTotalsRaceOptions) {
  return {
    hour, kind: options.kind, measure: options.measure, missing: null, warnings: [],
    rows: [{ id: '10', members: ['GER'], memberCountries: [1], total: value, missing: null }],
  };
}

function check<O>(projection: BarsProjection<O>, options: O, expected: BarsPlaybackForm | false, panel: (value: number) => unknown, source = 'main',
  live?: (amount: number) => ReadonlyMap<string, number>) {
  const context = `${projection.kind} ${JSON.stringify(options)}`;
  assert.equal(projection.singleValueBars?.(options), expected, context);
  if (!expected) return;
  for (const amount of counts) {
    const view = createBoundedReadView({
      hour, panels: new Map([[source, panel(amount)]]),
      ...(live ? { live: new Map([[source, valuesOf(live(amount), 'Not recorded')]]) } : {}),
    });
    const model = projection.project(view, options);
    assert.ok(model.rows.some(row => row.kind === 'data'), context);
    assert.ok(supportsBarsPlayback(model, expected), context);
    if ((expected === 'ordered-segments' || expected === 'share') && amount > 0) assert.ok(model.rows.some(row => row.kind === 'data' && row.segments?.length), context);
  }
  for (const view of [
    createBoundedReadView({ hour }),
    createBoundedReadView({ hour, refused: new Map([[source, 'Unavailable']]) }),
  ]) assert.ok(supportsBarsPlayback(projection.project(view, options), expected), context);
}

const cases: Record<string, (projection: BarsProjection<unknown>) => void> = {
  'air-force-size/mix': projection => {
    for (const measure of ['planes', 'wings']) for (const ownership of ['roster', 'effective']) for (const group of booleans) {
      check(projection, { ...settings, measure, ownership, group }, 'share', forces);
    }
  },
  'army-size/mix': projection => {
    for (const countFor of ['owner', 'host']) for (const group of booleans) check(projection, { ...settings, countFor, group }, 'share', forces);
  },
  'factories/mix': projection => {
    for (const { value: mode, label: modeLabel } of COUNT_MODES) for (const group of booleans) {
      check(projection, { ...settings, mode, modeLabel, group }, 'share', amount => ({
        hour, mode, missing: null, warnings: [],
        parts: [{ key: 'military', name: 'Military Factory', damaged: false, group: 'military' }],
        rows: [{ country: 1, tag: 'GER', total: amount, values: [amount], shares: [amount > 0 ? 1 : null], missing: null, partsMissing: null }],
      }));
    }
  },
  'naval-losses/mix': projection => {
    for (const kind of ['ships', 'convoys', 'both']) for (const attribution of ['lost', 'sunkBy']) for (const period of ['month', 'running']) for (const group of booleans) {
      check(projection, { ...settings, kind, attribution, period, group }, 'share', forces);
    }
  },
  'navy-size/mix': projection => {
    for (const measure of ['ships', 'strength']) for (const ownership of ['roster', 'logical']) for (const grouping of ['class', 'type']) for (const group of booleans) {
      check(projection, { ...settings, measure, ownership, grouping, group }, 'share', forces);
    }
  },
  'nuclear-arsenal/mix': projection => {
    for (const group of booleans) check(projection, { ...settings, group }, 'share', amount => ({
      types: [{ ordinal: 0, token: 'type', name: 'Type' }], missing: null, warnings: [],
      countries: [{ tag: 'GER', countryId: 1, slot: null, total: amount, counts: [{ ordinal: 0, value: amount, ready: 0 }] }],
    }));
  },
  'minefields/default': projection => {
    for (const order of ['mines', 'name']) check(projection, { ...settings, order }, 'share', () => ({
      regions: [{ region: 3, owners: [{ tag: 'ENG' }], unknownOwners: 0, pinned: false, missing: null }],
      unknown: [], empty: null, activeUnlisted: { ids: [], missing: null }, scale: 100000, warnings: [],
    }), 'main', amount => new Map([[liveKey('3', 'ENG'), amount], [liveKey('3', 'total'), amount]]));
  },
  'naval-supremacy/shares': projection => {
    check(projection, { ...settings, viewer: null, historyStart: hour, tiles: 0 }, 'share', amount => ({
      firstHour: hour, lastHour: hour, validThrough: hour, firstDayHour: hour, dayHours: 24, days: 1,
      rankMissing: null, empty: null, activeUnlisted: { ids: [], missing: null }, unknown: [], gaps: [],
      viewerName: 'Germany', viewerColor: [90, 90, 90], legend: [], scale: 100, warnings: [],
      regions: [{
        id: '17', name: 'English Channel', pinned: false, runs: [],
        ownShips: { kind: 'counted', ships: 1 }, enemyShips: { kind: 'notRecorded', reason: 'No ship count' },
        readout: { kind: 'control', control: 1, share: String(amount), friendly: String(amount), enemy: String(amount), own: String(amount), as_of_hour: hour },
      }],
    }), 'now');
  },
  'air-superiority-region/default': projection => {
    for (const mode of ['sides', 'viewpoint']) for (const order of ['contested', 'active', 'name']) for (const filteredSides of booleans) {
      const options = { mode, order, filteredSides, enemyColor: '#e5484d', mutedColor: '#8a8a8a' };
      const sides = { kind: 'sides', parts: [{ side: 0, tag: 'ENG', live: 'ENG@11' }], viewpoints: [],
        unknown: [], noAirObject: [], filtered: filteredSides, excluded: 0 };
      check(projection, options, 'share', () => ({
        missing: null, unlisted: null, empty: null, activeUnlisted: { ids: [], missing: null },
        sides: [{ key: 'c:ENG', faction: null, colourFaction: null, factions: [], tag: 'ENG', leader: 'ENG', coBelligerents: 0,
          anchor: { tag: 'ENG', hour }, name: 'United Kingdom', color: [34, 197, 94] }],
        legend: [0], viewpoint: mode === 'viewpoint' ? 0 : null,
        regions: [{ id: 11, name: 'English Channel', nameMissing: null, active: true, pinned: true, wings: 'unknown',
          cell: mode === 'viewpoint' ? { kind: 'view', side: 0, leader: 'ENG', live: 'ENG@11' } : sides }],
        fallback: { ...sides, parts: [] }, scale: 100, badBelow: 40, goodAbove: 60, warnings: [],
      }), 'main', amount => new Map([
        [liveKey('ENG@11', 'friendly'), amount],
        [liveKey('ENG@11', 'enemy'), amount],
        [liveKey('11', 'total'), mode === 'viewpoint' ? amount * 2 : amount],
      ]));
    }
  },
  'fuel-branches/bars': projection => {
    for (const digits of [0, 1, 2, 3, 4, 5]) {
      const options = { ...settings, digits };
      const reply = {
        tag: 'GER', scale: 100000, consumerNames: ['Army', 'Air'], consumerIcons: [null, null], consumerIconReasons: [null, null],
        fuelIcon: null, fuelIconReason: null, readHour: hour, consumers: [], missing: null, warnings: [],
      };
      check(projection, options, 'target-markers', amount => ({
        ...reply, consumers: [
          { index: 0, requested: String(amount * 100000), received: String(amount * 50000), priority: 1 },
          { index: 1, requested: '0', received: '0', priority: 2 },
        ],
      }));
      for (const panel of [reply, { ...reply, missing: 'Fuel not recorded' }]) {
        const view = createBoundedReadView({ hour, panels: new Map([['main', panel]]) });
        assert.ok(isSingleValueOrdered(projection.project(view, options), 'target-markers'));
      }
    }
  },
  'factories/race': projection => {
    for (const split of booleans) for (const { value: mode } of COUNT_MODES) check(projection, {
      ...settings, mode, split, modeLabel: 'Factories', damagedSource: 'damaged7',
      parts: [{ id: 'p', label: 'Part', source: `${mode}1`, color: '#ff8800' }],
    }, split || mode === 'built' ? 'ordered-segments' : true, amount => rank(amount, `${mode}7`, `${mode}1`, 'damaged7'));
  },
  'game-score/race': projection => {
    for (const split of booleans) for (const by of SCORE_PARTS) {
      const segmented = split && by === 'total';
      check(projection, { ...settings, by, split }, segmented ? 'ordered-segments' : true,
        amount => segmented ? rank(amount, by, 'army', 'navy', 'air') : rank(amount, by));
    }
  },
  'air-force-size/race': projection => {
    for (const split of booleans) for (const measure of ['planes', 'wings'] as const) for (const ownership of ['roster', 'effective'] as const) {
      check(projection, { ...settings, measure, ownership, split }, split ? 'ordered-segments' : true,
        amount => split ? forces(amount) : rank(amount, 'measure'));
    }
  },
  'army-size/race': projection => {
    for (const split of booleans) for (const { value: measure } of MEASURES) for (const countFor of ['owner', 'host'] as const) {
      const segmented = split && measure === 'battalions';
      check(projection, { ...settings, measure, countFor, split }, segmented ? 'ordered-segments' : true,
        amount => segmented ? forces(amount) : rank(amount, 'measure'));
    }
  },
  'navy-size/race': projection => {
    for (const split of booleans) for (const measure of ['ships', 'strength'] as const) for (const ownership of ['roster', 'logical'] as const) for (const grouping of ['class', 'type'] as const) {
      check(projection, { ...settings, measure, ownership, grouping, split }, split ? 'ordered-segments' : true,
        amount => !split && measure === 'ships' ? rank(amount, 'measure') : forces(amount));
    }
  },
  'naval-losses/race': projection => {
    for (const split of booleans) for (const kind of ['ships', 'convoys', 'both'] as const) for (const attribution of ['lost', 'sunkBy'] as const) for (const period of ['month', 'running'] as const) {
      check(projection, { ...settings, kind, attribution, period, split }, split ? 'ordered-segments' : true, forces);
    }
  },
  'nuclear-arsenal/race': projection => {
    for (const split of booleans) check(projection, { ...settings, split }, split ? 'ordered-segments' : true, amount => ({
      types: [{ ordinal: 0, token: 'type', name: 'Type' }], missing: null, warnings: [],
      countries: [{ tag: 'GER', countryId: 1, slot: null, total: amount, counts: [{ ordinal: 0, value: amount, ready: 0 }] }],
    }));
  },
  'occupation-reserve-manpower/main': projection => {
    for (const group of booleans) check(projection, { ...settings, group }, 'ordered-segments', amount => ({
      holders: [{ country: { id: 1, tag: 'GER' }, slot: null, admitted: true, total: amount,
        entries: [{ ordinal: 0, source: { id: 2, tag: 'FRA' }, amount }] }],
      readHour: hour, unnamed: 0, noneMissing: null, missing: null, warnings: [],
    }));
  },
  'raid-tally/race': projection => {
    for (const metric of ['both', 'launched', 'succeeded'] as const) for (const group of booleans) {
      check(projection, { ...settings, all: false, metric, group }, metric === 'both' ? 'ordered-segments' : false, amount => ({
        branchId: '1', hour, firstHour: hour, lastHour: hour, metric, available: true, unavailable: null,
        rows: [{ country: 1, tag: 'GER', launched: amount, succeeded: amount / 2, missing: null }],
        unattributed: 0, unattributedLaunched: 0, unattributedSucceeded: 0, missing: null, warnings: [],
      }));
    }
  },
  'casualties/race': projection => {
    for (const view of ['race', 'butterfly'] as const) for (const includeUnknown of booleans) for (const group of booleans) {
      check(projection, { view, includeUnknown, group }, view === 'race', amount => rank(amount, 'taken', 'unknown', 'partial'), 'rank');
    }
  },
  'equipment-produced/race': projection => {
    for (const count of ['units', 'ic'] as const) for (const measure of ['total', 'day', 'week', 'month'] as const) for (const group of booleans) {
      const options: EquipmentProducedRaceOptions = { ...settings, count, measure, group, equipmentLabel: 'Equipment', measureLabel: 'Production', noData: null };
      const reply = {
        branchId: '1', hour, firstHour: hour, lastHour: hour, count, measure, decimals: count === 'ic' ? 2 : 0, types: ['equipment'],
        rows: [], readHour: hour, observationHour: 1, missing: null, warnings: [],
      };
      check(projection, options, true, amount => ({ ...reply, rows: [{ country: 1, counted: 1, tag: 'GER', value: amount, unknown: null }] }));
      for (const kind of ['empty', 'unavailable'] as const) {
        const blocked = { ...options, noData: { kind, message: 'Equipment selection unavailable' } };
        assert.equal(projection.singleValueBars?.(blocked), true);
        const model = projection.project(createBoundedReadView({ hour }), blocked);
        assert.equal(model.emptyKind, kind);
        assert.ok(isSingleValueOrdered(model));
      }
      for (const panel of [reply, { ...reply, readHour: null }, { ...reply, missing: 'Production not recorded' }]) {
        const view = createBoundedReadView({ hour, panels: new Map([['main', panel]]) });
        assert.ok(isSingleValueOrdered(projection.project(view, options)));
      }
    }
  },
  'faction-strength/race': projection => {
    for (const aggregated of booleans) for (const kind of ['territory', 'country-factories'] as const) {
      const options = { ...factions, kind, aggregated, measure: kind === 'territory' ? 'controlledStates' : 'free7' };
      check(projection, options, true, amount => factionReply(amount, options));
    }
  },
  'territory/race': projection => {
    for (const mode of ['countries', 'factions'] as const) for (const measure of ['states', 'provinces', 'victoryPoints', 'area'] as const) for (const rule of ['controlled', 'owned', 'occupied'] as const) {
      const wire = rule + measure[0]!.toUpperCase() + measure.slice(1);
      const options = { ...factions, measure: wire };
      check(projection, { mode, measure, rule, measureLabel: measure, scaleUnit: measure, group: false, factions: options },
        true, amount => mode === 'countries' ? rank(amount, wire) : factionReply(amount, options));
    }
  },
  'occupation-trend/race': projection => {
    for (const rankBy of ['resistance', 'compliance'] as const) for (const digits of [0, 2, 5]) {
      check(projection, { ...settings, rank: rankBy, digits }, true, amount => rank(amount, rankBy));
    }
  },
  'world-tension/race': projection => { check(projection, settings, true, amount => rank(amount, 'contribution')); },
};

test('every bars projection declaring playback has fixtures for the options it accepts', async t => {
  const entries = await Promise.all(BARS_VIEWS.map(entry => entry.loadSpec()));
  const declaring = entries.filter(entry => entry.projection.singleValueBars);
  assert.deepEqual(declaring.map(entry => `${entry.kind}/${normalizeView(entry.view)}`).sort(), Object.keys(cases).sort());
  for (const { kind, view, projection } of declaring) {
    const key = `${kind}/${normalizeView(view)}`;
    await t.test(key, () => {
      assert.equal(projection.family, 'bars');
      cases[key]!(projection as BarsProjection<unknown>);
    });
  }
});
