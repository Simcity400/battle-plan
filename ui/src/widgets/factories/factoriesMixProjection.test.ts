import assert from 'node:assert/strict';
import test from 'node:test';
import { countryKey } from '../../dashboard/countryKey.ts';
import { createBoundedReadView, displayedHour } from '../../families/boundedView.ts';
import { assertBarsModelValid, type BarsDataRow } from '../../families/bars/model.ts';
import type { IdentityProvider } from '../../families/plan.ts';
import type { FactoryMixReply } from '../../generated/dashboard';
import { factoriesMixDeclaration, factoriesMixProjection, type FactoriesMixOptions } from './factoriesMixProjection.ts';

function fakeIdentity(names: Record<string, { name: string; color: string; tag?: string }>): IdentityProvider {
  return {
    resolve(kind, key, atHour) {
      const hit = kind === 'country' ? names[key] : undefined;
      return {
        kind,
        key,
        atHour,
        holder: { status: 'ready', value: key },
        tag: { status: 'ready', value: hit?.tag ?? key },
        name: hit ? { status: 'ready', value: hit.name } : { status: 'absent', reason: 'Name not recorded' },
        colour: hit ? { status: 'ready', value: hit.color } : { status: 'absent', reason: 'Colour not recorded' },
        flagChoice: { status: 'absent', reason: 'Flag not recorded' },
        validInterval: [0, 1000],
        nextChangeAt: Infinity,
      };
    },
  };
}

const OPTS: FactoriesMixOptions = { surface: '#1e1e1e',
  mode: 'built',
  modeLabel: 'Factories, built',
  group: false,
  top: 10,
};

