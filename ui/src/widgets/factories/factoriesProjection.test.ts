import assert from 'node:assert/strict';
import test from 'node:test';
import { assertBarsModelValid, deriveBarsAudit, type BarsDataRow } from '../../families/bars/model.ts';
import { createBoundedReadView, displayedHour, FutureDataError } from '../../families/boundedView.ts';
import type { IdentityProvider } from '../../families/plan.ts';
import { factoriesDeclaration, factoriesProjection, type FactoriesProjectionOptions } from './factoriesProjection.ts';

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

type Reading = number | string | null;

function reading(country: string, hour: number, value: Reading) {
  const recorded = typeof value === 'number';
  return {
    country, hour, drop: null,
    run: recorded ? { valueRaw: String(value), firstHour: hour - 10, lastHour: hour + 10, slot: 1, countryId: 1, startsSegment: false, segmentFirstHour: 0, firstExact: true, lastExact: true } : null,
    missing: recorded ? null : value ?? 'Factories not recorded',
  };
}

function panelOf(hour: number, rows: readonly (readonly [string, ...Reading[]])[], measures: readonly string[] = ['free7']) {
  return {
    branchId: '1', hour, from: hour, until: hour, refused: null,
    measures: measures.map(name => ({ name, decimals: 0 })),
    rows: rows.map(([country, ...values]) => ({ country, readings: values.map(value => reading(country, hour, value)) })),
  };
}

const BASE_OPTS: FactoriesProjectionOptions = { surface: '#1e1e1e',
  mode: 'free',
  modeLabel: 'Factories, available',
  split: false,
  parts: [
    { id: 'military', label: 'Military factories', source: 'free1', color: '#3987e5' },
    { id: 'naval', label: 'Naval dockyards', source: 'free2', color: '#d95926' },
    { id: 'civilian', label: 'Civilian factories', source: 'free4', color: '#199e70' },
  ],
  top: 10,
  group: false,
};

const SPLIT = ['free7', 'free1', 'free2', 'free4'];

function partsView(hour: number, panel: unknown) {
  return createBoundedReadView({
    hour: displayedHour(hour),
    panels: new Map(panel === undefined ? [] : [['main', panel]]),
    identity: fakeIdentity({ GER: { name: 'German Reich', color: '#6b7280' } }),
  });
}

test('factories projection preserves Rust top order, uses identity resolver, and matches exact audit', () => {
  const identity = fakeIdentity({
    USA: { name: 'United States', color: '#3b82f6' },
    GER: { name: 'German Reich', color: '#6b7280' },
    SOV: { name: 'Soviet Union', color: '#dc2626' },
  });
  const panel = panelOf(500, [['USA', 240], ['GER', 210], ['SOV', 180]]);
  const followedModel = factoriesProjection.project(createBoundedReadView({ hour: displayedHour(500), panels: new Map([['main', panel]]), identity }), BASE_OPTS);
  const exactModel = factoriesProjection.project(createBoundedReadView({ hour: displayedHour(500), panels: new Map([['main', structuredClone(panel)]]), identity }), BASE_OPTS);
  assertBarsModelValid(followedModel, 500);
  assertBarsModelValid(exactModel, 500);
  const dataRows = followedModel.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(dataRows.map(r => [r.key, r.label, r.numeric]), [
    ['USA', { kind: 'known', value: 'United States' }, 240],
    ['GER', { kind: 'known', value: 'German Reich' }, 210],
    ['SOV', { kind: 'known', value: 'Soviet Union' }, 180],
  ]);
  assert.equal(followedModel.form, 'ordered');
  assert.equal(followedModel.scaleUnit, 'Factories');
  assert.deepEqual(deriveBarsAudit(followedModel), deriveBarsAudit(exactModel));
});

test('factories projection keeps missing readings as notRecorded with null numeric and keeps the product order verbatim', () => {
  const view = createBoundedReadView({
    hour: displayedHour(200),
    panels: new Map([['main', panelOf(200, [['GER', 0], ['ENG', null]])]]),
    identity: fakeIdentity({ GER: { name: 'German Reich', color: '#6b7280' }, ENG: { name: 'United Kingdom', color: '#e11d48' } }),
  });
  const model = factoriesProjection.project(view, { ...BASE_OPTS, top: null });
  assertBarsModelValid(model, 200);
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(rows.map(row => [row.key, row.value?.value.kind, row.numeric]), [['GER', 'zero', 0], ['ENG', 'notRecorded', null]]);
});

test('a top the product refuses says why, never that no factories were recorded', () => {
  const model = factoriesProjection.project(partsView(300, { ...panelOf(300, []), refused: 'Country factories not recorded at this hour' }), BASE_OPTS);
  assert.deepEqual([model.rows.length, model.empty, model.emptyKind], [0, 'Country factories not recorded at this hour', 'unavailable']);
});

