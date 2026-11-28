import assert from 'node:assert/strict';
import test from 'node:test';
import { assertBarsModelValid, type BarsDataRow } from '../../families/bars/model.ts';
import { createBoundedReadView, displayedHour } from '../../families/boundedView.ts';
import type { IdentityProvider } from '../../families/plan.ts';
import type { ForceAtReply } from '../../generated/dashboard';
import { armySizeMixProjection } from './armySizeMixProjection.ts';
import { armySizeRaceProjection, type ArmySizeRaceOptions } from './armySizeRaceProjection.ts';

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

const BASE_OPTS: ArmySizeRaceOptions = { surface: '#1e1e1e',
  measure: 'battalions',
  countFor: 'owner',
  split: true,
  top: 10,
  all: false,
  group: false,
};

test('army-size race and mix split battalion unit groups from army-at and withhold unreconciled parts', () => {
  const reply: ForceAtReply = {
    parts: [
      { key: 'group:infantry', name: 'Infantry', unresolved: false },
      { key: 'group:armor', name: 'Armor', unresolved: false },
    ],
    ordinals: [0, 1],
    rows: [
      { country: 'GER', slot: null, tag: 'GER', total: 200, missing: null, values: [150, 50], partsMissing: null },
      { country: 'SOV', slot: null, tag: 'SOV', total: 180, missing: null, values: [null, null], partsMissing: 'Part breakdown unavailable' },
    ],
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map([['main', reply]]),
    identity: fakeIdentity({
      GER: { name: 'German Reich', color: '#6b7280' },
      SOV: { name: 'Soviet Union', color: '#dc2626' },
    }),
  });

  const race = armySizeRaceProjection.project(view, BASE_OPTS);
  assertBarsModelValid(race, 300);
  const raceRows = race.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(raceRows[0]!.segments?.map(s => [s.label, Math.round(s.share * 100)]), [['Infantry', 75], ['Armor', 25]]);
  assert.equal(raceRows[1]!.segments, undefined);
  assert.equal(raceRows[1]!.segmentsIncomplete, true);

  const mix = armySizeMixProjection.project(view, { surface: '#1e1e1e', countFor: 'owner', top: 10, all: false, group: false });
  assertBarsModelValid(mix, 300);
  assert.equal(mix.form, 'share');
  const mixRows = mix.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(mixRows[0]!.segments?.map(s => [s.label, Math.round(s.share * 100)]), [['Infantry', 75], ['Armor', 25]]);
  assert.equal(mixRows[1]!.segments?.length, 0);
  assert.equal(mixRows[1]!.value?.value.kind, 'notRecorded');
});

test('army-size race under default Top handles unpopulated panel and preserves explicit missing country', () => {
  const unpopulated = createBoundedReadView({ hour: displayedHour(300), panels: new Map() });
  const pending = armySizeRaceProjection.project(unpopulated, { ...BASE_OPTS, measure: 'manpower', split: false, top: 10 });
  assert.equal(pending.rows.length, 0);
  assert.equal(pending.empty, undefined);

  const missingExplicit = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map([['main', {
      branchId: '1',
      hour: 300,
      from: 300,
      until: 300,
      refused: 'Manpower not recorded at this hour',
      measures: [{ name: 'ownerManpower', decimals: 0 }],
      rows: [
        { country: 'GER', readings: [{ country: 'GER', hour: 300, run: { firstHour: 300, lastHour: 300, value: 1_500_000, valueRaw: '1500000' }, missing: null }] },
        { country: 'SOV', readings: [{ country: 'SOV', hour: 300, run: null, missing: 'Manpower not recorded at this hour' }] },
      ],
    }]]),
    identity: fakeIdentity({
      GER: { name: 'German Reich', color: '#6b7280' },
      SOV: { name: 'Soviet Union', color: '#dc2626' },
    }),
  });
  const model = armySizeRaceProjection.project(missingExplicit, {
    ...BASE_OPTS,
    measure: 'manpower',
    split: false,
    top: null,
  });
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(rows.map(r => [r.key, r.value?.value.kind]), [['GER', 'known'], ['SOV', 'notRecorded']]);
});