test('factoriesMixProjection sends group selection to Rust, keys rows by recorded slot, uses Rust shares, keeps witnessed parts on partial refusal, and preserves zeros', () => {
  const emptyView = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map(),
    identity: fakeIdentity({}),
  });
  const emptyModel = factoriesMixProjection.project(emptyView, OPTS);
  assertBarsModelValid(emptyModel, 300);
  assert.equal(emptyModel.rows.length, 0);
  assert.equal(emptyModel.scaleUnit, 'Factories');

  const refusedReply: FactoryMixReply = {
    hour: 300,
    mode: 'built',
    parts: [],
    rows: [],
    missing: 'Membership of this group is not recorded at this hour',
    warnings: [],
  };
  const refusedModel = factoriesMixProjection.project(
    createBoundedReadView({ hour: displayedHour(300), panels: new Map([['main', refusedReply]]), identity: fakeIdentity({}) }),
    { ...OPTS, group: true, top: null },
  );
  assertBarsModelValid(refusedModel, 300);
  assert.equal(refusedModel.emptyKind, 'unavailable');
  assert.match(refusedModel.empty ?? '', /Membership of this group is not recorded/);

  const reply: FactoryMixReply = {
    hour: 300,
    mode: 'built',
    parts: [
      { key: 'military', name: 'Military Factory', damaged: false, group: 'military' },
      { key: 'civilian', name: 'Civilian Factory', damaged: false, group: 'civilian' },
      { key: 'damaged:military', name: 'Damaged Military Factory', damaged: true, group: 'military' },
      { key: 'damaged:civilian', name: 'Damaged Civilian Factory', damaged: true, group: 'civilian' },
    ],
    rows: [
      { country: 7, tag: 'GER', total: 20, values: [8, 10, 2, 0], shares: [0.4, 0.5, 0.1, 0], missing: null, partsMissing: null },
      { country: 9, tag: 'GER', total: 20, values: [null, 10, null, 0], shares: [null, 0.5, null, 0], missing: null, partsMissing: 'Damaged factories do not reconcile with built factories at this hour' },
      { country: 12, tag: 'LUX', total: 0, values: [0, 0, 0, 0], shares: [null, null, null, null], missing: null, partsMissing: null },
      { country: 14, tag: 'FRA', total: null, values: [null, null, 3, null], shares: [null, null, null, null], missing: 'A factory total does not fit a whole number', partsMissing: null },
      { country: 15, tag: 'POL', total: 0, values: [0, 0, 0, null], shares: [null, null, null, null], missing: null, partsMissing: 'Civilian damage not recorded' },
    ],
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map([['main', reply]]),
    identity: fakeIdentity({
      [countryKey(7)]: { name: 'German Reich', color: '#6b7280', tag: 'GER' },
      [countryKey(9)]: { name: 'West Germany', color: '#4b5563', tag: 'GER' },
      [countryKey(12)]: { name: 'Luxembourg', color: '#0ea5e9', tag: 'LUX' },
      [countryKey(14)]: { name: 'France', color: '#2563eb', tag: 'FRA' },
    }),
  });
  const model = factoriesMixProjection.project(view, { ...OPTS, top: null });
  assertBarsModelValid(model, 300);
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(rows.map(r => r.key), [countryKey(7), countryKey(9), countryKey(12), countryKey(14), countryKey(15)], 'rows keyed by recorded slot, never collapsed on reassigned tag');
  assert.deepEqual(rows[0]!.label, { kind: 'known', value: 'German Reich' });
  assert.deepEqual(rows[1]!.label, { kind: 'known', value: 'West Germany' });
  assert.equal(rows[0]!.segments?.length, 3);
  assert.equal(rows[0]!.segments?.[0]!.share, 0.4);
  assert.equal(rows[0]!.segments?.[2]!.hatched, true);
  assert.equal(rows[1]!.segments?.length, 1, 'witnessed civilian share is kept beside unreconciled military part');
  assert.equal(rows[1]!.segments?.[0]!.share, 0.5);
  assert.equal(rows[1]!.value?.value.kind, 'notRecorded');
  assert.equal(rows[2]!.readout?.primary, '0', 'recorded zero is preserved');
  assert.equal(rows[2]!.segmentsIncomplete, undefined);
  assert.equal(rows[4]!.segmentsIncomplete, true);
  assert.equal(rows[3]!.value?.value.kind, 'notRecorded');
  assert.ok(rows[3]!.tooltip?.items?.some(item => item.label === 'Damaged Military Factory' && item.value === '3'), 'witnessed part count kept in tooltip even when total is missing');
  assert.equal(model.scaleUnit, 'Factories');
  assert.equal(model.legend?.length, 3);

  const plan = factoriesMixDeclaration.sources({
    widget: { id: 'w', kind: 'factories', trackIds: ['t'], countries: { kind: 'group', group: { type: 'faction', id: '42' }, membership: 'hour', aggregate: false }, time: { mode: 'follow' }, options: { view: 'mix', count: 'built' } },
    track: { id: 't', source: { id: 's', revision: '1' }, branch: { id: '1', firstHour: 0, lastHour: 100 }, axisOffset: 0 },
    branch: { id: '1', firstHour: 0, lastHour: 100 },
    options: { view: 'mix', count: 'built' },
    settings: {},
    countries: { tags: [], every: [], top: null, all: false, holders: null, membership: null, unobserved: [], reason: null },
  } as never, { following: true, staticSpan: null });
  const sentParams = (plan.entries[0]!.read as { params: Record<string, unknown> }).params;
  assert.deepEqual(sentParams.group, { type: 'faction', id: '42', membership: 'hour' }, 'group selection sent to Rust');
  assert.deepEqual(sentParams.countries, []);
});

test('factoriesMixProjection matches the shared missing-name labels when part names are absent or unnamed', () => {
  const reply: FactoryMixReply = {
    hour: 300,
    mode: 'built',
    parts: [
      { key: 'military', name: null, damaged: false, group: 'military' },
      { key: 'damaged:military', name: null, damaged: true, group: 'military' },
      { key: 'civilian', name: 'Building name unavailable', damaged: false, group: 'civilian' },
      { key: 'damaged:civilian', name: 'Damaged Building name unavailable', damaged: true, group: 'civilian' },
    ],
    rows: [
      { country: 7, tag: 'AAA', total: 20, values: [8, 2, 7, 3], shares: [0.4, 0.1, 0.35, 0.15], missing: null, partsMissing: null },
    ],
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map([['main', reply]]),
    identity: fakeIdentity({ [countryKey(7)]: { name: 'Alpha Republic', color: '#6b7280', tag: 'AAA' } }),
  });
  const model = factoriesMixProjection.project(view, { ...OPTS, top: null });
  const row = model.rows[0] as BarsDataRow;
  assert.deepEqual(row.segments?.map(s => s.label), [
    'Name not recorded',
    'Damaged (Name not recorded)',
    'Building name unavailable',
    'Damaged Building name unavailable',
  ]);
  assert.deepEqual(model.legend?.map(item => item.label), [
    'Name not recorded',
    'Damaged (Name not recorded)',
    'Building name unavailable',
    'Damaged Building name unavailable',
  ]);
});