test('factories projection splits into group segments and rejects reads past displayed hour', () => {
  const view = partsView(300, panelOf(300, [['GER', 100, 60, 10, 30], ['ENG', 8, 10, 0, -2]], SPLIT));
  const model = factoriesProjection.project(view, { ...BASE_OPTS, split: true });
  assertBarsModelValid(model, 300);
  const row = model.rows[0] as BarsDataRow;
  assert.deepEqual(row.segments?.map(s => [s.key, Math.round(s.share * 100)]), [['military', 60], ['naval', 10], ['civilian', 30]]);
  const signed = model.rows[1] as BarsDataRow;
  assert.equal(signed.numeric, 8);
  assert.deepEqual(signed.segments?.map(s => s.key), ['military']);
  assert.equal(signed.tooltip?.items?.find(item => item.label === 'Civilian factories')?.value, '\u22122');
  assert.throws(() => view.who('country', 'GER', displayedHour(301)), FutureDataError);
});

test('a pinned view with split on asks every part with the total in one rank of the chosen countries', () => {
  const ctx = {
    track: { source: { id: 's', revision: 1 }, axisOffset: 0 },
    branch: { id: '1' },
    options: { split: true, count: 'free' },
    countries: { top: null, all: false, every: ['GER', 'ENG'], tags: ['ENG', 'GER'], membership: null },
    widget: { countries: { kind: 'tags', tags: ['ENG', 'GER'] } },
  } as unknown as Parameters<typeof factoriesDeclaration.sources>[0];
  const plan = factoriesDeclaration.sources(ctx, { following: false, staticSpan: null });
  assert.deepEqual(plan.entries.map(entry => entry.name), ['main']);
  const main = plan.entries[0]!.read;
  assert.equal(main.shape, 'point');
  const params = main.shape === 'point' ? main.followQuery as { top?: number; countries?: string[]; measures?: string[] } : null;
  assert.equal(params?.top, undefined, 'chosen countries are not a top');
  assert.deepEqual(params?.countries, ['ENG', 'GER'], 'the product ranks among the chosen countries');
  assert.ok(params?.measures?.length, 'every part is read in the same reply');
});

test('a missing part leaves the rest of the proven total as not recorded, and missing damage is never a damaged share', () => {
  const split = factoriesProjection.project(partsView(300, panelOf(300, [['GER', 100, 40, 'Dockyards not recorded', 0]], SPLIT)), { ...BASE_OPTS, split: true });
  const row = split.rows[0] as BarsDataRow;
  assert.deepEqual(row.segments?.map(s => [s.key, Math.round(s.share * 100), s.value.kind, s.hatched === true]), [
    ['military', 40, 'known', false],
    ['unrecorded', 60, 'notRecorded', true],
  ]);

  const built = { ...BASE_OPTS, mode: 'built' as const, damagedSource: 'damaged7' };
  const unknownDamage = factoriesProjection.project(partsView(300, panelOf(300, [['GER', 100, 'Damage not recorded']], ['built7', 'damaged7'])), built).rows[0] as BarsDataRow;
  assert.equal(unknownDamage.segments, undefined, 'the whole bar stays one known total');
  assert.equal(unknownDamage.segmentsIncomplete, true);
  assert.deepEqual(unknownDamage.tooltip?.items?.find(item => item.label === 'Damaged'), { label: 'Damaged', missingReason: 'Damage not recorded' });
  const zeroTotal = factoriesProjection.project(partsView(300, panelOf(300, [['GER', 0, 'Damage not recorded']], ['built7', 'damaged7'])), built).rows[0] as BarsDataRow;
  assert.equal(zeroTotal.segmentsIncomplete, true, 'a zero total does not prove the damage reading');
  assert.deepEqual(zeroTotal.tooltip?.items, [
    { label: built.modeLabel, value: '0' },
    { label: 'Damaged', missingReason: 'Damage not recorded' },
  ]);

  const builtSplit = {
    ...built,
    split: true,
    parts: BASE_OPTS.parts.map((part, at) => ({ ...part, damagedSource: `damaged${[1, 2, 4][at]}` })),
  };
  const measures = ['built7', 'free1', 'free2', 'free4', 'damaged1', 'damaged2', 'damaged4'];
  const partDamage = factoriesProjection.project(partsView(300, panelOf(300, [
    ['GER', 100, 60, 10, 30, 6, 'Damage not recorded', 0],
  ], measures)), builtSplit).rows[0] as BarsDataRow;
  assert.deepEqual(partDamage.segments?.map(s => [s.key, Math.round(s.share * 100)]), [
    ['military', 54], ['damaged:military', 6], ['naval', 10], ['civilian', 30],
  ]);
  assert.equal(partDamage.segmentsIncomplete, true, 'the visible parts cannot establish the missing damage share');
  const zeroGroup = factoriesProjection.project(partsView(300, panelOf(300, [
    ['GER', 100, 60, 0, 40, 6, 'Damage not recorded', 0],
  ], measures)), builtSplit).rows[0] as BarsDataRow;
  assert.equal(zeroGroup.segmentsIncomplete, true, 'a zero-width factory group can lack recorded damage');
  assert.ok(!zeroGroup.segments?.some(item => item.key.includes('naval')));
  assert.equal(zeroGroup.tooltip?.items?.find(item => item.label === 'Naval dockyards')?.value, '0');
  assert.equal(zeroGroup.tooltip?.items?.find(item => item.label === 'Damaged Naval dockyards')?.missingReason, 'Damage not recorded');
});

