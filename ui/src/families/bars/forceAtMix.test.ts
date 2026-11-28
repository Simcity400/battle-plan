import assert from 'node:assert/strict';
import test from 'node:test';
import { assertBarsModelValid, deriveBarsMirror, type BarsDataRow } from './model.ts';
import { createBoundedReadView, displayedHour } from '../boundedView.ts';
import type { IdentityProvider } from '../plan.ts';
import type { ForceAtReply } from '../../generated/dashboard';
import { projectForceAtMix } from './forceAtMix.ts';
import { projectForceAtReply, validateNavalLossesAtReply } from './forceAtRace.ts';
import { categoricalPalette, colour, reservedGrey } from '../../charts/palette.ts';

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

test('projectForceAtMix builds share rows with legend and marks unreconciled parts unavailable instead of folding', () => {
  const reply: ForceAtReply = {
    parts: [
      { key: 'ships', name: 'Ships', unresolved: false },
      { key: 'convoys', name: 'Convoys', unresolved: false },
    ],
    ordinals: [0, 1],
    rows: [
      { country: 'ENG', slot: null, tag: 'ENG', total: 100, missing: null, partial: 'Uncounted ship', values: [25, 75], partsPartial: ['Uncounted ship', null], partsMissing: null },
      { country: 'GER', slot: null, tag: 'GER', total: 50, missing: null, values: [30, null], partsMissing: null },
      { country: 'ITA', slot: null, tag: 'ITA', total: 40, missing: null, values: [12, 20], partsMissing: 'Part breakdown does not reconcile with the recorded total at this hour' },
      ...[0, 8].map(total => ({ country: total, slot: total, tag: String(total), total, missing: null, partial: 'Uncounted ship', values: [0, total], partsPartial: ['Uncounted ship', null], partsMissing: null })),
    ],
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({
    hour: displayedHour(300),
    panels: new Map([['main', reply]]),
    identity: fakeIdentity({
      ENG: { name: 'United Kingdom', color: '#e11d48' },
      GER: { name: 'German Reich', color: '#6b7280' },
    }),
  });
  const model = projectForceAtMix(view, reply, { surface: '#1e1e1e', top: 10, all: false, group: false }, {
    decimals: 0,
    valueFormat: { decimals: 0 },
    valueLabel: 'Ships and convoys lost',
    unit: 'Ships & convoys',
    partsLabel: 'Ships and convoys',
    emptyMessage: 'No naval losses.',
  });
  assertBarsModelValid(model, 300);
  assert.equal(model.form, 'share');
  assert.equal(model.scaleUnit, 'Ships & convoys');
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data');
  assert.deepEqual(rows[0]!.segments?.map(s => [s.label, s.share]), [['Ships', 0.25], ['Convoys', 0.75]]);
  assert.equal(rows[0]!.readout?.primary, '100 (partial)');
  assert.deepEqual(model.rows.filter(row => row.kind === 'data').map(row => row.segmentsIncomplete), [true, undefined, undefined, true, true]);
  assert.equal(deriveBarsMirror(model).rows[0]!.cells.find(cell => cell.column === 'readout')?.spoken, '100, partial: Uncounted ship');
  assert.deepEqual(rows[0]!.tooltip?.items?.map(item => item.partialReason), ['Uncounted ship', 'Uncounted ship', undefined]);
  const race = projectForceAtReply(view, reply, { surface: '#1e1e1e', top: 10, group: false, split: true }, {
    decimals: 0, valueFormat: 'integer', scaleFormat: 'integer', scaleUnit: 'Losses',
    valueLabel: 'Lost', partsLabel: 'Parts', emptyMessage: 'No losses.',
  });
  const first = race.rows.find((row): row is BarsDataRow => row.kind === 'data')!;
  assert.deepEqual(first.value?.value, { kind: 'partial', value: 100, reason: 'Uncounted ship', atLeast: true });
  assert.deepEqual(first.tooltip?.items?.map(item => item.partialReason), ['Uncounted ship', 'Uncounted ship', undefined]);
  for (const bars of [model, race]) {
    for (const row of bars.rows.slice(-2)) {
      assert.ok(row.kind === 'data');
      const ships = row.tooltip?.items?.find(item => item.label === 'Ships');
      assert.equal(ships?.partialReason, 'Uncounted ship');
      assert.match(ships.value!, /^0(?: |$)/);
      assert.ok(!row.segments?.some(segment => segment.key === 'ships'));
    }
  }
  assert.equal(rows[1]!.segments?.length, 0);
  assert.equal(rows[1]!.value?.value.kind, 'notRecorded');
  assert.deepEqual(model.legend?.map(l => l.label), ['Ships', 'Convoys']);
  assert.equal(rows[2]!.segments?.length, 0, 'parts that do not reconcile are never drawn as shares');
  assert.deepEqual(rows[2]!.tooltip?.items?.slice(1), [
    { label: 'Ships and convoys', missingReason: 'Part breakdown does not reconcile with the recorded total at this hour' },
    { label: 'Ships', value: '12' },
    { label: 'Convoys', value: '20' },
  ], 'each witnessed count is still shown');
});

test('a part takes the palette colour at its content ordinal, so a new part moves no colour; one the content does not order is neutral', () => {
  const view = createBoundedReadView({ hour: displayedHour(300), panels: new Map(), identity: fakeIdentity({}) });
  const DARK = '#1e1e1e', LIGHT = '#ffffff';
  const project = (parts: ForceAtReply['parts'], ordinals: ForceAtReply['ordinals'], values: number[], surface = DARK) => projectForceAtMix(view, {
    parts, ordinals, rows: [{ country: 'ENG', slot: null, tag: 'ENG', total: values.reduce((a, b) => a + b, 0), missing: null, values, partsMissing: null }], missing: null, warnings: [],
  }, { surface, top: 10, all: false, group: false }, { decimals: 0, valueFormat: { decimals: 0 }, valueLabel: 'Lost', unit: 'Lost', partsLabel: 'Parts (colour test)', emptyMessage: 'None.' });
  const colours = (model: ReturnType<typeof project>) => new Map(model.legend?.map(item => [item.key, item.color]));
  const ships = { key: 'ships', name: 'Ships', unresolved: false };
  const convoys = { key: 'convoys', name: 'Convoys', unresolved: false };
  const both = colours(project([ships, convoys], [0, 1], [2, 3]));
  const added = colours(project([{ key: 'aaa', name: 'New kind', unresolved: false }, ships, convoys], [2, 0, 1], [1, 2, 3]));
  assert.deepEqual([added.get('ships'), added.get('convoys')], [both.get('ships'), both.get('convoys')], 'a part appearing later moves no colour on screen');
  assert.notEqual(added.get('aaa'), added.get('ships'));
  const naval = (parts: ForceAtReply['parts']) => validateNavalLossesAtReply({ firstHour: 0, lastHour: 1, parts, rows: [], missing: null, warnings: [] }).ordinals;
  assert.deepEqual([naval([ships, convoys]), naval([convoys])], [[0, 1], [1]], 'convoys alone keep the place they hold beside ships');
  const unordered = colours(project([ships, { key: 'mystery', name: 'Mystery', unresolved: true }], [0, null], [1, 1]));
  assert.equal(unordered.get('mystery'), reservedGrey(DARK), 'a part without an ordinal is drawn in the neutral colour, never guessed');
  const past = colours(project([ships], [categoricalPalette(DARK).length], [1]));
  assert.ok(!categoricalPalette(DARK).includes(past.get('ships')!), 'a part past the palette takes its generated extension');
  const light = colours(project([ships, { key: 'mystery', name: 'Mystery', unresolved: true }], [0, null], [1, 1], LIGHT));
  assert.deepEqual([light.get('ships'), light.get('mystery')], [colour(LIGHT, 0), reservedGrey(LIGHT)], 'the colours are solved against the theme the widget is drawn on');
});

test('a total not known beside a known part keeps the known part in its tooltip (ships known, convoys not)', () => {
  const reply: ForceAtReply = {
    parts: [{ key: 'ships', name: 'Ships', unresolved: false }, { key: 'convoys', name: 'Convoys', unresolved: false }],
    ordinals: [0, 1],
    rows: [{ country: 'ENG', slot: null, tag: 'ENG', total: null, missing: 'Convoys sunk are not known at this hour', values: [3, null], partsMissing: null }],
    missing: null,
    warnings: [],
  };
  const view = createBoundedReadView({ hour: displayedHour(300), panels: new Map([['main', reply]]), identity: fakeIdentity({}) });
  const model = projectForceAtMix(view, reply, { surface: '#1e1e1e', top: null, all: false, group: false }, { decimals: 0, valueFormat: { decimals: 0 }, valueLabel: 'Lost', unit: 'Lost', partsLabel: 'Ships and convoys', emptyMessage: 'None.' });
  const row = model.rows.find((r): r is BarsDataRow => r.kind === 'data')!;
  assert.deepEqual(row.tooltip?.items?.slice(1), [{ label: 'Ships', value: '3' }, { label: 'Convoys', missingReason: 'Not recorded at this hour' }]);
});