test('the one reply is read and indexed once per picture, not once per country and part', () => {
  const countries = Array.from({ length: 40 }, (_, at) => [`C${at}`, 100 - at, 10, 10, 10] as const);
  const view = partsView(300, panelOf(300, countries, SPLIT));
  const reads = new Map<string, number>();
  const counted = { ...view, panel: (name: string) => { reads.set(name, (reads.get(name) ?? 0) + 1); return view.panel(name); } };
  factoriesProjection.project(counted as typeof view, { ...BASE_OPTS, split: true });
  assert.deepEqual([...reads], [['main', 1]]);
});

test('a factory type keeps its colour when a type before it is deselected', () => {
  const ctx = (types: string) => ({
    track: { source: { id: 's', revision: 1 }, axisOffset: 0 },
    branch: { id: '1' },
    options: { split: true, count: 'free', types },
    countries: { top: 5, all: false, every: [], tags: [], membership: null, holders: null, unobserved: [] },
    widget: { countries: { kind: 'group', group: { type: 'top', count: 5 }, membership: 'hour', aggregate: false } },
  }) as unknown as Parameters<typeof factoriesDeclaration.projectionOptions>[0];
  const colourOf = (types: string, id: string) => factoriesDeclaration.projectionOptions(ctx(types), { extent: [0, 0], surface: '#1e1e1e' }).parts.find(part => part.id === id)!.color;
  assert.equal(colourOf('naval civilian', 'civilian'), colourOf('', 'civilian'));
  assert.equal(colourOf('civilian', 'civilian'), colourOf('', 'civilian'));
});

test('a faction is sent as the group, and a known empty group says so', () => {
  const ctx = {
    track: { source: { id: 's', revision: 1 }, axisOffset: 0 },
    branch: { id: '1' },
    options: { count: 'free' },
    countries: { top: null, all: false, every: [], tags: ['ITA'], membership: null },
    widget: { countries: { kind: 'group', group: { type: 'war', tag: 'GER' }, membership: 'ever', aggregate: false } },
  } as unknown as Parameters<typeof factoriesDeclaration.sources>[0];
  const main = factoriesDeclaration.sources(ctx, { following: false, staticSpan: null }).entries[0]!.read;
  assert.equal(main.shape, 'point');
  if (main.shape !== 'point') return;
  const asked = main.followQuery as { group?: unknown; countries?: unknown };
  assert.deepEqual([asked.group, asked.countries], [{ type: 'war', tag: 'GER', membership: 'ever' }, undefined]);
  const model = factoriesProjection.project(partsView(500, panelOf(500, [])), { ...BASE_OPTS, top: null, group: true });
  assert.deepEqual([model.emptyKind, model.empty], ['empty', 'No country of this group at this date.']);
});

test('factoriesDeclaration names split parts from recorded factory groups and marks unrecorded ones as Name not recorded', () => {
  const ctx = {
    track: { source: { id: 's', revision: 1 }, axisOffset: 0 },
    branch: { id: '1' },
    options: { split: true, count: 'built', types: '' },
    aux: {
      missing: null,
      warnings: [],
      groups: [
        { key: 'military', flag: 'military_production', buildings: [{ token: 'ordnance_works', name: 'Ordnance Works' }] },
        { key: 'naval', flag: 'naval_production', buildings: [] },
      ],
    },
    countries: { top: 5, all: false, every: [], tags: [], membership: null, holders: null, unobserved: [] },
    widget: { countries: { kind: 'group', group: { type: 'top', count: 5 }, membership: 'hour', aggregate: false } },
  } as unknown as Parameters<typeof factoriesDeclaration.projectionOptions>[0];
  const opts = factoriesDeclaration.projectionOptions(ctx, { extent: [0, 0], surface: '#1e1e1e' });
  assert.deepEqual(opts.parts.map(p => [p.id, p.label]), [
    ['military', 'Ordnance Works'],
    ['naval', 'Name not recorded'],
    ['civilian', 'Name not recorded'],
  ]);
  const model = factoriesProjection.project(
    partsView(300, panelOf(300, [['GER', 20, 10, 10, 0, 2, 3, 0]], ['built7', 'built1', 'built2', 'built4', 'damaged1', 'damaged2', 'damaged4'])),
    opts,
  );
  const row = model.rows[0] as BarsDataRow;
  assert.deepEqual(row.segments?.map(s => s.label), ['Ordnance Works', 'Damaged Ordnance Works', 'Name not recorded', 'Damaged (Name not recorded)']);
});
